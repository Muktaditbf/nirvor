//! Phase 1 integration tests: a real nirvor server in front of a fake upstream on localhost.

use hickory_proto::op::{Edns, Message, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, SOA, TXT};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use nirvor_core::Server;
use nirvor_core::config::{Config, Limits, ServerConfig};
use nirvor_core::handler::Resolver;
use nirvor_core::mode::{Control, Mode};
use nirvor_upstream::{PoolConfig, UpstreamSpec};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

/// Each test server gets its own data directory (SQLite file, snapshot).
static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const LOCAL: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// Fake upstream. `a.test` A → 192.0.2.1; `big.test` TXT → ~3 KB (UDP always truncated); `slow.test` → never answers.
fn answer(query: &Message, over_udp: bool) -> Option<Message> {
    let q = query.queries.first()?;
    let name = q.name().to_ascii();
    if name == "slow.test." {
        return None;
    }
    let mut reply = Message::response(query.metadata.id, OpCode::Query);
    reply.metadata.recursion_desired = query.metadata.recursion_desired;
    reply.metadata.recursion_available = true;
    reply.add_query(q.clone());
    if let Some(e) = &query.edns {
        let mut edns = Edns::new();
        edns.set_max_payload(e.max_payload());
        reply.set_edns(edns);
    }
    match name.as_str() {
        "a.test." | "delay.test." => {
            reply.add_answer(Record::from_rdata(
                q.name().clone(),
                300,
                RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
            ));
        }
        "big.test." if over_udp => reply.metadata.truncation = true,
        "big.test." => {
            for _ in 0..15 {
                reply.add_answer(Record::from_rdata(
                    q.name().clone(),
                    300,
                    RData::TXT(TXT::new(vec!["x".repeat(200)])),
                ));
            }
        }
        _ => {
            reply.metadata.response_code = ResponseCode::NXDomain;
            let soa = SOA::new(
                Name::from_ascii("ns.test.").unwrap(),
                Name::from_ascii("admin.test.").unwrap(),
                1,
                3600,
                600,
                86400,
                120,
            );
            reply.add_authority(Record::from_rdata(
                Name::from_ascii("test.").unwrap(),
                300,
                RData::SOA(soa),
            ));
        }
    }
    Some(reply)
}

/// Starts the fake upstream on UDP and TCP (same port) and returns its address.
async fn fake_upstream() -> SocketAddr {
    fake_upstream_counted().await.0
}

/// Like `fake_upstream`, also returning a count of UDP queries it received.
async fn fake_upstream_counted() -> (SocketAddr, Arc<AtomicUsize>) {
    let (addr, count, _) = fake_upstream_switch().await;
    (addr, count)
}

/// Like `fake_upstream_counted`, plus a switch: while it is `true` the upstream ignores every query.
async fn fake_upstream_switch() -> (SocketAddr, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let down = Arc::new(AtomicBool::new(false));
    let udp_down = down.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let udp_count = count.clone();
    let udp = Arc::new(UdpSocket::bind(LOCAL).await.unwrap());
    let addr = udp.local_addr().unwrap();
    let tcp = TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_535];
        loop {
            let (n, peer) = udp.recv_from(&mut buf).await.unwrap();
            let q = Message::from_vec(&buf[..n]).unwrap();
            if !q.queries[0].name().is_root() {
                udp_count.fetch_add(1, Ordering::SeqCst); // health probes ask for "." and are not counted
            }
            if udp_down.load(Ordering::SeqCst) {
                continue;
            }
            let udp = udp.clone();
            tokio::spawn(async move {
                if q.queries[0].name().to_ascii().starts_with("delay.") {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                if let Some(r) = answer(&q, true) {
                    udp.send_to(&r.to_vec().unwrap(), peer).await.unwrap();
                }
            });
        }
    });
    tokio::spawn(async move {
        loop {
            let (mut s, _) = tcp.accept().await.unwrap();
            tokio::spawn(async move {
                let n = usize::from(s.read_u16().await.unwrap());
                let mut buf = vec![0u8; n];
                s.read_exact(&mut buf).await.unwrap();
                let q = Message::from_vec(&buf).unwrap();
                if let Some(r) = answer(&q, false) {
                    let out = r.to_vec().unwrap();
                    s.write_u16(out.len() as u16).await.unwrap();
                    s.write_all(&out).await.unwrap();
                }
            });
        }
    });
    (addr, count, down)
}

