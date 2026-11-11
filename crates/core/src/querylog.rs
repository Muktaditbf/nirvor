//! Recent-query log, live query stream and top domains/clients, with privacy controls.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// How much about clients is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// Full client addresses.
    Full,
    /// IPv4 with the last octet zeroed, IPv6 cut to /48.
    Anonymized,
    /// No query log, no live stream, no per-client counts.
    Off,
}

/// Query logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
    /// Query log mode.
    pub query_log: Privacy,
    /// Recent queries kept in memory for the dashboard.
    pub buffer: usize,
    /// Days of per-minute traffic history kept in the database.
    pub history_days: i64,
    /// Days of audit log kept.
    pub audit_days: i64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            query_log: Privacy::Anonymized,
            buffer: 2000,
            history_days: 30,
            audit_days: 365,
        }
    }
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Fresh cache hit.
    Cache,
    /// Resolved through an upstream or iteratively.
    Upstream,
    /// Expired cache entry (serve-stale).
    Stale,
    /// Local zone, override or local root zone.
    Local,
    /// Blocked by policy.
    Blocked,
    /// Error reply (SERVFAIL, FORMERR, timeout).
    Error,
}

/// One answered query.
#[derive(Debug, Clone, Serialize)]
pub struct QueryEvent {
    /// Unix milliseconds.
    pub ts_ms: u64,
    /// Client address, possibly anonymized.
    pub client: String,
    /// Query name.
    pub name: String,
    /// Query type.
    pub qtype: String,
    /// Response code.
    pub rcode: String,
    /// Time to answer, ms.
    pub latency_ms: f64,
    /// Where the answer came from.
    pub source: Source,
    /// UDP or TCP.
    pub tcp: bool,
}

/// Ranked entry with a per-minute sparkline (oldest first).
#[derive(Debug, Clone, Serialize)]
pub struct TopEntry {
    /// Domain or client.
    pub key: String,
    /// Total in the window.
    pub total: u64,
    /// Count per minute.
    pub spark: Vec<u64>,
}

/// Minutes of top-N history.
pub const TOP_MINUTES: usize = 30;
/// Distinct keys tracked per minute; more are folded into "(other)" to bound memory.
const TOP_KEYS_PER_MINUTE: usize = 50_000;

#[derive(Default)]
struct Minute {
    minute: u64,
    domains: HashMap<String, u64>,
    clients: HashMap<String, u64>,
}

/// The log. Shared by reference.
pub struct QueryLog {
    recent: Mutex<VecDeque<QueryEvent>>,
    top: Mutex<VecDeque<Minute>>,
    live: broadcast::Sender<QueryEvent>,
}

impl Default for QueryLog {
    fn default() -> Self {
        Self {
            recent: Mutex::default(),
            top: Mutex::default(),
            live: broadcast::channel(1024).0,
        }
    }
}

/// Client address as stored under `privacy`, or `None` when nothing may be kept.
pub fn client_label(ip: IpAddr, privacy: Privacy) -> Option<String> {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match privacy {
        Privacy::Off => None,
        Privacy::Full => Some(ip.to_string()),
        Privacy::Anonymized => Some(match ip {
            IpAddr::V4(a) => {
                let o = a.octets();
                format!("{}.{}.{}.0", o[0], o[1], o[2])
            }
            IpAddr::V6(a) => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
            }
        }),
    }
}

fn bump(map: &mut HashMap<String, u64>, key: &str) {
    if let Some(v) = map.get_mut(key) {
        *v += 1;
    } else if map.len() < TOP_KEYS_PER_MINUTE {
        map.insert(key.to_string(), 1);
    } else {
        *map.entry("(other)".into()).or_default() += 1;
    }
}

