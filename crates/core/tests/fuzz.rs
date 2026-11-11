//! Property tests: any bytes thrown at the pipeline must never panic, and every reply must echo
//! the query ID with QR set.

use hickory_proto::op::{Edns, Message, Query};
use hickory_proto::rr::{Name, RecordType};
use nirvor_core::Server;
use nirvor_core::config::Config;
use nirvor_core::handler::{Resolver, Transport};
use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

async fn resolver(tag: &str) -> Arc<Resolver> {
    let dir = std::env::temp_dir().join(format!("nirvor-fuzz-{}-{tag}", std::process::id()));
    let text = format!(
        "[server]\nlisten = [\"127.0.0.1:0\"]\nudp_sockets = 1\n\n[upstream]\ntimeout_ms = 50\nmin_timeout_ms = 50\nmax_attempts = 1\n\n[[upstream.servers]]\nname = \"dead\"\naddr = \"127.0.0.1:9\"\n\n[limits]\nquery_timeout_ms = 100\n\n[policy.rate_limit]\nper_client_qps = 0\n\n[policy.rrl]\nresponses_per_second = 0\n\n[resilience]\ndata_dir = \"{}\"\n\n[resilience.root_zone]\nenabled = false\n",
        dir.display()
    );
    let config: Config = toml::from_str(&text).unwrap();
    Server::bind(&config).await.unwrap().resolver()
}

fn valid_query() -> Vec<u8> {
    let mut m = Message::query();
    m.metadata.recursion_desired = true;
    m.add_query(Query::query(
        Name::from_ascii("fuzz.example.").unwrap(),
        RecordType::A,
    ));
    m.set_edns(Edns::new());
    m.to_vec().unwrap()
}

static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static REPLIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn check(r: &Arc<Resolver>, packet: Vec<u8>, transport: Transport) -> Result<(), TestCaseError> {
    RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let client = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let reply = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(r.handle(&packet, transport, client))
    });
    if let Some(reply) = reply {
        REPLIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        prop_assert!(reply.len() >= 12, "reply shorter than a header");
        prop_assert_eq!(&reply[0..2], &packet[0..2], "reply must echo the ID");
        prop_assert!(reply[2] & 0x80 != 0, "reply must have QR set");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_bytes_never_panic() {
    let r = resolver("random").await;
    let mut runner = TestRunner::new(PtConfig {
        cases: 2000,
        ..PtConfig::default()
    });
    runner
        .run(&proptest::collection::vec(any::<u8>(), 0..600), |bytes| {
            check(&r, bytes, Transport::Udp)
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutated_queries_never_panic() {
    let r = resolver("mutated").await;
    let base = valid_query();
    let len = base.len();
    let strategy = (
        proptest::collection::vec((0..len, any::<u8>()), 1..6),
        0..len + 8,
        any::<bool>(),
    );
    let mut runner = TestRunner::new(PtConfig {
        cases: 400,
        ..PtConfig::default()
    });
    runner
        .run(&strategy, |(edits, cut, tcp)| {
            let mut p = base.clone();
            for (i, b) in edits {
                p[i] = b;
            }
            p.resize(cut.max(12), 0);
            p[2] &= 0x7f; // keep it a query so it goes through the whole pipeline
            check(&r, p, if tcp { Transport::Tcp } else { Transport::Udp })
        })
        .unwrap();
    eprintln!(
        "fuzz: {} packets handled, {} replies",
        RUNS.load(std::sync::atomic::Ordering::Relaxed),
        REPLIES.load(std::sync::atomic::Ordering::Relaxed)
    );
}
