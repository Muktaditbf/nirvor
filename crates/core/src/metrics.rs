//! Counters, a latency histogram and a per-second time series, with Prometheus text output.

use serde::Serialize;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Histogram buckets: bucket `i` holds latencies below 2^i microseconds (bucket 23 ≈ 8.4 s and above).
pub const BUCKETS: usize = 24;

/// Lock-free latency histogram.
#[derive(Default)]
pub struct Histogram {
    counts: [AtomicU64; BUCKETS],
    sum_us: AtomicU64,
}

impl Histogram {
    /// Records one latency.
    pub fn observe(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        let i = ((64 - us.leading_zeros()) as usize).min(BUCKETS - 1);
        self.counts[i].fetch_add(1, Relaxed);
        self.sum_us.fetch_add(us, Relaxed);
    }

    /// Current cumulative counts per bucket.
    pub fn snapshot(&self) -> [u64; BUCKETS] {
        std::array::from_fn(|i| self.counts[i].load(Relaxed))
    }
}

/// Estimated `q` quantile (0..1) in milliseconds from per-bucket counts, interpolating inside the bucket.
pub fn quantile_ms(counts: &[u64; BUCKETS], q: f64) -> f64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let target = q * total as f64;
    let mut seen = 0.0;
    for (i, &c) in counts.iter().enumerate() {
        let next = seen + c as f64;
        if next >= target && c > 0 {
            let lo = if i == 0 {
                0.0
            } else {
                (1u64 << (i - 1)) as f64
            };
            let hi = (1u64 << i) as f64;
            let frac = (target - seen) / c as f64;
            return (lo + frac * (hi - lo)) / 1000.0;
        }
        seen = next;
    }
    (1u64 << (BUCKETS - 1)) as f64 / 1000.0
}

/// All counters. Shared by reference; every field is atomic.
#[derive(Default)]
pub struct Metrics {
    /// Queries received (after basic parsing).
    pub queries: AtomicU64,
    /// Queries over TCP.
    pub tcp_queries: AtomicU64,
    /// Answered from fresh cache.
    pub cache_hits: AtomicU64,
    /// Needed an upstream.
    pub cache_misses: AtomicU64,
    /// Answered from expired cache because upstreams failed or were slow.
    pub stale_served: AtomicU64,
    /// Background refreshes started.
    pub prefetches: AtomicU64,
    /// SERVFAIL replies.
    pub servfail: AtomicU64,
    /// NXDOMAIN replies.
    pub nxdomain: AtomicU64,
    /// FORMERR replies.
    pub formerr: AtomicU64,
    /// REFUSED replies (ACL).
    pub refused: AtomicU64,
    /// Blocked by policy.
    pub blocked: AtomicU64,
    /// Dropped or slipped by rate limiting.
    pub rate_limited: AtomicU64,
    /// Answered from local zones, overrides or the local root zone.
    pub local_answers: AtomicU64,
    /// Answered by our own iterative resolution.
    pub recursive_answers: AtomicU64,
    /// Upstream exchanges that failed.
    pub upstream_errors: AtomicU64,
    /// Time from receiving a query to having its reply.
    pub latency: Histogram,
    series: Mutex<Series>,
}

/// One second of traffic, for live charts.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Sample {
    /// Unix seconds.
    pub ts: u64,
    /// Queries in this second.
    pub qps: u64,
    /// Fresh cache hits in this second.
    pub hits: u64,
    /// Cache misses in this second.
    pub misses: u64,
    /// Median latency, ms.
    pub p50: f64,
    /// 95th percentile latency, ms.
    pub p95: f64,
    /// 99th percentile latency, ms.
    pub p99: f64,
}

#[derive(Default)]
struct Series {
    samples: VecDeque<Sample>,
    hist_deltas: VecDeque<[u64; BUCKETS]>,
    last: Option<(u64, u64, u64, [u64; BUCKETS])>,
}

/// Seconds of per-second history kept in memory.
pub const SERIES_LEN: usize = 3600;
/// Seconds in the rolling window for summary percentiles.
pub const WINDOW: usize = 60;

/// Summary over the last [`WINDOW`] seconds.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Average queries per second.
    pub qps: f64,
    /// Fresh hits / (hits + misses), 0..1.
    pub hit_ratio: f64,
    /// Latency percentiles, ms.
    pub p50: f64,
    /// 95th percentile, ms.
    pub p95: f64,
    /// 99th percentile, ms.
    pub p99: f64,
    /// Lifetime totals.
    pub totals: Totals,
}