impl QueryLog {
    /// Records one answered query.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        cfg: &LoggingConfig,
        client: IpAddr,
        name: &str,
        qtype: &str,
        rcode: &str,
        latency_ms: f64,
        source: Source,
        tcp: bool,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let minute = now.as_secs() / 60;
        let client = client_label(client, cfg.query_log);
        let domain = name.trim_end_matches('.').to_ascii_lowercase();
        {
            let mut top = self.top.lock().unwrap_or_else(|p| p.into_inner());
            if top.back().is_none_or(|m| m.minute != minute) {
                top.push_back(Minute {
                    minute,
                    ..Default::default()
                });
                while top.len() > TOP_MINUTES {
                    top.pop_front();
                }
            }
            if let Some(m) = top.back_mut() {
                bump(&mut m.domains, &domain);
                if let Some(c) = &client {
                    bump(&mut m.clients, c);
                }
            }
        }
        let Some(client) = client else { return };
        let ev = QueryEvent {
            ts_ms: u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
            client,
            name: domain,
            qtype: qtype.to_string(),
            rcode: rcode.to_string(),
            latency_ms,
            source,
            tcp,
        };
        {
            let mut r = self.recent.lock().unwrap_or_else(|p| p.into_inner());
            while r.len() >= cfg.buffer.max(1) {
                r.pop_front();
            }
            r.push_back(ev.clone());
        }
        let _ = self.live.send(ev); // no subscribers is fine
    }

    /// Recent queries, newest first.
    pub fn recent(&self, limit: usize) -> Vec<QueryEvent> {
        self.recent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clears the recent log (after switching privacy mode).
    pub fn clear(&self) {
        self.recent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.top.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Live feed of queries.
    pub fn subscribe(&self) -> broadcast::Receiver<QueryEvent> {
        self.live.subscribe()
    }

    /// Top `limit` domains (or clients) over the last [`TOP_MINUTES`] minutes.
    pub fn top(&self, clients: bool, limit: usize) -> Vec<TopEntry> {
        let now_min = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 60;
        let first = now_min + 1 - TOP_MINUTES as u64;
        let top = self.top.lock().unwrap_or_else(|p| p.into_inner());
        let mut totals: HashMap<&str, u64> = HashMap::new();
        for m in top.iter().filter(|m| m.minute >= first) {
            let map = if clients { &m.clients } else { &m.domains };
            for (k, v) in map {
                *totals.entry(k.as_str()).or_default() += v;
            }
        }
        let mut ranked: Vec<(&str, u64)> = totals.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        ranked
            .into_iter()
            .take(limit)
            .map(|(k, total)| {
                let spark = (first..=now_min)
                    .map(|minute| {
                        top.iter()
                            .find(|m| m.minute == minute)
                            .and_then(|m| {
                                if clients {
                                    m.clients.get(k)
                                } else {
                                    m.domains.get(k)
                                }
                            })
                            .copied()
                            .unwrap_or(0)
                    })
                    .collect();
                TopEntry {
                    key: k.to_string(),
                    total,
                    spark,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn anonymizes_addresses() {
        assert_eq!(
            client_label(ip("103.4.5.6"), Privacy::Anonymized).unwrap(),
            "103.4.5.0"
        );
        assert_eq!(
            client_label(ip("::ffff:103.4.5.6"), Privacy::Full).unwrap(),
            "103.4.5.6"
        );
        assert_eq!(
            client_label(ip("2401:db00:12:34::9"), Privacy::Anonymized).unwrap(),
            "2401:db00:12::/48"
        );
        assert_eq!(client_label(ip("10.0.0.1"), Privacy::Off), None);
    }

    #[test]
    fn records_recent_and_top() {
        let log = QueryLog::default();
        let cfg = LoggingConfig {
            buffer: 3,
            ..Default::default()
        };
        for (i, name) in ["a.test.", "b.test.", "a.test.", "A.TEST.", "c.test."]
            .iter()
            .enumerate()
        {
            log.record(
                &cfg,
                ip(&format!("10.0.0.{i}")),
                name,
                "A",
                "NOERROR",
                1.0,
                Source::Cache,
                false,
            );
        }
        let recent = log.recent(10);
        assert_eq!(recent.len(), 3, "buffer bounded");
        assert_eq!(recent[0].name, "c.test");
        assert_eq!(recent[0].client, "10.0.0.0", "anonymized by default");
        let top = log.top(false, 2);
        assert_eq!(top[0].key, "a.test");
        assert_eq!(top[0].total, 3);
        assert_eq!(top[0].spark.len(), TOP_MINUTES);
        assert_eq!(*top[0].spark.last().unwrap(), 3);
        let clients = log.top(true, 5);
        assert_eq!(clients[0].total, 5, "all /24-anonymized clients merge");
    }

    #[test]
    fn privacy_off_keeps_nothing_about_clients() {
        let log = QueryLog::default();
        let cfg = LoggingConfig {
            query_log: Privacy::Off,
            ..Default::default()
        };
        log.record(
            &cfg,
            ip("10.0.0.1"),
            "a.test.",
            "A",
            "NOERROR",
            1.0,
            Source::Cache,
            false,
        );
        assert!(log.recent(10).is_empty());
        assert!(log.top(true, 10).is_empty());
        assert_eq!(log.top(false, 10).len(), 1, "domain counts are kept");
    }
}
