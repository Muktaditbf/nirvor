//! dnsperf-style load generator: keeps N UDP queries in flight for a fixed time and reports
//! throughput, loss and latency percentiles. Query file format is dnsperf's: `name type` per line.

use anyhow::{Context, Result, bail};
use clap::Parser;
use hickory_proto::op::{Message, Query};
use hickory_proto::rr::{Name, RecordType};
use nirvor_core::metrics::{Histogram, quantile_ms};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

#[derive(Parser)]
#[command(about = "UDP DNS load generator (dnsperf-like)")]
struct Args {
    /// Server to test.
    #[arg(short, long, default_value = "127.0.0.1:5300")]
    server: SocketAddr,
    /// Query file, one `name type` per line.
    #[arg(short = 'd', long)]
    data: std::path::PathBuf,
    /// Seconds to run.
    #[arg(short = 'l', long, default_value_t = 20)]
    seconds: u64,
    /// Queries in flight at once (one socket each).
    #[arg(short = 'c', long, default_value_t = 64)]
    concurrency: usize,
    /// Per-query timeout in milliseconds; slower answers count as lost.
    #[arg(short = 't', long, default_value_t = 2000)]
    timeout_ms: u64,
}

fn load_queries(path: &std::path::Path) -> Result<Vec<Vec<u8>>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(rtype)) = (parts.next(), parts.next()) else {
            bail!("bad line: {line}");
        };
        let mut m = Message::query();
        m.metadata.recursion_desired = true;
        m.add_query(Query::query(
            Name::from_str(name)?,
            RecordType::from_str(rtype)?,
        ));
        out.push(m.to_vec()?);
    }
    if out.is_empty() {
        bail!("no queries in {}", path.display());
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let queries = Arc::new(load_queries(&args.data)?);
    let next = Arc::new(AtomicUsize::new(0));
    let (sent, answered, lost) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let hist = Arc::new(Histogram::default());
    let end = Instant::now() + Duration::from_secs(args.seconds);
    let wait = Duration::from_millis(args.timeout_ms);

    let mut tasks = Vec::new();
    for _ in 0..args.concurrency {
        let (queries, next, sent, answered, lost, hist) = (
            queries.clone(),
            next.clone(),
            sent.clone(),
            answered.clone(),
            lost.clone(),
            hist.clone(),
        );
        let sock = UdpSocket::bind(if args.server.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .await?;
        sock.connect(args.server).await?;
        tasks.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 65_535];
            let mut id: u16 = rand_id();
            while Instant::now() < end {
                let mut q = queries[next.fetch_add(1, Relaxed) % queries.len()].clone();
                id = id.wrapping_add(1);
                q[0..2].copy_from_slice(&id.to_be_bytes());
                let t = Instant::now();
                if sock.send(&q).await.is_err() {
                    lost.fetch_add(1, Relaxed);
                    continue;
                }
                sent.fetch_add(1, Relaxed);
                let deadline = t + wait;
                loop {
                    match tokio::time::timeout_at(deadline.into(), sock.recv(&mut buf)).await {
                        Ok(Ok(n)) if n >= 2 && buf[0..2] == id.to_be_bytes() => {
                            hist.observe(t.elapsed());
                            answered.fetch_add(1, Relaxed);
                            break;
                        }
                        Ok(Ok(_)) => continue, // late answer to an earlier query
                        _ => {
                            lost.fetch_add(1, Relaxed);
                            break;
                        }
                    }
                }
            }
        }));
    }
    let start = Instant::now();
    for t in tasks {
        t.await?;
    }
    let secs = start.elapsed().as_secs_f64();
    let counts = hist.snapshot();
    let ok = answered.load(Relaxed);
    println!("server            {}", args.server);
    println!(
        "duration          {secs:.1} s, concurrency {}",
        args.concurrency
    );
    println!("queries sent      {}", sent.load(Relaxed));
    println!("queries answered  {ok}");
    println!("queries lost      {}", lost.load(Relaxed));
    println!("throughput        {:.0} qps", ok as f64 / secs);
    println!(
        "latency ms        p50 {:.3}  p95 {:.3}  p99 {:.3}",
        quantile_ms(&counts, 0.50),
        quantile_ms(&counts, 0.95),
        quantile_ms(&counts, 0.99)
    );
    Ok(())
}

fn rand_id() -> u16 {
    // Process ID and clock are enough to spread IDs between sockets; this is a load tool, not a resolver.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ std::process::id()) as u16
}