/// Lifetime counter values.
#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    /// See [`Metrics`] fields.
    pub queries: u64,
    /// Fresh hits.
    pub cache_hits: u64,
    /// Misses.
    pub cache_misses: u64,
    /// Stale answers served.
    pub stale_served: u64,
    /// Prefetches.
    pub prefetches: u64,
    /// SERVFAIL.
    pub servfail: u64,
    /// NXDOMAIN.
    pub nxdomain: u64,
    /// Blocked.
    pub blocked: u64,
    /// Rate limited.
    pub rate_limited: u64,
    /// Refused.
    pub refused: u64,
    /// Local answers.
    pub local_answers: u64,
    /// Recursive answers.
    pub recursive_answers: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Metrics {
    /// Adds one to a counter.
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Relaxed);
    }

    /// Takes a one-second sample. Call once per second.
    pub fn tick(&self) {
        let q = self.queries.load(Relaxed);
        let h = self.cache_hits.load(Relaxed);
        let m = self.cache_misses.load(Relaxed);
        let hist = self.latency.snapshot();
        let Ok(mut s) = self.series.lock() else {
            return;
        };
        if let Some((lq, lh, lm, lhist)) = s.last {
            let delta: [u64; BUCKETS] = std::array::from_fn(|i| hist[i].saturating_sub(lhist[i]));
            let sample = Sample {
                ts: now_unix(),
                qps: q.saturating_sub(lq),
                hits: h.saturating_sub(lh),
                misses: m.saturating_sub(lm),
                p50: quantile_ms(&delta, 0.50),
                p95: quantile_ms(&delta, 0.95),
                p99: quantile_ms(&delta, 0.99),
            };
            push_capped(&mut s.samples, sample, SERIES_LEN);
            push_capped(&mut s.hist_deltas, delta, WINDOW);
        }
        s.last = Some((q, h, m, hist));
    }

    /// Per-second samples, oldest first, at most `seconds` of them.
    pub fn series(&self, seconds: usize) -> Vec<Sample> {
        let Ok(s) = self.series.lock() else {
            return Vec::new();
        };
        let skip = s.samples.len().saturating_sub(seconds);
        s.samples.iter().skip(skip).copied().collect()
    }

    /// Rolling summary of the last minute plus lifetime totals.
    pub fn summary(&self) -> Summary {
        let (qps, hits, misses, hist) = match self.series.lock() {
            Ok(s) => {
                let n = s.samples.len().min(WINDOW);
                let recent = s.samples.iter().rev().take(n);
                let (q, h, m) = recent.fold((0, 0, 0), |a, x| {
                    (a.0 + x.qps, a.1 + x.hits, a.2 + x.misses)
                });
                let mut hist = [0u64; BUCKETS];
                for d in &s.hist_deltas {
                    for i in 0..BUCKETS {
                        hist[i] += d[i];
                    }
                }
                (if n == 0 { 0.0 } else { q as f64 / n as f64 }, h, m, hist)
            }
            Err(_) => (0.0, 0, 0, [0; BUCKETS]),
        };
        Summary {
            qps,
            hit_ratio: if hits + misses == 0 {
                0.0
            } else {
                hits as f64 / (hits + misses) as f64
            },
            p50: quantile_ms(&hist, 0.50),
            p95: quantile_ms(&hist, 0.95),
            p99: quantile_ms(&hist, 0.99),
            totals: self.totals(),
        }
    }

    /// Lifetime totals.
    pub fn totals(&self) -> Totals {
        Totals {
            queries: self.queries.load(Relaxed),
            cache_hits: self.cache_hits.load(Relaxed),
            cache_misses: self.cache_misses.load(Relaxed),
            stale_served: self.stale_served.load(Relaxed),
            prefetches: self.prefetches.load(Relaxed),
            servfail: self.servfail.load(Relaxed),
            nxdomain: self.nxdomain.load(Relaxed),
            blocked: self.blocked.load(Relaxed),
            rate_limited: self.rate_limited.load(Relaxed),
            refused: self.refused.load(Relaxed),
            local_answers: self.local_answers.load(Relaxed),
            recursive_answers: self.recursive_answers.load(Relaxed),
        }
    }

    /// Prometheus text exposition (format 0.0.4). `extra` lines are appended as-is.
    pub fn prometheus(&self, extra: &str) -> String {
        let mut out = String::new();
        let counters: [(&str, &str, &AtomicU64); 15] = [
            ("queries_total", "Queries received", &self.queries),
            (
                "tcp_queries_total",
                "Queries received over TCP",
                &self.tcp_queries,
            ),
            (
                "cache_hits_total",
                "Answers from fresh cache",
                &self.cache_hits,
            ),
            (
                "cache_misses_total",
                "Queries needing an upstream",
                &self.cache_misses,
            ),
            (
                "stale_served_total",
                "Answers from expired cache (RFC 8767)",
                &self.stale_served,
            ),
            (
                "prefetches_total",
                "Background cache refreshes",
                &self.prefetches,
            ),
            ("servfail_total", "SERVFAIL replies", &self.servfail),
            ("nxdomain_total", "NXDOMAIN replies", &self.nxdomain),
            ("formerr_total", "FORMERR replies", &self.formerr),
            ("refused_total", "REFUSED replies", &self.refused),
            ("blocked_total", "Queries blocked by policy", &self.blocked),
            (
                "rate_limited_total",
                "Queries dropped or slipped by rate limiting",
                &self.rate_limited,
            ),
            (
                "local_answers_total",
                "Answers from local data",
                &self.local_answers,
            ),
            (
                "recursive_answers_total",
                "Answers from iterative resolution",
                &self.recursive_answers,
            ),
            (
                "upstream_errors_total",
                "Failed upstream exchanges",
                &self.upstream_errors,
            ),
        ];
        for (name, help, c) in counters {
            let _ = writeln!(
                out,
                "# HELP nirvor_{name} {help}\n# TYPE nirvor_{name} counter\nnirvor_{name} {}",
                c.load(Relaxed)
            );
        }
        let hist = self.latency.snapshot();
        let _ = writeln!(
            out,
            "# HELP nirvor_query_duration_seconds Query latency\n# TYPE nirvor_query_duration_seconds histogram"
        );
        let mut cum = 0;
        for (i, c) in hist.iter().enumerate().take(BUCKETS - 1) {
            cum += c;
            let le = (1u64 << i) as f64 / 1e6;
            let _ = writeln!(
                out,
                "nirvor_query_duration_seconds_bucket{{le=\"{le}\"}} {cum}"
            );
        }
        cum += hist[BUCKETS - 1];
        let _ = writeln!(
            out,
            "nirvor_query_duration_seconds_bucket{{le=\"+Inf\"}} {cum}"
        );
        let _ = writeln!(
            out,
            "nirvor_query_duration_seconds_sum {}",
            self.latency.sum_us.load(Relaxed) as f64 / 1e6
        );
        let _ = writeln!(out, "nirvor_query_duration_seconds_count {cum}");
        out.push_str(extra);
        out
    }
}