/// A free UDP port with nothing listening, to act as a dead upstream.
async fn dead_upstream() -> SocketAddr {
    UdpSocket::bind(LOCAL).await.unwrap().local_addr().unwrap()
}

async fn start(upstreams: Vec<SocketAddr>, timeout_ms: u64) -> SocketAddr {
    start_with(upstreams, timeout_ms, 4096).await
}

async fn start_with(
    upstreams: Vec<SocketAddr>,
    timeout_ms: u64,
    max_udp_payload: u16,
) -> SocketAddr {
    start_cfg(upstreams, |c| {
        c.upstream.timeout_ms = timeout_ms;
        c.server.max_udp_payload = max_udp_payload;
    })
    .await
}

fn test_config(upstreams: Vec<SocketAddr>) -> Config {
    Config {
        server: ServerConfig {
            listen: vec![LOCAL],
            max_udp_payload: 4096,
            udp_sockets: Some(2),
        },
        upstream: PoolConfig {
            servers: upstreams
                .into_iter()
                .enumerate()
                .map(|(i, addr)| UpstreamSpec {
                    name: format!("up{i}"),
                    addr,
                    international: true,
                    enabled: true,
                })
                .collect(),
            timeout_ms: 1000,
            min_timeout_ms: 100,
            max_attempts: 3,
            health_interval_secs: 60,
            failure_threshold: 3,
            probe_name: ".".into(),
        },
        limits: Limits::default(),
        cache: Default::default(),
        api: Default::default(),
        zones: vec![],
        resilience: nirvor_core::config::ResilienceConfig {
            data_dir: std::env::temp_dir().join(format!(
                "nirvor-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::SeqCst)
            )),
            root_zone: nirvor_resilience::RootZoneConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
        isolation: Default::default(),
        policy: Default::default(),
        logging: Default::default(),
    }
}

async fn start_cfg(upstreams: Vec<SocketAddr>, tweak: impl FnOnce(&mut Config)) -> SocketAddr {
    start_full(upstreams, tweak).await.0
}

async fn start_full(
    upstreams: Vec<SocketAddr>,
    tweak: impl FnOnce(&mut Config),
) -> (SocketAddr, Arc<Resolver>) {
    let mut config = test_config(upstreams);
    tweak(&mut config);
    config.validate().unwrap();
    let server = Server::bind(&config).await.unwrap();
    let addr = server.local_addrs().unwrap()[0];
    let resolver = server.resolver();
    tokio::spawn(server.run());
    (addr, resolver)
}

fn query(name: &str, rtype: RecordType, edns_size: Option<u16>) -> Message {
    let mut m = Message::query();
    m.metadata.recursion_desired = true;
    m.add_query(Query::query(Name::from_ascii(name).unwrap(), rtype));
    if let Some(size) = edns_size {
        let mut e = Edns::new();
        e.set_max_payload(size);
        m.set_edns(e);
    }
    m
}

async fn udp(server: SocketAddr, packet: &[u8]) -> Option<Vec<u8>> {
    udp_wait(server, packet, 3000).await
}

async fn udp_wait(server: SocketAddr, packet: &[u8], ms: u64) -> Option<Vec<u8>> {
    let s = UdpSocket::bind(LOCAL).await.unwrap();
    s.send_to(packet, server).await.unwrap();
    let mut buf = vec![0u8; 65_535];
    let n = timeout(Duration::from_millis(ms), s.recv(&mut buf))
        .await
        .ok()?
        .unwrap();
    Some(buf[..n].to_vec())
}

async fn tcp(server: SocketAddr, packet: &[u8]) -> Vec<u8> {
    let mut s = TcpStream::connect(server).await.unwrap();
    s.write_u16(packet.len() as u16).await.unwrap();
    s.write_all(packet).await.unwrap();
    let n = usize::from(s.read_u16().await.unwrap());
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).await.unwrap();
    buf
}

