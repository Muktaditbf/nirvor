//! Who may query (ACL), how fast (per-client rate limit, response rate limiting) and what is blocked.

pub mod blocklist;

pub use blocklist::{BlocklistManager, BlocklistSource, BlocklistStatus};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What to do with a query from outside the ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DenyAction {
    /// Send nothing (safest against reflection).
    Drop,
    /// Answer REFUSED.
    Refuse,
}

/// How blocked names are answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockResponse {
    /// NXDOMAIN.
    Nxdomain,
    /// REFUSED.
    Refused,
    /// 0.0.0.0 / :: for A / AAAA, empty answer for other types.
    Null,
    /// The configured redirect addresses (a block-notice page) for A / AAAA.
    Redirect,
}

/// Per-client query rate limit (token bucket).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
    /// Sustained queries per second per client (0 = off).
    pub per_client_qps: u32,
    /// Bucket size: short bursts allowed above the rate.
    pub burst: u32,
    /// IPv6 clients are grouped by this prefix length, so one host cannot dodge the limit by rotating addresses.
    pub ipv6_prefix: u8,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_client_qps: 300,
            burst: 600,
            ipv6_prefix: 64,
        }
    }
}

/// Response rate limiting for UDP, against reflection/amplification abuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RrlConfig {
    /// Identical responses per second to one client network (0 = off).
    pub responses_per_second: u32,
    /// Every `slip`-th limited response is sent truncated (TC=1) so real clients retry over TCP; 0 = drop all.
    pub slip: u32,
    /// IPv4 network size for grouping clients.
    pub ipv4_prefix: u8,
    /// IPv6 network size for grouping clients.
    pub ipv6_prefix: u8,
}

impl Default for RrlConfig {
    fn default() -> Self {
        Self {
            responses_per_second: 50,
            slip: 2,
            ipv4_prefix: 24,
            ipv6_prefix: 56,
        }
    }
}

/// Policy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyConfig {
    /// Networks allowed to query. Everything else is denied (never run an open resolver).
    pub allow: Vec<IpNet>,
    /// What to do with denied queries.
    pub deny_action: DenyAction,
    /// Per-client query limit.
    pub rate_limit: RateLimitConfig,
    /// Response rate limiting.
    pub rrl: RrlConfig,
    /// How to answer blocked names.
    pub block_response: BlockResponse,
    /// IPv4 address for `redirect`.
    pub redirect_ipv4: Option<Ipv4Addr>,
    /// IPv6 address for `redirect`.
    pub redirect_ipv6: Option<Ipv6Addr>,
    /// TTL of blocked answers.
    pub block_ttl: u32,
    /// Blocklist sources.
    pub blocklists: Vec<BlocklistSource>,
    /// Seconds between checks of blocklist files for changes.
    pub reload_check_secs: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allow: vec![
                "127.0.0.0/8".parse().expect("valid"),
                "::1/128".parse().expect("valid"),
            ],
            deny_action: DenyAction::Drop,
            rate_limit: RateLimitConfig::default(),
            rrl: RrlConfig::default(),
            block_response: BlockResponse::Nxdomain,
            redirect_ipv4: None,
            redirect_ipv6: None,
            block_ttl: 60,
            blocklists: Vec::new(),
            reload_check_secs: 30,
        }
    }
}

impl PolicyConfig {
    /// Checks values serde cannot.
    pub fn validate(&self) -> Result<(), String> {
        if self.block_response == BlockResponse::Redirect
            && self.redirect_ipv4.is_none()
            && self.redirect_ipv6.is_none()
        {
            return Err(
                "policy.block_response = \"redirect\" needs redirect_ipv4 or redirect_ipv6".into(),
            );
        }
        if self.rate_limit.per_client_qps > 0
            && self.rate_limit.burst < self.rate_limit.per_client_qps
        {
            return Err("policy.rate_limit.burst must be at least per_client_qps".into());
        }
        if self.rate_limit.ipv6_prefix > 128
            || self.rrl.ipv6_prefix > 128
            || self.rrl.ipv4_prefix > 32
        {
            return Err("policy prefix lengths out of range".into());
        }
        if self.allow.iter().any(|n| n.prefix_len() == 0) {
            return Err(
                "policy.allow must not contain 0.0.0.0/0 or ::/0: that makes an open resolver"
                    .into(),
            );
        }
        for b in &self.blocklists {
            if b.path.is_none() == b.url.is_none() {
                return Err(format!(
                    "blocklist {}: set exactly one of path or url",
                    b.name
                ));
            }
        }
        Ok(())
    }
}

