//! Iterative resolution starting from the local root zone copy. Used when forwarders are unreachable,
//! for example during an international outage, so names served from inside the country (such as most
//! of `.bd`) still resolve. Answers are not DNSSEC-validated.

use crate::rootzone::RootManager;
use hickory_proto::op::{Edns, Message, OpCode, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use nirvor_upstream::exchange;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

const MAX_REFERRALS: usize = 16;
const MAX_CNAMES: usize = 8;
const MAX_NS_DEPTH: usize = 3;
const SERVERS_PER_ZONE: usize = 4;

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Iterative resolver.
#[derive(Clone)]
pub struct Recursor {
    root: RootManager,
    /// Zone cut (lower-cased) → nameserver addresses learned from referrals.
    zones: moka::sync::Cache<Name, Arc<Vec<SocketAddr>>>,
    timeout: Duration,
    /// Port used for nameservers learned from glue (53; tests use another).
    port: u16,
}

impl Recursor {
    /// Creates a recursor using `root` for the top of the tree. `timeout` is per server.
    pub fn new(root: RootManager, timeout: Duration) -> Self {
        Self {
            root,
            zones: moka::sync::Cache::builder()
                .max_capacity(20_000)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            timeout,
            port: 53,
        }
    }

    /// True when a verified root zone is available to start from.
    pub fn ready(&self) -> bool {
        self.root.zone().is_some()
    }

    /// Resolves one question. The reply has the answer chain (CNAMEs first) and, for negative answers, the SOA.
    pub async fn resolve(&self, q: &Query) -> Result<Message, String> {
        self.resolve_depth(q.name().clone(), q.query_type(), 0)
            .await
    }

    fn resolve_depth(
        &self,
        qname: Name,
        qtype: RecordType,
        depth: usize,
    ) -> BoxFut<'_, Result<Message, String>> {
        Box::pin(async move {
            let root = self.root.zone();
            let mut chain: Vec<Record> = Vec::new();
            let mut name = qname;
            for _ in 0..MAX_CNAMES {
                if let Some(root) = &root {
                    if name.is_root() {
                        let mut m = root.apex_answer(qtype, false).ok_or("root apex missing")?;
                        m.answers.splice(0..0, chain);
                        return Ok(m);
                    }
                    if root.tld_missing(&name) {
                        let mut m = root.nxdomain();
                        m.answers = chain;
                        return Ok(m);
                    }
                }
                let (mut zone, mut servers) = self.closest(&name)?;
                let mut next_name = None;
                for _ in 0..MAX_REFERRALS {
                    let resp = self.ask(&servers, &name, qtype).await?;
                    let rcode = resp.metadata.response_code;
                    if rcode == ResponseCode::NXDomain {
                        let mut m = Message::error_msg(0, OpCode::Query, ResponseCode::NXDomain);
                        m.answers = chain;
                        m.authorities = soa_only(&resp.authorities);
                        return Ok(m);
                    }
                    if rcode != ResponseCode::NoError {
                        return Err(format!("servers for {zone} answered {rcode}"));
                    }

                    let at_name: Vec<&Record> =
                        resp.answers.iter().filter(|r| r.name == name).collect();
                    if at_name.iter().any(|r| r.record_type() == qtype) {
                        chain.extend(
                            at_name
                                .into_iter()
                                .filter(|r| r.record_type() == qtype)
                                .cloned(),
                        );
                        let mut m = Message::response(0, OpCode::Query);
                        m.answers = chain;
                        return Ok(m);
                    }
                    if let Some(c) = at_name
                        .iter()
                        .find(|r| r.record_type() == RecordType::CNAME)
                    {
                        let RData::CNAME(target) = &c.data else { break };
                        chain.push((*c).clone());
                        let target = target.0.clone();
                        // Same response may already carry the target's records.
                        let more: Vec<Record> = resp
                            .answers
                            .iter()
                            .filter(|r| r.name == target && r.record_type() == qtype)
                            .cloned()
                            .collect();
                        if !more.is_empty() {
                            chain.extend(more);
                            let mut m = Message::response(0, OpCode::Query);
                            m.answers = chain;
                            return Ok(m);
                        }
                        next_name = Some(target);
                        break;
                    }

                    // Referral: NS for a zone between the current cut and the name.
                    let ns: Vec<&Record> = resp
                        .authorities
                        .iter()
                        .filter(|r| {
                            r.record_type() == RecordType::NS
                                && r.name != zone
                                && zone.zone_of(&r.name)
                                && r.name.zone_of(&name)
                        })
                        .collect();
                    if let Some(first) = ns.first() {
                        let new_zone = first.name.to_lowercase();
                        let targets: Vec<Name> = ns
                            .iter()
                            .filter(|r| r.name == new_zone)
                            .filter_map(|r| match &r.data {
                                RData::NS(n) => Some(n.0.clone()),
                                _ => None,
                            })
                            .collect();
                        // Only accept glue inside the zone we just asked (bailiwick rule).
                        let mut addrs: Vec<SocketAddr> = resp
                            .additionals
                            .iter()
                            .filter(|r| targets.contains(&r.name) && zone.zone_of(&r.name))
                            .filter_map(|r| match &r.data {
                                RData::A(a) => Some(SocketAddr::from((a.0, self.port))),
                                RData::AAAA(a) => Some(SocketAddr::from((a.0, self.port))),
                                _ => None,
                            })
                            .collect();
                        if addrs.is_empty() && depth < MAX_NS_DEPTH {
                            for t in targets.iter().take(3) {
                                if let Ok(m) = self
                                    .resolve_depth(t.clone(), RecordType::A, depth + 1)
                                    .await
                                {
                                    addrs.extend(m.answers.iter().filter_map(|r| match &r.data {
                                        RData::A(a) => Some(SocketAddr::from((a.0, self.port))),
                                        _ => None,
                                    }));
                                }
                                if !addrs.is_empty() {
                                    break;
                                }
                            }
                        }
                        if addrs.is_empty() {
                            return Err(format!(
                                "no reachable address for nameservers of {new_zone}"
                            ));
                        }
                        addrs.sort_by_key(|a| a.is_ipv6());
                        addrs.dedup();
                        self.zones.insert(new_zone.clone(), Arc::new(addrs.clone()));
                        zone = new_zone;
                        servers = addrs;
                        continue;
                    }

                    // NODATA needs an authoritative signal; anything else is a lame or broken server.
                    if !resp.metadata.authoritative && soa_only(&resp.authorities).is_empty() {
                        return Err(format!("unexpected answer from servers for {zone}"));
                    }
                    let mut m = Message::response(0, OpCode::Query);
                    m.answers = chain;
                    m.authorities = soa_only(&resp.authorities);
                    return Ok(m);
                }
                match next_name {
                    Some(n) => name = n,
                    None => return Err("too many referrals".into()),
                }
            }
            Err("CNAME chain too long".into())
        })
    }

    /// Closest known zone cut for `name`: learned referrals first, then the root zone's TLD delegation.
    fn closest(&self, name: &Name) -> Result<(Name, Vec<SocketAddr>), String> {
        let lower = name.to_lowercase();
        for n in (1..=lower.num_labels()).rev() {
            let cut = lower.trim_to(n as usize);
            if let Some(addrs) = self.zones.get(&cut) {
                return Ok((cut, addrs.to_vec()));
            }
        }
        let root = self.root.zone().ok_or("no verified root zone")?;
        let (tld, _, addrs) = root.delegation(name).ok_or("no TLD delegation")?;
        if addrs.is_empty() {
            return Err(format!("no glue for {tld}"));
        }
        self.zones.insert(tld.clone(), Arc::new(addrs.clone()));
        Ok((tld, addrs))
    }

    /// Asks the zone's servers (no recursion) until one gives a usable answer.
    async fn ask(
        &self,
        servers: &[SocketAddr],
        name: &Name,
        qtype: RecordType,
    ) -> Result<Message, String> {
        let mut q = Message::query();
        q.metadata.recursion_desired = false;
        q.add_query(Query::query(name.clone(), qtype));
        let mut e = Edns::new();
        e.set_max_payload(hickory_proto::op::DEFAULT_MAX_PAYLOAD_LEN);
        q.set_edns(e);
        let mut last = "no servers".to_string();
        for &s in servers.iter().take(SERVERS_PER_ZONE) {
            match exchange(s, &q, self.timeout).await {
                Ok(m)
                    if !matches!(
                        m.metadata.response_code,
                        ResponseCode::ServFail | ResponseCode::Refused
                    ) =>
                {
                    return Ok(m);
                }
                Ok(m) => last = format!("{s} answered {}", m.metadata.response_code),
                Err(e) => {
                    debug!(server = %s, error = %e, "authoritative server failed");
                    last = format!("{s}: {e}");
                }
            }
        }
        Err(last)
    }
}