async fn ask_udp(server: SocketAddr, q: &Message) -> Message {
    let raw = udp(server, &q.to_vec().unwrap())
        .await
        .expect("no UDP reply");
    let r = Message::from_vec(&raw).unwrap();
    assert_eq!(
        r.metadata.id, q.metadata.id,
        "reply must carry the client's ID"
    );
    r
}

#[tokio::test]
async fn forwards_over_udp() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let r = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.metadata.response_code, ResponseCode::NoError);
    assert_eq!(r.answers.len(), 1);
    assert_eq!(r.answers[0].data, RData::A(A(Ipv4Addr::new(192, 0, 2, 1))));
}

#[tokio::test]
async fn forwards_over_tcp() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let q = query("a.test.", RecordType::A, None);
    let r = Message::from_vec(&tcp(server, &q.to_vec().unwrap()).await).unwrap();
    assert_eq!(r.metadata.id, q.metadata.id);
    assert_eq!(r.answers.len(), 1);
}

#[tokio::test]
async fn big_answer_truncated_for_plain_udp_client() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let r = ask_udp(server, &query("big.test.", RecordType::TXT, None)).await;
    assert!(r.metadata.truncation, "512-byte client must get TC=1");
    assert!(r.answers.is_empty());
}

#[tokio::test]
async fn upstream_truncation_falls_back_to_tcp_for_edns_client() {
    // The fake upstream always truncates big.test over UDP, so a full answer proves the TCP fallback.
    let server = start(vec![fake_upstream().await], 1000).await;
    let r = ask_udp(server, &query("big.test.", RecordType::TXT, Some(4096))).await;
    assert!(!r.metadata.truncation);
    assert_eq!(r.answers.len(), 15);
    assert!(
        r.edns.is_some(),
        "EDNS query must get an OPT record back (RFC 6891 §6.1.1)"
    );
}

#[tokio::test]
async fn big_answer_full_over_tcp() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let q = query("big.test.", RecordType::TXT, None);
    let r = Message::from_vec(&tcp(server, &q.to_vec().unwrap()).await).unwrap();
    assert_eq!(r.answers.len(), 15);
}

#[tokio::test]
async fn malformed_query_gets_formerr() {
    let server = start(vec![fake_upstream().await], 1000).await;
    // Valid header claiming one question, followed by junk.
    let packet = [
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff,
    ];
    let r = Message::from_vec(&udp(server, &packet).await.expect("reply")).unwrap();
    assert_eq!(r.metadata.id, 0x1234);
    assert_eq!(r.metadata.response_code, ResponseCode::FormErr);
}

#[tokio::test]
async fn short_packet_and_responses_are_dropped() {
    let server = start(vec![fake_upstream().await], 1000).await;
    assert!(udp(server, &[1, 2, 3]).await.is_none());
    let mut resp = query("a.test.", RecordType::A, None).to_vec().unwrap();
    resp[2] |= 0x80; // QR=1
    assert!(udp(server, &resp).await.is_none());
    // Server still alive afterwards.
    ask_udp(server, &query("a.test.", RecordType::A, None)).await;
}

#[tokio::test]
async fn non_query_opcode_gets_notimp() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let mut q = query("a.test.", RecordType::A, None);
    q.metadata.op_code = OpCode::Status;
    let r = ask_udp(server, &q).await;
    assert_eq!(r.metadata.response_code, ResponseCode::NotImp);
}

#[tokio::test]
async fn fails_over_to_next_upstream() {
    let server = start(vec![dead_upstream().await, fake_upstream().await], 300).await;
    let r = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.answers.len(), 1);
}

#[tokio::test]
async fn servfail_when_all_upstreams_fail() {
    let server = start(vec![fake_upstream().await], 200).await;
    let r = ask_udp(server, &query("slow.test.", RecordType::A, Some(1232))).await;
    assert_eq!(r.metadata.response_code, ResponseCode::ServFail);
    assert_eq!(r.queries.len(), 1, "error replies keep the question");
}

#[tokio::test]
async fn second_query_comes_from_cache() {
    let (up, count) = fake_upstream_counted().await;
    let server = start(vec![up], 1000).await;
    let first = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    let second = ask_udp(server, &query("A.Test.", RecordType::A, None)).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "second query must not reach the upstream"
    );
    assert_eq!(first.answers, second.answers);
    assert_eq!(
        second.queries[0].name().to_ascii(),
        "A.Test.",
        "reply echoes the client's spelling"
    );
}