/// IPv4-mapped IPv6 addresses (from dual-stack sockets) are treated as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// True if `ip` is inside one of `allow`.
pub fn acl_allows(allow: &[IpNet], ip: IpAddr) -> bool {
    let ip = canonical(ip);
    allow.iter().any(|n| n.contains(&ip))
}

/// `ip` truncated to a network of `v4`/`v6` prefix bits.
fn network(ip: IpAddr, v4: u8, v6: u8) -> IpAddr {
    match canonical(ip) {
        IpAddr::V4(a) => IpAddr::V4(Ipv4Addr::from_bits(
            a.to_bits()
                & u32::MAX
                    .checked_shl(32 - u32::from(v4.min(32)))
                    .unwrap_or(0),
        )),
        IpAddr::V6(a) => IpAddr::V6(Ipv6Addr::from_bits(
            a.to_bits()
                & u128::MAX
                    .checked_shl(128 - u32::from(v6.min(128)))
                    .unwrap_or(0),
        )),
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
    limited: u32,
}

impl Bucket {
    fn take(&mut self, rate: f64, burst: f64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Token buckets keyed by `K`, with bounded memory (idle buckets are dropped).
struct Buckets<K: std::hash::Hash + Eq + Send + Sync + 'static> {
    map: moka::sync::Cache<K, Arc<Mutex<Bucket>>>,
}

impl<K: std::hash::Hash + Eq + Send + Sync + Clone + 'static> Buckets<K> {
    fn new(capacity: u64) -> Self {
        Self {
            map: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(Duration::from_secs(60))
                .build(),
        }
    }

    /// Takes a token; returns `None` if allowed, else `Some(number of limited events so far)`.
    fn check(&self, key: K, rate: f64, burst: f64, now: Instant) -> Option<u32> {
        let b = self.map.get_with(key, || {
            Arc::new(Mutex::new(Bucket {
                tokens: burst,
                last: now,
                limited: 0,
            }))
        });
        let mut b = b.lock().unwrap_or_else(|p| p.into_inner());
        if b.take(rate, burst, now) {
            None
        } else {
            b.limited = b.limited.wrapping_add(1);
            Some(b.limited)
        }
    }
}

/// What RRL decided for one UDP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RrlVerdict {
    /// Send the response.
    Send,
    /// Send a truncated (TC=1) empty response instead.
    Slip,
    /// Send nothing.
    Drop,
}

/// Runtime rate limiters.
pub struct Limiter {
    clients: Buckets<IpAddr>,
    responses: Buckets<(IpAddr, String, u16)>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            clients: Buckets::new(200_000),
            responses: Buckets::new(200_000),
        }
    }
}

impl Limiter {
    /// Per-client query limit. True if the query may proceed.
    pub fn allow_query(&self, cfg: &RateLimitConfig, client: IpAddr) -> bool {
        if cfg.per_client_qps == 0 {
            return true;
        }
        let key = network(client, 32, cfg.ipv6_prefix);
        self.clients
            .check(
                key,
                f64::from(cfg.per_client_qps),
                f64::from(cfg.burst),
                Instant::now(),
            )
            .is_none()
    }