fn soa_only(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .filter(|r| r.record_type() == RecordType::SOA)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::rdata::{A, CNAME, NS, SOA};
    use std::net::Ipv4Addr;
    use tokio::net::UdpSocket;

    fn n(s: &str) -> Name {
        Name::from_ascii(s).unwrap()
    }

    fn soa(zone: &str) -> Record {
        Record::from_rdata(
            n(zone),
            300,
            RData::SOA(SOA::new(
                n("ns.test."),
                n("admin.test."),
                1,
                3600,
                600,
                86400,
                60,
            )),
        )
    }

    /// `test.` server: refers example.test to 127.0.0.2 (with glue), plus an out-of-bailiwick
    /// glue record that must be ignored; `alias.test` is a CNAME into example.test.
    fn tld_server(q: &Query) -> Message {
        let mut m = Message::response(0, OpCode::Query);
        let name = q.name().to_ascii();
        if name.ends_with("example.test.") {
            m.add_authority(Record::from_rdata(
                n("example.test."),
                300,
                RData::NS(NS(n("ns.example.test."))),
            ));
            m.add_additional(Record::from_rdata(
                n("ns.example.test."),
                300,
                RData::A(A(Ipv4Addr::new(127, 0, 0, 2))),
            ));
            m.add_additional(Record::from_rdata(
                n("ns.example.test."),
                300,
                RData::A(A(Ipv4Addr::new(127, 0, 0, 2))),
            ));
            m.add_additional(Record::from_rdata(
                n("evil.other."),
                300,
                RData::A(A(Ipv4Addr::new(6, 6, 6, 6))),
            ));
        } else if name == "alias.test." {
            m.metadata.authoritative = true;
            m.add_answer(Record::from_rdata(
                n("alias.test."),
                300,
                RData::CNAME(CNAME(n("www.example.test."))),
            ));
        } else {
            m.metadata.authoritative = true;
            m.metadata.response_code = ResponseCode::NXDomain;
            m.add_authority(soa("test."));
        }
        m
    }

    fn example_server(q: &Query) -> Message {
        let mut m = Message::response(0, OpCode::Query);
        m.metadata.authoritative = true;
        if q.name().to_ascii() == "www.example.test." && q.query_type() == RecordType::A {
            m.add_answer(Record::from_rdata(
                n("www.example.test."),
                300,
                RData::A(A(Ipv4Addr::new(192, 0, 2, 7))),
            ));
        } else {
            m.add_authority(soa("example.test."));
        }
        m
    }

    async fn serve(ip: [u8; 4], port: u16, f: fn(&Query) -> Message) -> u16 {
        let sock = UdpSocket::bind(SocketAddr::from((ip, port))).await.unwrap();
        let port = sock.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let (len, peer) = sock.recv_from(&mut buf).await.unwrap();
                let q = Message::from_vec(&buf[..len]).unwrap();
                assert!(
                    !q.metadata.recursion_desired,
                    "iterative queries must not set RD"
                );
                let mut r = f(&q.queries[0]);
                r.metadata.id = q.metadata.id;
                r.add_query(q.queries[0].clone());
                sock.send_to(&r.to_vec().unwrap(), peer).await.unwrap();
            }
        });
        port
    }

    async fn recursor() -> Recursor {
        let port = serve([127, 0, 0, 1], 0, tld_server).await;
        serve([127, 0, 0, 2], port, example_server).await;
        let dir = std::env::temp_dir().join("nirvor-no-root");
        let mut r = Recursor::new(
            RootManager::new(
                crate::RootZoneConfig {
                    enabled: false,
                    ..Default::default()
                },
                &dir,
            ),
            Duration::from_millis(500),
        );
        r.port = port;
        r.zones.insert(
            n("test."),
            Arc::new(vec![SocketAddr::from(([127, 0, 0, 1], port))]),
        );
        r
    }

    #[tokio::test]
    async fn follows_referral_with_glue() {
        let r = recursor().await;
        let m = r
            .resolve(&Query::query(n("www.example.test."), RecordType::A))
            .await
            .unwrap();
        assert_eq!(m.answers.len(), 1);
        assert_eq!(m.answers[0].data, RData::A(A(Ipv4Addr::new(192, 0, 2, 7))));
        let learned = r.zones.get(&n("example.test.")).unwrap();
        assert_eq!(
            *learned,
            vec![SocketAddr::from(([127, 0, 0, 2], r.port))],
            "out-of-bailiwick glue ignored, duplicates removed"
        );
    }

    #[tokio::test]
    async fn chases_cname_across_zones() {
        let r = recursor().await;
        let m = r
            .resolve(&Query::query(n("alias.test."), RecordType::A))
            .await
            .unwrap();
        assert_eq!(m.answers.len(), 2);
        assert_eq!(m.answers[0].record_type(), RecordType::CNAME);
        assert_eq!(m.answers[1].record_type(), RecordType::A);
    }

    #[tokio::test]
    async fn negative_answers_keep_soa() {
        let r = recursor().await;
        let nx = r
            .resolve(&Query::query(n("missing.test."), RecordType::A))
            .await
            .unwrap();
        assert_eq!(nx.metadata.response_code, ResponseCode::NXDomain);
        assert_eq!(nx.authorities.len(), 1);
        let nodata = r
            .resolve(&Query::query(n("www.example.test."), RecordType::AAAA))
            .await
            .unwrap();
        assert_eq!(nodata.metadata.response_code, ResponseCode::NoError);
        assert!(nodata.answers.is_empty());
        assert_eq!(nodata.authorities[0].record_type(), RecordType::SOA);
    }
}
