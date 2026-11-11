//! Configuration loaded from `config.toml`.

use crate::mode::IsolationConfig;
use crate::querylog::LoggingConfig;
use nirvor_cache::CacheConfig;
use nirvor_policy::PolicyConfig;
use nirvor_resilience::RootZoneConfig;
use nirvor_upstream::PoolConfig;
use nirvor_zones::ZoneConfig;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Listener settings.
    pub server: ServerConfig,
    /// Upstream resolvers to forward to.
    pub upstream: PoolConfig,
    /// Resource limits.
    #[serde(default)]
    pub limits: Limits,
    /// Answer cache.
    #[serde(default)]
    pub cache: CacheConfig,
    /// Dashboard and HTTP API.
    #[serde(default)]
    pub api: ApiConfig,
    /// Static local zones.
    #[serde(default)]
    pub zones: Vec<ZoneConfig>,
    /// Root zone copy, cache snapshots and pinned domains.
    #[serde(default)]
    pub resilience: ResilienceConfig,
    /// Isolation mode detection and behaviour.
    #[serde(default)]
    pub isolation: IsolationConfig,
    /// Access control, rate limits and blocklists.
    #[serde(default)]
    pub policy: PolicyConfig,
    /// Query log and history retention.
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Local resilience settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResilienceConfig {
    /// Directory for the root zone copy and cache snapshot.
    pub data_dir: PathBuf,
    /// Local root zone (RFC 8806).
    pub root_zone: RootZoneConfig,
    /// Seconds between cache snapshots (one is also written on shutdown).
    pub snapshot_interval_secs: u64,
    /// Domains kept warm: their A and AAAA answers are refreshed before expiry and kept stale for longer.
    pub pinned_domains: Vec<String>,
    /// Seconds between pinned-domain checks.
    pub pinned_refresh_secs: u64,
    /// Per-server timeout for iterative resolution, ms.
    pub recursion_timeout_ms: u64,
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            root_zone: RootZoneConfig::default(),
            snapshot_interval_secs: 300,
            pinned_domains: Vec::new(),
            pinned_refresh_secs: 60,
            recursion_timeout_ms: 1500,
        }
    }
}

/// Dashboard and HTTP API listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApiConfig {
    /// Address for the dashboard, API and `/metrics`. Keep it on a management network.
    pub listen: SocketAddr,
    /// PEM certificate chain; with `tls_key`, the dashboard is served over HTTPS.
    pub tls_cert: Option<PathBuf>,
    /// PEM private key.
    pub tls_key: Option<PathBuf>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8053)),
            tls_cert: None,
            tls_key: None,
        }
    }
}

/// Where the resolver listens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Addresses to listen on, for both UDP and TCP.
    pub listen: Vec<SocketAddr>,
    /// Largest UDP reply sent, whatever the client offers (DNS Flag Day 2020 recommends 1232).
    #[serde(default = "default_max_udp_payload")]
    pub max_udp_payload: u16,
    /// UDP sockets per listen address (SO_REUSEPORT). Default: one per CPU.
    #[serde(default)]
    pub udp_sockets: Option<usize>,
}

fn default_max_udp_payload() -> u16 {
    1232
}

/// Bounds that keep memory use fixed under load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Maximum UDP queries being answered at once; extra queries are dropped.
    pub max_inflight_udp: usize,
    /// Maximum open TCP connections; extra connections are closed at once.
    pub max_tcp_connections: usize,
    /// Close a TCP connection after this long without a query, in milliseconds (RFC 7766 §6.2.3).
    pub tcp_idle_timeout_ms: u64,
    /// Queries larger than this many bytes are dropped.
    pub max_query_size: usize,
    /// Longest time spent on one query before answering SERVFAIL, ms.
    pub query_timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_inflight_udp: 4096,
            max_tcp_connections: 512,
            tcp_idle_timeout_ms: 10_000,
            max_query_size: 4096,
            query_timeout_ms: 4000,
        }
    }
}

impl Config {
    /// Checks values that serde cannot.
    pub fn validate(&self) -> Result<(), String> {
        if self.server.listen.is_empty() {
            return Err("server.listen must have at least one address".into());
        }
        self.upstream.validate()?;
        nirvor_zones::LocalZones::new(&self.zones)?;
        self.policy.validate()?;
        if self.api.tls_cert.is_some() != self.api.tls_key.is_some() {
            return Err("api.tls_cert and api.tls_key must be set together".into());
        }
        nirvor_zones::LocalZones::new(&self.isolation.overrides)
            .map_err(|e| format!("isolation override {e}"))?;
        for d in &self.resilience.pinned_domains {
            hickory_proto::rr::Name::from_ascii(d)
                .map_err(|e| format!("pinned domain {d}: {e}"))?;
        }
        if self.limits.max_inflight_udp == 0 || self.limits.max_tcp_connections == 0 {
            return Err("limits must be above 0".into());
        }
        Ok(())
    }
}