#[tokio::test]
async fn nxdomain_is_cached_with_soa() {
    let (up, count) = fake_upstream_counted().await;
    let server = start(vec![up], 1000).await;
    for _ in 0..3 {
        let r = ask_udp(server, &query("missing.test.", RecordType::A, None)).await;
        assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
        assert_eq!(r.authorities.len(), 1);
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn edns_reply_capped_at_configured_payload() {
    // Client offers 4096 but the server sends at most 1232 over UDP (DNS Flag Day 2020).
    let server = start_with(vec![fake_upstream().await], 1000, 1232).await;
    let r = ask_udp(server, &query("big.test.", RecordType::TXT, Some(4096))).await;
    assert!(r.metadata.truncation);
    assert_eq!(r.edns.map(|e| e.max_payload()), Some(1232));
}

#[tokio::test]
async fn no_opt_in_reply_to_non_edns_query() {
    let server = start(vec![fake_upstream().await], 1000).await;
    let r = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert!(
        r.edns.is_none(),
        "RFC 6891 §7: no OPT in reply when the query had none"
    );
}

#[tokio::test]
async fn concurrent_misses_share_one_upstream_query() {
    let (up, count) = fake_upstream_counted().await;
    let server = start(vec![up], 2000).await;
    let asks = (0..20)
        .map(|_| async move { ask_udp(server, &query("delay.test.", RecordType::A, None)).await });
    let replies = futures_join_all(asks).await;
    assert!(replies.iter().all(|r| r.answers.len() == 1));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "20 identical misses, one upstream query"
    );
}

async fn futures_join_all<F: std::future::Future + Send + 'static>(
    fs: impl Iterator<Item = F>,
) -> Vec<F::Output>
where
    F::Output: Send,
{
    let handles: Vec<_> = fs.map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test]
