//! Small static zones from the config, answered authoritatively (ISP-internal names, overrides).
//!
//! Records use zone-file syntax without class or TTL, relative to the zone name:
//! `"www A 10.0.0.10"`, `"@ MX 10 mail"`, `"portal CNAME www"`.

use hickory_proto::op::{Message, OpCode, ResponseCode};
use hickory_proto::rr::rdata::SOA;
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_proto::serialize::txt::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One zone from the config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneConfig {
    /// Zone name, for example `isp.example.bd`.
    pub name: String,
    /// TTL for every record and for negative answers.
    #[serde(default = "d_ttl")]
    pub ttl: u32,
    /// Records, one per entry.
    #[serde(default)]
    pub records: Vec<String>,
}

fn d_ttl() -> u32 {
    300
}

struct Zone {
    origin: Name,
    soa: Record,
    /// Lower-cased owner → records.
    names: HashMap<Name, Vec<Record>>,
}

/// All configured zones.
#[derive(Default)]
pub struct LocalZones {
    zones: Vec<Zone>,
}

impl LocalZones {
    /// Parses every zone; the error names the zone that failed.
    pub fn new(configs: &[ZoneConfig]) -> Result<Self, String> {
        let mut zones = Vec::new();
        for c in configs {
            zones.push(parse_zone(c).map_err(|e| format!("zone {}: {e}", c.name))?);
        }
        // Longest origin first, so the most specific zone wins.
        zones.sort_by_key(|z| std::cmp::Reverse(z.origin.num_labels()));
        Ok(Self { zones })
    }

    /// True when no zones are configured.
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    /// Authoritative answer if `qname` is inside a local zone, else `None`.
    pub fn lookup(&self, qname: &Name, qtype: RecordType) -> Option<Message> {
        let zone = self.zones.iter().find(|z| z.origin.zone_of(qname))?;
        let mut m = Message::response(0, OpCode::Query);
        m.metadata.authoritative = true;
        let mut name = qname.to_lowercase();
        for _ in 0..8 {
            let Some(records) = zone.names.get(&name) else {
                // An empty non-terminal (a name with records below it) exists: NODATA, not NXDOMAIN.
                let exists = zone.names.keys().any(|k| name.zone_of(k));
                if m.answers.is_empty() && !exists {
                    m.metadata.response_code = ResponseCode::NXDomain;
                }
                if m.answers.is_empty() {
                    m.add_authority(zone.soa.clone());
                }
                return Some(m);
            };
            let matching: Vec<Record> = records
                .iter()
                .filter(|r| r.record_type() == qtype || qtype == RecordType::ANY)
                .cloned()
                .collect();
            if !matching.is_empty() {
                m.add_answers(matching);
                return Some(m);
            }
            let Some(cname) = records
                .iter()
                .find(|r| r.record_type() == RecordType::CNAME)
            else {
                if m.answers.is_empty() {
                    m.add_authority(zone.soa.clone()); // NODATA
                }
                return Some(m);
            };
            m.add_answer(cname.clone());
            let RData::CNAME(target) = &cname.data else {
                return Some(m);
            };
            if !zone.origin.zone_of(&target.0) {
                return Some(m); // target outside the zone: the client follows it
            }
            name = target.0.to_lowercase();
        }
        Some(m)
    }
}