    /// Response rate limiting for one UDP response to `client` for (`qname`, `qtype`).
    pub fn rrl(&self, cfg: &RrlConfig, client: IpAddr, qname: &str, qtype: u16) -> RrlVerdict {
        if cfg.responses_per_second == 0 {
            return RrlVerdict::Send;
        }
        let net = network(client, cfg.ipv4_prefix, cfg.ipv6_prefix);
        let rate = f64::from(cfg.responses_per_second);
        match self.responses.check(
            (net, qname.to_ascii_lowercase(), qtype),
            rate,
            rate,
            Instant::now(),
        ) {
            None => RrlVerdict::Send,
            Some(n) if cfg.slip > 0 && n % cfg.slip == 0 => RrlVerdict::Slip,
            Some(_) => RrlVerdict::Drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn acl_default_is_loopback_only_and_maps_v4_in_v6() {
        let allow = PolicyConfig::default().allow;
        assert!(acl_allows(&allow, ip("127.0.0.1")));
        assert!(acl_allows(&allow, ip("::1")));
        assert!(acl_allows(&allow, ip("::ffff:127.0.0.1")));
        assert!(!acl_allows(&allow, ip("8.8.8.8")));
        assert!(!acl_allows(&allow, ip("10.0.0.1")));
        let isp: Vec<IpNet> = vec![
            "103.0.0.0/22".parse().unwrap(),
            "2401:db00::/32".parse().unwrap(),
        ];
        assert!(acl_allows(&isp, ip("103.0.3.255")));
        assert!(!acl_allows(&isp, ip("103.0.4.0")));
        assert!(acl_allows(&isp, ip("2401:db00:1::5")));
    }

    #[test]
    fn validate_refuses_open_resolver() {
        let mut c = PolicyConfig::default();
        c.allow.push("0.0.0.0/0".parse().unwrap());
        assert!(c.validate().is_err());
    }

    #[test]
    fn network_truncation() {
        assert_eq!(network(ip("10.1.2.3"), 24, 56), ip("10.1.2.0"));
        assert_eq!(network(ip("2001:db8:1:2:3::9"), 24, 56), ip("2001:db8:1::"));
        assert_eq!(network(ip("10.1.2.3"), 0, 0), ip("0.0.0.0"));
    }

    #[test]
    fn token_bucket_allows_burst_then_rate() {
        let b = Buckets::new(10);
        let t0 = Instant::now();
        let allowed = (0..15)
            .filter(|_| b.check("k", 5.0, 10.0, t0).is_none())
            .count();
        assert_eq!(allowed, 10, "burst");
        assert!(
            b.check("k", 5.0, 10.0, t0 + Duration::from_millis(200))
                .is_none(),
            "one token after 0.2 s at 5/s"
        );
        assert!(
            b.check("k", 5.0, 10.0, t0 + Duration::from_millis(200))
                .is_some()
        );
    }

    #[test]
    fn per_client_limit_groups_ipv6_by_prefix() {
        let l = Limiter::default();
        let cfg = RateLimitConfig {
            per_client_qps: 2,
            burst: 2,
            ipv6_prefix: 64,
        };
        assert!(l.allow_query(&cfg, ip("2001:db8::1")));
        assert!(l.allow_query(&cfg, ip("2001:db8::2")));
        assert!(
            !l.allow_query(&cfg, ip("2001:db8::3")),
            "same /64 shares the bucket"
        );
        assert!(
            l.allow_query(&cfg, ip("2001:db8:0:1::1")),
            "other /64 has its own"
        );
    }

    #[test]
    fn rrl_slips_every_second_limited_response() {
        let l = Limiter::default();
        let cfg = RrlConfig {
            responses_per_second: 3,
            slip: 2,
            ..Default::default()
        };
        let v: Vec<RrlVerdict> = (0..7)
            .map(|_| l.rrl(&cfg, ip("192.0.2.10"), "victim.example.", 255))
            .collect();
        use RrlVerdict::*;
        assert_eq!(v, [Send, Send, Send, Drop, Slip, Drop, Slip]);
        assert_eq!(
            l.rrl(&cfg, ip("192.0.2.10"), "other.example.", 1),
            Send,
            "different name has its own budget"
        );
    }
}