async fn serves_stale_when_upstream_is_down() {
    let (up, _, down) = fake_upstream_switch().await;
    let server = start_cfg(vec![up], |c| c.cache.max_ttl = 1).await;
    let fresh = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(fresh.answers[0].ttl, 1);
    down.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let stale = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(stale.metadata.response_code, ResponseCode::NoError);
    assert_eq!(stale.answers.len(), 1);
    assert_eq!(
        stale.answers[0].ttl, 30,
        "RFC 8767: stale answers carry a 30 s TTL"
    );
    // Failure recheck: the next query is answered stale at once, without waiting for the upstream.
    let t = std::time::Instant::now();
    ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert!(
        t.elapsed() < Duration::from_millis(50),
        "took {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn slow_refresh_answers_stale_then_updates_cache() {
    let (up, count) = fake_upstream_counted().await;
    let server = start_cfg(vec![up], |c| {
        c.cache.max_ttl = 1;
        c.cache.client_response_timer_ms = 50;
    })
    .await;
    ask_udp(server, &query("delay.test.", RecordType::A, None)).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let t = std::time::Instant::now();
    let r = ask_udp(server, &query("delay.test.", RecordType::A, None)).await;
    assert!(
        t.elapsed() < Duration::from_millis(250),
        "stale answer should come after ~50 ms, took {:?}",
        t.elapsed()
    );
    assert_eq!(r.answers[0].ttl, 30);
    tokio::time::sleep(Duration::from_millis(400)).await; // background refresh lands
    let r = ask_udp(server, &query("delay.test.", RecordType::A, None)).await;
    assert_eq!(
        r.answers[0].ttl, 1,
        "fresh again after the background refresh"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn servfail_is_remembered_briefly() {
    let (up, count) = fake_upstream_counted().await;
    let server = start_cfg(vec![up], |c| c.upstream.timeout_ms = 200).await;
    for _ in 0..3 {
        let r = ask_udp(server, &query("slow.test.", RecordType::A, None)).await;
        assert_eq!(r.metadata.response_code, ResponseCode::ServFail);
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "later queries answered from the failure cache"
    );
}

#[tokio::test]
async fn local_zone_answers_without_upstream() {
    let (up, count) = fake_upstream_counted().await;
    let server = start_cfg(vec![up], |c| {
        c.zones = vec![nirvor_zones::ZoneConfig {
            name: "isp.example".into(),
            ttl: 60,
            records: vec!["portal A 10.1.2.3".into()],
        }]
    })
    .await;
    let r = ask_udp(server, &query("portal.isp.example.", RecordType::A, None)).await;
    assert!(r.metadata.authoritative);
    assert_eq!(r.answers[0].data, RData::A(A(Ipv4Addr::new(10, 1, 2, 3))));
    let nx = ask_udp(server, &query("other.isp.example.", RecordType::A, None)).await;
    assert_eq!(nx.metadata.response_code, ResponseCode::NXDomain);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pinned_domains_are_warmed_at_startup() {
    let (up, count) = fake_upstream_counted().await;
    let server = start_cfg(vec![up], |c| {
        c.resilience.pinned_domains = vec!["a.test".into()]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = count.load(Ordering::SeqCst);
    assert!(before >= 1, "pinned refresh should have queried upstream");
    let r = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.answers.len(), 1);
    assert_eq!(
        count.load(Ordering::SeqCst),
        before,
        "answered from the warmed cache"
    );
}

#[tokio::test]
async fn forced_isolation_uses_local_upstreams_and_overrides() {
    let (intl, intl_count) = fake_upstream_counted().await;
    let (local, local_count) = fake_upstream_counted().await;
    let (server, resolver) = start_full(vec![intl, local], |c| {
        c.upstream.servers[1].international = false;
        c.isolation.overrides = vec![nirvor_zones::ZoneConfig {
            name: "connectivitycheck.gstatic.com".into(),
            ttl: 30,
            records: vec!["@ A 10.0.0.53".into()],
        }];
    })
    .await;
    // Not isolated: the override is not used.
    let normal = ask_udp(
        server,
        &query("connectivitycheck.gstatic.com.", RecordType::A, None),
    )
    .await;
    assert_ne!(
        normal.answers.first().map(|a| a.data.clone()),
        Some(RData::A(A(Ipv4Addr::new(10, 0, 0, 53))))
    );

    resolver.set_control(Control::Isolation, "admin").await;
    assert_eq!(resolver.mode_state().mode, Mode::Isolation);
    let before_intl = intl_count.load(Ordering::SeqCst);
    let r = ask_udp(server, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.answers.len(), 1);
    assert_eq!(
        intl_count.load(Ordering::SeqCst),
        before_intl,
        "international upstream skipped"
    );
    assert!(
        local_count.load(Ordering::SeqCst) >= 1,
        "local upstream used"
    );
    let o = ask_udp(
        server,
        &query("connectivitycheck.gstatic.com.", RecordType::A, None),
    )
    .await;
    assert_eq!(o.answers[0].data, RData::A(A(Ipv4Addr::new(10, 0, 0, 53))));

    resolver.set_control(Control::Auto, "admin").await;
    let log = resolver.storage.audit_log(10).await.unwrap();
    assert!(
        log.iter().any(|e| e.action == "mode_change"
            && e.detail.contains("Isolation")
            && e.actor == "admin"),
        "{log:?}"
    );
    assert!(log.iter().any(|e| e.action == "mode_control"));
}

#[tokio::test]
async fn detects_isolation_when_international_upstreams_go_silent() {
    let (intl, _, down) = fake_upstream_switch().await;
    let (_, resolver) = start_full(vec![intl], |c| {
        c.upstream.timeout_ms = 200;
        c.upstream.health_interval_secs = 1;
        c.upstream.failure_threshold = 1;
        c.isolation.isolation_after_secs = 1;
        c.isolation.recover_after_secs = 1;
        c.isolation.check_interval_secs = 1;
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        resolver.mode_state().mode,
        Mode::Normal,
        "{:?}",
        resolver.mode_state()
    );
    down.store(true, Ordering::SeqCst);
    let mut waited = 0;
    while resolver.mode_state().mode != Mode::Isolation && waited < 60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(
        resolver.mode_state().mode,
        Mode::Isolation,
        "{:?}",
        resolver.mode_state()
    );
    down.store(false, Ordering::SeqCst);
    waited = 0;
    while resolver.mode_state().mode == Mode::Isolation && waited < 60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(
        resolver.mode_state().mode,
        Mode::Normal,
        "{:?}",
        resolver.mode_state()
    );
}

#[tokio::test]
async fn acl_drops_or_refuses_outsiders() {
    let up = fake_upstream().await;
    let q = query("a.test.", RecordType::A, None).to_vec().unwrap();
    let drop = start_cfg(vec![up], |c| {
        c.policy.allow = vec!["10.0.0.0/8".parse().unwrap()]
    })
    .await;
    assert!(
        udp_wait(drop, &q, 300).await.is_none(),
        "default action drops"
    );
    let refuse = start_cfg(vec![up], |c| {
        c.policy.allow = vec!["10.0.0.0/8".parse().unwrap()];
        c.policy.deny_action = nirvor_policy::DenyAction::Refuse;
    })
    .await;
    let r = Message::from_vec(&udp(refuse, &q).await.unwrap()).unwrap();
    assert_eq!(r.metadata.response_code, ResponseCode::Refused);
}

#[tokio::test]
async fn blocklist_nxdomain_and_null() {
    let (up, count) = fake_upstream_counted().await;
    let dir = std::env::temp_dir().join(format!("nirvor-bl-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let list = dir.join("block.txt");
    std::fs::write(&list, "a.test\n*.track.test\n").unwrap();
    let source = nirvor_policy::BlocklistSource {
        name: "test".into(),
        path: Some(list),
        url: None,
        enabled: true,
        refresh_secs: 60,
    };
    let nx = start_cfg(vec![up], |c| c.policy.blocklists = vec![source.clone()]).await;
    let r = ask_udp(nx, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
    let r = ask_udp(nx, &query("x.y.track.test.", RecordType::A, None)).await;
    assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "blocked names never reach the upstream"
    );

    let null = start_cfg(vec![up], |c| {
        c.policy.blocklists = vec![source.clone()];
        c.policy.block_response = nirvor_policy::BlockResponse::Null;
    })
    .await;
    let r = ask_udp(null, &query("a.test.", RecordType::A, None)).await;
    assert_eq!(r.answers[0].data, RData::A(A(Ipv4Addr::UNSPECIFIED)));
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn per_client_rate_limit_drops_excess() {
    let up = fake_upstream().await;
    let server = start_cfg(vec![up], |c| {
        c.policy.rate_limit.per_client_qps = 5;
        c.policy.rate_limit.burst = 5;
        c.policy.rrl.responses_per_second = 0;
    })
    .await;
    let q = query("a.test.", RecordType::A, None).to_vec().unwrap();
    let mut answered = 0;
    for _ in 0..10 {
        if udp_wait(server, &q, 150).await.is_some() {
            answered += 1;
        }
    }
    assert!(
        (5..=7).contains(&answered),
        "answered {answered} of 10 with a 5 qps limit"
    );
}

#[tokio::test]
async fn rrl_slips_truncated_answers() {
    let up = fake_upstream().await;
    let server = start_cfg(vec![up], |c| {
        c.policy.rrl.responses_per_second = 2;
        c.policy.rrl.slip = 2;
    })
    .await;
    let q = query("a.test.", RecordType::A, None).to_vec().unwrap();
    let mut tc = 0;
    let mut silent = 0;
    for _ in 0..6 {
        match udp_wait(server, &q, 150).await {
            Some(raw) if Message::from_vec(&raw).unwrap().metadata.truncation => tc += 1,
            Some(_) => {}
            None => silent += 1,
        }
    }
    assert!(tc >= 1 && silent >= 1, "tc {tc}, dropped {silent}");
    // TCP is never limited by RRL.
    let r = Message::from_vec(&tcp(server, &q).await).unwrap();
    assert_eq!(r.answers.len(), 1);
}

#[tokio::test]
async fn oversized_query_is_dropped() {
    let server = start_cfg(vec![fake_upstream().await], |c| {
        c.limits.max_query_size = 512
    })
    .await;
    let mut q = query("a.test.", RecordType::A, None).to_vec().unwrap();
    q.resize(600, 0);
    assert!(udp_wait(server, &q, 300).await.is_none());
}