fn parse_zone(c: &ZoneConfig) -> Result<Zone, String> {
    let origin = Name::from_ascii(&c.name)
        .map_err(|e| e.to_string())?
        .to_lowercase();
    let mut origin = origin;
    origin.set_fqdn(true);
    let soa_rdata = SOA::new(
        Name::from_ascii("ns")
            .and_then(|n| n.append_domain(&origin))
            .map_err(|e| e.to_string())?,
        Name::from_ascii("hostmaster")
            .and_then(|n| n.append_domain(&origin))
            .map_err(|e| e.to_string())?,
        1,
        3600,
        600,
        86_400,
        c.ttl,
    );
    let soa = Record::from_rdata(origin.clone(), c.ttl, RData::SOA(soa_rdata));
    let mut text = format!("$TTL {}\n", c.ttl);
    for line in &c.records {
        if line.contains('\n') || line.contains('$') {
            return Err(format!(
                "record must be a single line without directives: {line}"
            ));
        }
        // Insert the class after the owner so every line is complete on its own.
        let mut parts = line.splitn(2, char::is_whitespace);
        let owner = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default().trim();
        text.push_str(&format!("{owner} IN {rest}\n"));
    }
    let (_, sets) = Parser::new(text, None, Some(origin.clone()))
        .parse()
        .map_err(|e| e.to_string())?;
    let mut names: HashMap<Name, Vec<Record>> = HashMap::new();
    for (key, set) in sets {
        let owner: Name = key.name.into();
        if !origin.zone_of(&owner) {
            return Err(format!("{owner} is outside the zone"));
        }
        names
            .entry(owner.to_lowercase())
            .or_default()
            .extend(set.records_without_rrsigs().cloned());
    }
    for (owner, recs) in &names {
        if recs.iter().any(|r| r.record_type() == RecordType::CNAME) && recs.len() > 1 {
            return Err(format!(
                "{owner} has a CNAME and other records (RFC 1034 §3.6.2)"
            ));
        }
    }
    Ok(Zone { origin, soa, names })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::rdata::A;
    use std::net::Ipv4Addr;

    fn zones() -> LocalZones {
        LocalZones::new(&[
            ZoneConfig {
                name: "isp.example".into(),
                ttl: 120,
                records: vec![
                    "@ A 10.0.0.1".into(),
                    "www A 10.0.0.10".into(),
                    "portal CNAME www".into(),
                    "outside CNAME example.com.".into(),
                    "@ MX 10 mail".into(),
                    "mail A 10.0.0.20".into(),
                    "host.lab A 10.0.1.1".into(),
                ],
            },
            ZoneConfig {
                name: "deep.isp.example".into(),
                ttl: 60,
                records: vec!["x A 10.9.9.9".into()],
            },
        ])
        .unwrap()
    }

    fn n(s: &str) -> Name {
        Name::from_ascii(s).unwrap()
    }

    #[test]
    fn answers_and_case_insensitive() {
        let m = zones()
            .lookup(&n("WWW.isp.example."), RecordType::A)
            .unwrap();
        assert!(m.metadata.authoritative);
        assert_eq!(m.answers[0].data, RData::A(A(Ipv4Addr::new(10, 0, 0, 10))));
        assert_eq!(m.answers[0].ttl, 120);
    }

    #[test]
    fn follows_in_zone_cname() {
        let m = zones()
            .lookup(&n("portal.isp.example."), RecordType::A)
            .unwrap();
        assert_eq!(m.answers.len(), 2);
        let out = zones()
            .lookup(&n("outside.isp.example."), RecordType::A)
            .unwrap();
        assert_eq!(
            out.answers.len(),
            1,
            "out-of-zone target is left to the client"
        );
    }

    #[test]
    fn nxdomain_nodata_and_empty_non_terminal() {
        let z = zones();
        let nx = z.lookup(&n("nope.isp.example."), RecordType::A).unwrap();
        assert_eq!(nx.metadata.response_code, ResponseCode::NXDomain);
        assert_eq!(nx.authorities[0].record_type(), RecordType::SOA);
        let nodata = z.lookup(&n("www.isp.example."), RecordType::AAAA).unwrap();
        assert_eq!(nodata.metadata.response_code, ResponseCode::NoError);
        assert!(nodata.answers.is_empty());
        let ent = z.lookup(&n("lab.isp.example."), RecordType::A).unwrap();
        assert_eq!(
            ent.metadata.response_code,
            ResponseCode::NoError,
            "lab exists because host.lab does"
        );
    }

    #[test]
    fn most_specific_zone_wins_and_outside_is_none() {
        let z = zones();
        let m = z.lookup(&n("x.deep.isp.example."), RecordType::A).unwrap();
        assert_eq!(m.answers[0].ttl, 60);
        assert!(z.lookup(&n("example.com."), RecordType::A).is_none());
    }

    #[test]
    fn rejects_bad_config() {
        let bad = |records: Vec<&str>| {
            LocalZones::new(&[ZoneConfig {
                name: "z.example".into(),
                ttl: 60,
                records: records.into_iter().map(String::from).collect(),
            }])
        };
        assert!(bad(vec!["www A not-an-ip"]).is_err());
        assert!(bad(vec!["www CNAME a", "www A 10.0.0.1"]).is_err());
        assert!(bad(vec!["other.example. A 10.0.0.1"]).is_err());
        assert!(bad(vec!["$INCLUDE /etc/passwd"]).is_err());
    }
}
