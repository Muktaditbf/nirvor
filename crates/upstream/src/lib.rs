//! Upstream pool: latency tracking (Jacobson/Karels RTT estimates), adaptive timeouts, active health
//! checks and failover. Healthy upstreams are tried fastest first.

mod exchange;

pub use exchange::{UpstreamError, exchange, matches};

use hickory_proto::op::{Message, Query, ResponseCode};
use hickory_proto::rr::{Name, RecordType};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// One upstream resolver from the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamSpec {
    /// Label shown in the dashboard.
    pub name: String,
    /// Address, for example `1.1.1.1:53`.
    pub addr: SocketAddr,
    /// Reached over international links. Used to detect isolation and skipped while isolated.
    #[serde(default = "yes")]
    pub international: bool,
    /// Disabled upstreams are kept in the config but never used.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Pool settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Upstreams, in preference order for equal latency.
    pub servers: Vec<UpstreamSpec>,
    /// Longest wait for one upstream, ms. The actual wait adapts to measured latency.
    #[serde(default = "d_timeout")]
    pub timeout_ms: u64,
    /// Shortest wait for one upstream, ms.
    #[serde(default = "d_min_timeout")]
    pub min_timeout_ms: u64,
    /// Most upstreams tried for one query.
    #[serde(default = "d_attempts")]
    pub max_attempts: usize,
    /// Seconds between health probes.
    #[serde(default = "d_interval")]
    pub health_interval_secs: u64,
    /// Consecutive failures before an upstream is marked down.
    #[serde(default = "d_threshold")]
    pub failure_threshold: u32,
    /// Name probed by health checks (type NS).
    #[serde(default = "d_probe")]
    pub probe_name: String,
}

fn d_timeout() -> u64 {
    1500
}
fn d_min_timeout() -> u64 {
    150
}
fn d_attempts() -> usize {
    3
}
fn d_interval() -> u64 {
    5
}
fn d_threshold() -> u32 {
    3
}
fn d_probe() -> String {
    ".".into()
}

impl PoolConfig {
    /// Checks values serde cannot.
    pub fn validate(&self) -> Result<(), String> {
        if self.servers.is_empty() {
            return Err("upstream.servers must list at least one server".into());
        }
        if self.min_timeout_ms == 0 || self.timeout_ms < self.min_timeout_ms {
            return Err("upstream timeouts: need 0 < min_timeout_ms <= timeout_ms".into());
        }
        if self.max_attempts == 0 || self.health_interval_secs == 0 || self.failure_threshold == 0 {
            return Err(
                "upstream max_attempts, health_interval_secs and failure_threshold must be above 0"
                    .into(),
            );
        }
        Name::from_ascii(&self.probe_name).map_err(|e| format!("upstream.probe_name: {e}"))?;
        let mut addrs: Vec<_> = self.servers.iter().map(|s| s.addr).collect();
        addrs.sort();
        addrs.dedup();
        if addrs.len() != self.servers.len() {
            return Err("upstream.servers has a duplicate address".into());
        }
        Ok(())
    }
}

/// One health check or query outcome, for history charts.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CheckSample {
    /// Unix seconds.
    pub ts: u64,
    /// Answered in time.
    pub ok: bool,
    /// Round-trip time, ms (0 when failed).
    pub rtt_ms: f64,
}

/// Status of one upstream for the API and dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamStatus {
    /// Config entry.
    #[serde(flatten)]
    pub spec: UpstreamSpec,
    /// Currently considered usable.
    pub healthy: bool,
    /// Smoothed RTT, ms (`None` before the first answer).
    pub srtt_ms: Option<f64>,
    /// Current per-attempt timeout, ms.
    pub timeout_ms: u64,
    /// Failures in a row.
    pub consecutive_failures: u32,
    /// Queries sent, lifetime.
    pub queries: u64,
    /// Failed queries, lifetime.
    pub failures: u64,
    /// Last error text.
    pub last_error: Option<String>,
    /// Recent health checks, oldest first (up to an hour at the default interval).
    pub history: Vec<CheckSample>,
}

const HISTORY: usize = 720;

#[derive(Debug, Default)]
struct State {
    srtt: Option<f64>,
    rttvar: f64,
    consecutive_failures: u32,
    down: bool,
    queries: u64,
    failures: u64,
    last_error: Option<String>,
    history: VecDeque<CheckSample>,
}

struct Upstream {
    spec: UpstreamSpec,
    state: Mutex<State>,
}