fn push_capped<T>(q: &mut VecDeque<T>, v: T, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_and_quantiles() {
        let h = Histogram::default();
        for _ in 0..90 {
            h.observe(Duration::from_micros(100)); // bucket 7: [64, 128) µs
        }
        for _ in 0..10 {
            h.observe(Duration::from_millis(20)); // bucket 15: [16384, 32768) µs
        }
        let c = h.snapshot();
        assert_eq!(c[7], 90);
        assert_eq!(c[15], 10);
        let p50 = quantile_ms(&c, 0.5);
        assert!((0.064..=0.128).contains(&p50), "p50 {p50}");
        let p99 = quantile_ms(&c, 0.99);
        assert!((16.0..=33.0).contains(&p99), "p99 {p99}");
    }

    #[test]
    fn tick_builds_series_and_summary() {
        let m = Metrics::default();
        m.tick();
        for _ in 0..5 {
            Metrics::inc(&m.queries);
        }
        Metrics::inc(&m.cache_hits);
        Metrics::inc(&m.cache_misses);
        m.tick();
        let s = m.series(10);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].qps, 5);
        let sum = m.summary();
        assert!((sum.hit_ratio - 0.5).abs() < 1e-9);
        assert!(m.prometheus("").contains("nirvor_queries_total 5"));
    }
}