impl Upstream {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The upstream pool. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Pool {
    upstreams: Arc<RwLock<Vec<Arc<Upstream>>>>,
    config: Arc<RwLock<PoolConfig>>,
}

impl Pool {
    /// Builds a pool. Call [`Pool::run_health_checks`] in a task to keep health current.
    pub fn new(config: PoolConfig) -> Self {
        let pool = Self {
            upstreams: Arc::default(),
            config: Arc::new(RwLock::new(config.clone())),
        };
        pool.set_config(config);
        pool
    }

    /// Replaces the config, keeping measured state for addresses that stay.
    pub fn set_config(&self, config: PoolConfig) {
        let mut ups = self.upstreams.write().unwrap_or_else(|p| p.into_inner());
        let old: Vec<Arc<Upstream>> = std::mem::take(&mut *ups);
        for spec in &config.servers {
            let state = old
                .iter()
                .find(|u| u.spec.addr == spec.addr)
                .map(|u| std::mem::take(&mut *u.state()))
                .unwrap_or_default();
            ups.push(Arc::new(Upstream {
                spec: spec.clone(),
                state: Mutex::new(state),
            }));
        }
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = config;
    }

    /// Current config.
    pub fn config(&self) -> PoolConfig {
        self.config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn limits(&self) -> (Duration, Duration, usize) {
        let c = self.config.read().unwrap_or_else(|p| p.into_inner());
        (
            Duration::from_millis(c.min_timeout_ms),
            Duration::from_millis(c.timeout_ms),
            c.max_attempts,
        )
    }

    /// Upstreams `allow` accepts, healthy first, then by smoothed RTT (unmeasured last among healthy).
    fn ordered(&self, allow: &(dyn Fn(&UpstreamSpec) -> bool + Sync)) -> Vec<Arc<Upstream>> {
        let ups = self.upstreams.read().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<(bool, f64, usize, Arc<Upstream>)> = ups
            .iter()
            .enumerate()
            .filter(|(_, u)| u.spec.enabled && allow(&u.spec))
            .map(|(i, u)| {
                let s = u.state();
                (s.down, s.srtt.unwrap_or(f64::MAX), i, u.clone())
            })
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        list.into_iter().map(|x| x.3).collect()
    }

    /// Sends `query` to the best allowed upstreams in turn and returns the first usable answer.
    /// SERVFAIL and REFUSED make it try the next upstream; if all say so, the last such answer is returned.
    pub async fn forward(
        &self,
        query: &Message,
        allow: &(dyn Fn(&UpstreamSpec) -> bool + Sync),
    ) -> Result<Message, UpstreamError> {
        let (min_t, max_t, attempts) = self.limits();
        let mut last = UpstreamError::NoneUsable;
        let mut fallback = None;
        for up in self.ordered(allow).into_iter().take(attempts) {
            let wait = rto(&up.state(), min_t, max_t);
            let start = Instant::now();
            match exchange(up.spec.addr, query, wait).await {
                Ok(answer) => {
                    let rcode = answer.metadata.response_code;
                    // REFUSED means the upstream will not serve us: a health problem. SERVFAIL is usually the domain's fault.
                    self.record(
                        &up,
                        rcode != ResponseCode::Refused,
                        start.elapsed(),
                        Some(format!("answered {rcode}")),
                        false,
                    );
                    if matches!(rcode, ResponseCode::ServFail | ResponseCode::Refused) {
                        fallback = Some(answer);
                        continue;
                    }
                    return Ok(answer);
                }
                Err(e) => {
                    debug!(upstream = %up.spec.name, error = %e, "upstream failed, trying next");
                    self.record(&up, false, start.elapsed(), Some(e.to_string()), false);
                    last = e;
                }
            }
        }
        match fallback {
            Some(answer) => Ok(answer),
            None => Err(UpstreamError::AllFailed(Box::new(last))),
        }
    }

    fn record(
        &self,
        up: &Upstream,
        ok: bool,
        rtt: Duration,
        note: Option<String>,
        from_check: bool,
    ) {
        let threshold = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .failure_threshold;
        let mut s = up.state();
        s.queries += 1;
        let ms = rtt.as_secs_f64() * 1000.0;
        if ok {
            // RFC 6298 §2: rttvar = 3/4 rttvar + 1/4 |srtt - r|; srtt = 7/8 srtt + 1/8 r.
            match s.srtt {
                None => {
                    s.srtt = Some(ms);
                    s.rttvar = ms / 2.0;
                }
                Some(srtt) => {
                    s.rttvar = 0.75 * s.rttvar + 0.25 * (srtt - ms).abs();
                    s.srtt = Some(0.875 * srtt + 0.125 * ms);
                }
            }
            s.consecutive_failures = 0;
            if s.down {
                info!(upstream = %up.spec.name, "upstream is back up");
            }
            s.down = false;
        } else {
            s.failures += 1;
            s.consecutive_failures += 1;
            s.last_error = note;
            if !s.down && s.consecutive_failures >= threshold {
                warn!(upstream = %up.spec.name, error = ?s.last_error, "upstream marked down");
                s.down = true;
            }
        }
        if from_check {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if s.history.len() == HISTORY {
                s.history.pop_front();
            }
            s.history.push_back(CheckSample {
                ts,
                ok,
                rtt_ms: if ok { ms } else { 0.0 },
            });
        }
    }

    /// Probes every enabled upstream once.
    pub async fn check_all(&self) {
        let (probe, max_t) = {
            let c = self.config.read().unwrap_or_else(|p| p.into_inner());
            (c.probe_name.clone(), Duration::from_millis(c.timeout_ms))
        };
        let Ok(name) = Name::from_ascii(&probe) else {
            return;
        };
        let ups: Vec<Arc<Upstream>> = self
            .upstreams
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let checks = ups.into_iter().filter(|u| u.spec.enabled).map(|up| {
            let name = name.clone();
            let pool = self.clone();
            tokio::spawn(async move {
                let mut q = Message::query();
                q.metadata.recursion_desired = true;
                q.add_query(Query::query(name, RecordType::NS));
                let start = Instant::now();
                let result = exchange(up.spec.addr, &q, max_t).await;
                let (ok, note) = match &result {
                    Ok(a)
                        if !matches!(
                            a.metadata.response_code,
                            ResponseCode::ServFail | ResponseCode::Refused
                        ) =>
                    {
                        (true, None)
                    }
                    Ok(a) => (
                        false,
                        Some(format!("probe answered {}", a.metadata.response_code)),
                    ),
                    Err(e) => (false, Some(e.to_string())),
                };
                pool.record(&up, ok, start.elapsed(), note, true);
            })
        });
        for c in checks.collect::<Vec<_>>() {
            let _ = c.await;
        }
    }

    /// Runs health checks forever at the configured interval.
    pub async fn run_health_checks(self) {
        loop {
            self.check_all().await;
            let every = self
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .health_interval_secs;
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    }

    /// Status of every upstream, in config order.
    pub fn status(&self) -> Vec<UpstreamStatus> {
        let (min_t, max_t, _) = self.limits();
        let ups = self.upstreams.read().unwrap_or_else(|p| p.into_inner());
        ups.iter()
            .map(|u| {
                let s = u.state();
                UpstreamStatus {
                    spec: u.spec.clone(),
                    healthy: u.spec.enabled && !s.down && s.srtt.is_some(),
                    srtt_ms: s.srtt,
                    timeout_ms: u64::try_from(rto(&s, min_t, max_t).as_millis())
                        .unwrap_or(u64::MAX),
                    consecutive_failures: s.consecutive_failures,
                    queries: s.queries,
                    failures: s.failures,
                    last_error: s.last_error.clone(),
                    history: s.history.iter().copied().collect(),
                }
            })
            .collect()
    }

    /// (healthy, total) among enabled upstreams that `allow` accepts.
    pub fn health_count(&self, allow: &(dyn Fn(&UpstreamSpec) -> bool + Sync)) -> (usize, usize) {
        let st = self.status();
        let relevant: Vec<_> = st
            .iter()
            .filter(|s| s.spec.enabled && allow(&s.spec))
            .collect();
        (
            relevant.iter().filter(|s| s.healthy).count(),
            relevant.len(),
        )
    }
}

/// Retransmission timeout: srtt + 4·rttvar (RFC 6298 §2), clamped. Unmeasured upstreams get the maximum.
fn rto(s: &State, min_t: Duration, max_t: Duration) -> Duration {
    match s.srtt {
        None => max_t,
        Some(srtt) => {
            Duration::from_secs_f64((srtt + 4.0 * s.rttvar).max(0.0) / 1000.0).clamp(min_t, max_t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, port: u16, international: bool) -> UpstreamSpec {
        UpstreamSpec {
            name: name.into(),
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            international,
            enabled: true,
        }
    }

    fn pool(specs: Vec<UpstreamSpec>) -> Pool {
        Pool::new(PoolConfig {
            servers: specs,
            timeout_ms: 1000,
            min_timeout_ms: 100,
            max_attempts: 3,
            health_interval_secs: 5,
            failure_threshold: 2,
            probe_name: ".".into(),
        })
    }

    fn find(p: &Pool, name: &str) -> Arc<Upstream> {
        p.upstreams
            .read()
            .unwrap()
            .iter()
            .find(|u| u.spec.name == name)
            .unwrap()
            .clone()
    }

    #[test]
    fn orders_by_health_then_latency() {
        let p = pool(vec![
            spec("slow", 1, true),
            spec("fast", 2, true),
            spec("dead", 3, true),
        ]);
        p.record(
            &find(&p, "slow"),
            true,
            Duration::from_millis(80),
            None,
            false,
        );
        p.record(
            &find(&p, "fast"),
            true,
            Duration::from_millis(10),
            None,
            false,
        );
        p.record(
            &find(&p, "dead"),
            true,
            Duration::from_millis(1),
            None,
            false,
        );
        for _ in 0..2 {
            p.record(
                &find(&p, "dead"),
                false,
                Duration::ZERO,
                Some("timeout".into()),
                false,
            );
        }
        let order: Vec<String> = p
            .ordered(&|_| true)
            .iter()
            .map(|u| u.spec.name.clone())
            .collect();
        assert_eq!(order, ["fast", "slow", "dead"]);
    }

    #[test]
    fn marked_down_after_threshold_and_back_on_success() {
        let p = pool(vec![spec("a", 1, true)]);
        let a = find(&p, "a");
        p.record(&a, true, Duration::from_millis(10), None, false);
        p.record(&a, false, Duration::ZERO, None, false);
        assert!(p.status()[0].healthy, "one failure is below the threshold");
        p.record(&a, false, Duration::ZERO, None, false);
        assert!(!p.status()[0].healthy);
        p.record(&a, true, Duration::from_millis(10), None, true);
        assert!(p.status()[0].healthy);
        assert_eq!(
            p.status()[0].history.len(),
            1,
            "only health checks go into history"
        );
    }

    #[test]
    fn rto_adapts_and_is_clamped() {
        let mut s = State::default();
        assert_eq!(
            rto(&s, Duration::from_millis(100), Duration::from_millis(1000)),
            Duration::from_millis(1000)
        );
        s.srtt = Some(20.0);
        s.rttvar = 5.0;
        assert_eq!(
            rto(&s, Duration::from_millis(100), Duration::from_millis(1000)),
            Duration::from_millis(100)
        );
        s.srtt = Some(200.0);
        s.rttvar = 50.0;
        assert_eq!(
            rto(&s, Duration::from_millis(100), Duration::from_millis(1000)),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn filter_and_disabled_are_respected() {
        let mut off = spec("off", 3, false);
        off.enabled = false;
        let p = pool(vec![spec("intl", 1, true), spec("local", 2, false), off]);
        let local: Vec<String> = p
            .ordered(&|s| !s.international)
            .iter()
            .map(|u| u.spec.name.clone())
            .collect();
        assert_eq!(local, ["local"]);
        assert_eq!(
            p.health_count(&|_| true),
            (0, 2),
            "unmeasured counts as not healthy yet"
        );
    }

    #[test]
    fn set_config_keeps_state_for_same_address() {
        let p = pool(vec![spec("a", 1, true)]);
        p.record(&find(&p, "a"), true, Duration::from_millis(42), None, false);
        let mut c = p.config();
        c.servers[0].name = "renamed".into();
        c.servers.push(spec("b", 2, true));
        p.set_config(c);
        let st = p.status();
        assert_eq!(st[0].spec.name, "renamed");
        assert_eq!(st[0].srtt_ms, Some(42.0));
        assert_eq!(st[1].srtt_ms, None);
    }

    #[test]
    fn validate_rejects_duplicates_and_bad_timeouts() {
        let mut c = pool(vec![spec("a", 1, true), spec("b", 1, true)]).config();
        assert!(c.validate().is_err());
        c.servers.pop();
        assert!(c.validate().is_ok());
        c.min_timeout_ms = 5000;
        assert!(c.validate().is_err());
    }
}
