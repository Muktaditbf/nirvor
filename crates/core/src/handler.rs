//! The query pipeline: one client packet in, one reply out.

use crate::config::Config;
use crate::metrics::Metrics;
use crate::mode::{Control, Mode, ModeState, Monitor, next_mode};
use crate::querylog::{QueryLog, Source};
use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA};
use hickory_proto::rr::{DNSClass, Name, RecordType};
use hickory_proto::rr::{RData, Record};
use nirvor_cache::{Cache, Key};
use nirvor_policy::{
    BlockResponse, BlocklistManager, DenyAction, Limiter, PolicyConfig, RrlVerdict, acl_allows,
};
use nirvor_resilience::{Recursor, RootManager, snapshot};
use nirvor_storage::Storage;
use nirvor_upstream::{Pool, UpstreamSpec};
use nirvor_zones::LocalZones;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// DNS header length (RFC 1035 §4.1.1).
const HEADER_LEN: usize = 12;

/// How the query reached us. UDP answers must fit the client's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Plain UDP.
    Udp,
    /// TCP with a 2-byte length prefix.
    Tcp,
}

/// Per-query context.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    /// UDP or TCP.
    pub transport: Transport,
    /// Client address.
    pub client: IpAddr,
    /// When the query arrived.
    pub start: Instant,
}

/// Everything needed to answer queries. Shared behind an `Arc`.
pub struct Resolver {
    /// Upstream pool.
    pub upstreams: Pool,
    /// Recent upstream failures per key (RFC 8767 failure recheck timer).
    pub failures: moka::sync::Cache<Key, Instant>,
    /// Answer cache.
    pub cache: Cache,
    /// Counters and histograms.
    pub metrics: Arc<Metrics>,
    /// Upstream queries in progress, for coalescing duplicates.
    pub inflight: Mutex<HashMap<Key, broadcast::Sender<Result<Message, String>>>>,
    /// Largest UDP reply we send, whatever the client offers (DNS Flag Day 2020: 1232).
    pub max_udp_payload: u16,
    /// Static local zones.
    pub zones: RwLock<Arc<LocalZones>>,
    /// Local root zone copy (RFC 8806).
    pub root: RootManager,
    /// Iterative resolution from the local root, used when forwarders fail.
    pub recursor: Recursor,
    /// Settings in effect (pinned list, data directory, …).
    pub config: RwLock<Arc<Config>>,
    /// Audit log, settings and history.
    pub storage: Storage,
    /// Zones answered only in isolation (connectivity-check overrides).
    pub overrides: RwLock<Arc<LocalZones>>,
    /// Operating mode.
    pub mode: RwLock<ModeState>,
    monitor: Mutex<Monitor>,
    /// Rate limiters.
    pub limiter: Limiter,
    /// Blocklists.
    pub blocklists: BlocklistManager,
    /// Recent queries and top lists.
    pub querylog: QueryLog,
}

impl Resolver {
    /// Builds the resolver from a validated config and loads the cache snapshot, if any.
    pub fn new(config: &Config, storage: Storage) -> Result<Arc<Self>, String> {
        let r = &config.resilience;
        let root = RootManager::new(r.root_zone.clone(), &r.data_dir);
        let cache = Cache::new(config.cache.clone());
        cache.set_pinned(r.pinned_domains.iter().cloned());
        let snap = r.data_dir.join("cache.snapshot");
        match snapshot::load(&cache, &snap) {
            Ok(n) => tracing::info!(entries = n, "cache snapshot loaded"),
            Err(e) if snap.exists() => warn!(error = %e, "cache snapshot not loaded"),
            Err(_) => {}
        }
        Ok(Arc::new(Self {
            upstreams: Pool::new(config.upstream.clone()),
            failures: moka::sync::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(
                    config
                        .cache
                        .failure_recheck_secs
                        .max(config.cache.servfail_cache_secs),
                ))
                .build(),
            cache,
            metrics: Arc::new(Metrics::default()),
            inflight: Mutex::default(),
            max_udp_payload: config.server.max_udp_payload,
            zones: RwLock::new(Arc::new(LocalZones::new(&config.zones)?)),
            recursor: Recursor::new(root.clone(), Duration::from_millis(r.recursion_timeout_ms)),
            root,
            config: RwLock::new(Arc::new(config.clone())),
            storage,
            overrides: RwLock::new(Arc::new(LocalZones::new(&config.isolation.overrides)?)),
            mode: RwLock::new(ModeState {
                mode: Mode::Normal,
                control: Control::Auto,
                since: now_unix(),
                reason: "starting".into(),
                international_healthy: 0,
                international_total: 0,
            }),
            monitor: Mutex::default(),
            limiter: Limiter::default(),
            blocklists: BlocklistManager::default(),
            querylog: QueryLog::default(),
        }))
    }

    /// Applies a new config at runtime. Returns the sections that only take effect after a restart.
    pub async fn apply_config(&self, new: Config, actor: &str) -> Result<Vec<String>, String> {
        new.validate()?;
        let old = self.settings();
        let section = |c: &Config, name: &str| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.get(name).cloned())
        };
        let mut changed = Vec::new();
        for name in [
            "server",
            "upstream",
            "limits",
            "cache",
            "api",
            "zones",
            "resilience",
            "isolation",
            "policy",
            "logging",
        ] {
            if section(&old, name) != section(&new, name) {
                changed.push(name.to_string());
            }
        }
        let mut restart = Vec::new();
        for name in ["server", "cache", "api"] {
            if changed.iter().any(|c| c == name) {
                restart.push(name.to_string());
            }
        }
        if old.resilience.data_dir != new.resilience.data_dir
            || old.resilience.root_zone.enabled != new.resilience.root_zone.enabled
            || old.resilience.root_zone.sources != new.resilience.root_zone.sources
        {
            restart.push("resilience.data_dir/root_zone".into());
        }
        if old.limits.max_inflight_udp != new.limits.max_inflight_udp
            || old.limits.max_tcp_connections != new.limits.max_tcp_connections
            || old.limits.tcp_idle_timeout_ms != new.limits.tcp_idle_timeout_ms
        {
            restart.push("limits (connection limits)".into());
        }

        let zones = LocalZones::new(&new.zones)?;
        let overrides = LocalZones::new(&new.isolation.overrides)?;
        self.upstreams.set_config(new.upstream.clone());
        *self.zones.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(zones);
        *self.overrides.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(overrides);
        self.cache
            .set_pinned(new.resilience.pinned_domains.iter().cloned());
        let lists_changed = old.policy.blocklists != new.policy.blocklists;
        if old.logging.query_log != new.logging.query_log {
            self.querylog.clear(); // do not keep data collected under a looser privacy setting
        }
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(new.clone());
        if lists_changed {
            self.blocklists
                .set_sources(new.policy.blocklists.clone())
                .await;
        }
        if !changed.is_empty() {
            self.audit(
                actor,
                "config_update",
                &format!("changed: {}", changed.join(", ")),
            )
            .await;
        }
        Ok(restart)
    }

    /// Writes one row of per-minute history every minute and prunes old rows daily.
    pub async fn run_history(self: Arc<Self>) {
        let mut last = self.metrics.totals();
        let mut ticks: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let summary = self.metrics.summary();
            let now = summary.totals.clone();
            let row = nirvor_storage::MinuteStat {
                ts: (now_unix() / 60 * 60) as i64 - 60,
                queries: (now.queries - last.queries) as i64,
                hits: (now.cache_hits - last.cache_hits) as i64,
                misses: (now.cache_misses - last.cache_misses) as i64,
                blocked: (now.blocked - last.blocked) as i64,
                servfail: (now.servfail - last.servfail) as i64,
                p50: summary.p50,
                p95: summary.p95,
                p99: summary.p99,
            };
            last = now;
            if let Err(e) = self.storage.record_minute(&row).await {
                warn!(error = %e, "could not store traffic history");
            }
            ticks += 1;
            if ticks % 1440 == 1 {
                let l = self.settings().logging.clone();
                if let Err(e) = self.storage.prune(l.history_days, l.audit_days).await {
                    warn!(error = %e, "could not prune history");
                }
            }
        }
    }

    /// True while in isolation mode.
    pub fn isolated(&self) -> bool {
        self.mode.read().unwrap_or_else(|p| p.into_inner()).mode == Mode::Isolation
    }

    /// Current mode state.
    pub fn mode_state(&self) -> ModeState {
        self.mode.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Sets who controls the mode and re-evaluates at once. Logged to the audit log.
    pub async fn set_control(&self, control: Control, actor: &str) {
        let before = {
            let mut m = self.mode.write().unwrap_or_else(|p| p.into_inner());
            let before = m.control;
            m.control = control;
            before
        };
        if before != control {
            self.audit(actor, "mode_control", &format!("{before:?} → {control:?}"))
                .await;
        }
        self.evaluate_mode(actor).await;
    }

    async fn audit(&self, actor: &str, action: &str, detail: &str) {
        if let Err(e) = self.storage.audit(actor, action, detail).await {
            warn!(error = %e, "could not write audit log");
        }
    }

    /// One mode check: applies the operator's choice or the automatic rule, and logs changes.
    async fn evaluate_mode(&self, actor: &str) {
        let (healthy, total) = self
            .upstreams
            .health_count(&|s: &UpstreamSpec| s.international);
        let cfg = self.settings().isolation.clone();
        let change = {
            let mut m = self.mode.write().unwrap_or_else(|p| p.into_inner());
            let (auto_mode, auto_reason) = {
                let mut mon = self.monitor.lock().unwrap_or_else(|p| p.into_inner());
                next_mode(m.mode, healthy, total, &mut mon, Instant::now(), &cfg)
            };
            let (mode, reason) = match m.control {
                Control::Auto => (auto_mode, auto_reason),
                Control::Normal => (Mode::Normal, "forced by operator".to_string()),
                Control::Isolation => (Mode::Isolation, "forced by operator".to_string()),
            };
            m.international_healthy = healthy;
            m.international_total = total;
            m.reason = reason.clone();
            if m.mode == mode {
                None
            } else {
                let from = m.mode;
                m.mode = mode;
                m.since = now_unix();
                Some((from, mode, reason))
            }
        };
        if let Some((from, to, reason)) = change {
            warn!(?from, ?to, %reason, "mode changed");
            let who = if actor == "monitor" { "system" } else { actor };
            self.audit(who, "mode_change", &format!("{from:?} → {to:?}: {reason}"))
                .await;
        }
    }

    /// Re-checks the mode every `check_interval_secs`.
    pub async fn run_mode_monitor(self: Arc<Self>) {
        // Unmeasured upstreams look unhealthy; wait for the first health round (bounded) so a
        // restart does not log a false mode change.
        for _ in 0..50 {
            let measured = self
                .upstreams
                .status()
                .iter()
                .all(|u| !u.spec.enabled || u.srtt_ms.is_some() || u.failures > 0);
            if measured {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        loop {
            self.evaluate_mode("monitor").await;
            let every = self.settings().isolation.check_interval_secs.max(1);
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    }

    /// Current settings.
    pub fn settings(&self) -> Arc<Config> {
        self.config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Writes the cache snapshot to the data directory. Returns the number of entries saved.
    pub fn save_snapshot(&self) -> std::io::Result<usize> {
        let path = self.settings().resilience.data_dir.join("cache.snapshot");
        snapshot::save(&self.cache, &path)
    }

    /// Saves the cache snapshot every `snapshot_interval_secs`.
    pub async fn run_snapshots(self: Arc<Self>) {
        loop {
            let every = self.settings().resilience.snapshot_interval_secs.max(30);
            tokio::time::sleep(Duration::from_secs(every)).await;
            let this = self.clone();
            match tokio::task::spawn_blocking(move || this.save_snapshot()).await {
                Ok(Ok(n)) => debug!(entries = n, "cache snapshot saved"),
                Ok(Err(e)) => warn!(error = %e, "cache snapshot failed"),
                Err(e) => warn!(error = %e, "cache snapshot task failed"),
            }
        }
    }

    /// Keeps pinned domains (A and AAAA) in cache, refreshing them before they expire.
    pub async fn run_pinned(self: Arc<Self>) {
        loop {
            let settings = self.settings();
            for domain in &settings.resilience.pinned_domains {
                let Ok(mut name) = Name::from_ascii(domain) else {
                    continue;
                };
                name.set_fqdn(true); // config entries may omit the trailing dot
                for t in [RecordType::A, RecordType::AAAA] {
                    let key = Key::new(&name, t, DNSClass::IN, false, false);
                    let due = self
                        .cache
                        .remaining(&key)
                        .is_none_or(|(left, ttl)| left.saturating_mul(4) < ttl);
                    if due
                        && let Err(e) = self
                            .fetch(&key, &Query::query(name.clone(), t), false, false)
                            .await
                    {
                        debug!(domain, error = %e, "pinned refresh failed");
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(
                settings.resilience.pinned_refresh_secs.max(10),
            ))
            .await;
        }
    }
    /// Handles one raw query. `None` means send nothing back (too short to answer, or not a query).
    pub async fn handle(
        self: &Arc<Self>,
        packet: &[u8],
        transport: Transport,
        client: IpAddr,
    ) -> Option<Vec<u8>> {
        let start = Instant::now();
        let ctx = Ctx {
            transport,
            client,
            start,
        };
        let limit = Duration::from_millis(self.settings().limits.query_timeout_ms);
        let reply = match tokio::time::timeout(limit, self.handle_inner(packet, ctx)).await {
            Ok(r) => r,
            Err(_) => {
                debug!(%client, "query timed out");
                Message::from_vec(packet).ok().and_then(|q| {
                    self.finish(&q, error_reply(ResponseCode::ServFail), ctx, Source::Error)
                })
            }
        };
        if reply.is_some() {
            self.metrics.latency.observe(start.elapsed());
        }
        reply
    }

    async fn handle_inner(self: &Arc<Self>, packet: &[u8], ctx: Ctx) -> Option<Vec<u8>> {
        let settings = self.settings();
        if packet.len() < HEADER_LEN
            || packet[2] & 0x80 != 0
            || packet.len() > settings.limits.max_query_size
        {
            return None; // no usable ID, a response (replying could loop), or oversized
        }
        let policy = &settings.policy;
        if !acl_allows(&policy.allow, ctx.client) {
            Metrics::inc(&self.metrics.refused);
            return match policy.deny_action {
                DenyAction::Drop => None,
                DenyAction::Refuse => Some(error_raw(packet, ResponseCode::Refused)),
            };
        }
        if !self.limiter.allow_query(&policy.rate_limit, ctx.client) {
            Metrics::inc(&self.metrics.rate_limited);
            return None;
        }
        Metrics::inc(&self.metrics.queries);
        if ctx.transport == Transport::Tcp {
            Metrics::inc(&self.metrics.tcp_queries);
        }
        let query = match Message::from_vec(packet) {
            Ok(q) => q,
            Err(e) => {
                debug!(error = %e, "malformed query");
                Metrics::inc(&self.metrics.formerr);
                return Some(error_raw(packet, ResponseCode::FormErr));
            }
        };
        if query.metadata.op_code != OpCode::Query {
            let reply = Message::error_msg(
                query.metadata.id,
                query.metadata.op_code,
                ResponseCode::NotImp,
            );
            return encode(&reply);
        }
        if query.queries.len() != 1 {
            Metrics::inc(&self.metrics.formerr);
            return self.finish(
                &query,
                error_reply(ResponseCode::FormErr),
                ctx,
                Source::Error,
            );
        }

        let q = &query.queries[0];
        let dnssec_ok = query.edns.as_ref().is_some_and(|e| e.flags().dnssec_ok);
        let cd = query.metadata.checking_disabled;

        if self.blocklists.current().is_blocked(&q.name().to_ascii()) {
            Metrics::inc(&self.metrics.blocked);
            return self.finish(&query, blocked_reply(policy, q), ctx, Source::Blocked);
        }

        let isolated = self.isolated();
        if isolated {
            let overrides = self
                .overrides
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(answer) = overrides.lookup(q.name(), q.query_type()) {
                Metrics::inc(&self.metrics.local_answers);
                return self.finish(&query, answer, ctx, Source::Local);
            }
        }
        let local = self.zones.read().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(answer) = local.lookup(q.name(), q.query_type()) {
            Metrics::inc(&self.metrics.local_answers);
            return self.finish(&query, answer, ctx, Source::Local);
        }
        if let Some(root) = self.root.zone() {
            if q.name().is_root()
                && let Some(answer) = root.apex_answer(q.query_type(), dnssec_ok)
            {
                Metrics::inc(&self.metrics.local_answers);
                return self.finish(&query, answer, ctx, Source::Local);
            }
            // RFC 8806: names under a TLD that does not exist get NXDOMAIN locally. DNSSEC clients
            // need the NSEC proof, so they still go upstream.
            if !dnssec_ok && root.tld_missing(q.name()) {
                Metrics::inc(&self.metrics.local_answers);
                return self.finish(&query, root.nxdomain(), ctx, Source::Local);
            }
        }

        let key = Key::new(q.name(), q.query_type(), q.query_class(), dnssec_ok, cd);

        if let Some(hit) = self.cache.get(&key) {
            Metrics::inc(&self.metrics.cache_hits);
            if hit.prefetch {
                self.spawn_prefetch(key, q.clone(), dnssec_ok, cd);
            }
            return self.finish(&query, hit.msg, ctx, Source::Cache);
        }
        Metrics::inc(&self.metrics.cache_misses);

        let stale = self.cache.get_stale(&key, isolated);
        let cfg = self.cache.config();
        // RFC 8767 §5 failure recheck: after a failure, answer stale (or SERVFAIL) without asking again for a while.
        if let Some(failed_at) = self.failures.get(&key) {
            if let Some(st) = stale {
                Metrics::inc(&self.metrics.stale_served);
                return self.finish(&query, st, ctx, Source::Stale);
            }
            if failed_at.elapsed() < Duration::from_secs(cfg.servfail_cache_secs) {
                return self.finish(
                    &query,
                    error_reply(ResponseCode::ServFail),
                    ctx,
                    Source::Error,
                );
            }
        }

        let Some(st) = stale else {
            return match self.fetch(&key, q, dnssec_ok, cd).await {
                Ok(answer) => self.finish(&query, answer, ctx, Source::Upstream),
                Err(_) => self.finish(
                    &query,
                    error_reply(ResponseCode::ServFail),
                    ctx,
                    Source::Error,
                ),
            };
        };
        // RFC 8767 §5 client response timer: if the refresh is slow, answer stale and let it finish in the background.
        let this = self.clone();
        let (k, q2) = (key.clone(), q.clone());
        let mut refresh = tokio::spawn(async move { this.fetch(&k, &q2, dnssec_ok, cd).await });
        let timer = Duration::from_millis(cfg.client_response_timer_ms);
        tokio::select! {
            r = &mut refresh => match r {
                Ok(Ok(answer)) => self.finish(&query, answer, ctx, Source::Upstream),
                _ => {
                    Metrics::inc(&self.metrics.stale_served);
                    self.finish(&query, st, ctx, Source::Stale)
                }
            },
            _ = tokio::time::sleep(timer) => {
                Metrics::inc(&self.metrics.stale_served);
                self.finish(&query, st, ctx, Source::Stale)
            }
        }
    }

    /// Asks upstream and caches the answer. Concurrent calls for the same key share one upstream
    /// query, so a popular name expiring does not send a burst of identical queries upstream.
    async fn fetch(
        &self,
        key: &Key,
        q: &Query,
        dnssec_ok: bool,
        cd: bool,
    ) -> Result<Message, String> {
        let waiter = {
            let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
            match inflight.get(key) {
                Some(tx) => Some(tx.subscribe()),
                None => {
                    inflight.insert(key.clone(), broadcast::channel(1).0);
                    None
                }
            }
        };
        if let Some(mut rx) = waiter {
            return rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("shared upstream query was cancelled".into()));
        }
        let isolated = self.isolated();
        // Removes the in-flight entry even if this future is dropped, so waiters are released.
        let guard = InflightGuard {
            resolver: self,
            key,
        };
        let mut result = self
            .upstreams
            // In isolation only local upstreams are tried; international ones would just time out.
            .forward(&upstream_query(q, dnssec_ok, cd), &|s: &UpstreamSpec| {
                !isolated || !s.international
            })
            .await
            .map_err(|e| e.to_string());
        if result.is_err() && self.recursor.ready() {
            // Forwarders failed: resolve from the local root ourselves (not DNSSEC-validated).
            match self.recursor.resolve(q).await {
                Ok(answer) => {
                    Metrics::inc(&self.metrics.recursive_answers);
                    result = Ok(answer);
                }
                Err(e) => debug!(error = %e, name = %q.name(), "iterative resolution failed"),
            }
        }
        match &mut result {
            Ok(answer) => {
                self.cache.insert(key.clone(), answer);
                self.failures.invalidate(key);
                *answer = self.cache.clamp_ttls(answer);
            }
            Err(e) => {
                Metrics::inc(&self.metrics.upstream_errors);
                warn!(error = %e, name = %q.name(), "all upstreams failed");
                self.failures.insert(key.clone(), Instant::now());
            }
        }
        if let Some(tx) = guard.take() {
            let _ = tx.send(result.clone());
        }
        result
    }

    fn spawn_prefetch(self: &Arc<Self>, key: Key, q: Query, dnssec_ok: bool, cd: bool) {
        Metrics::inc(&self.metrics.prefetches);
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.fetch(&key, &q, dnssec_ok, cd).await {
                debug!(error = %e, name = %q.name(), "prefetch failed");
            }
            this.cache.prefetch_done(&key);
        });
    }

    /// Turns an answer into the client's reply: their ID, question and RD bit, our OPT if they used EDNS,
    /// and truncation if it does not fit their UDP buffer (RFC 7766 §5).
    fn finish(
        &self,
        query: &Message,
        mut answer: Message,
        ctx: Ctx,
        src: Source,
    ) -> Option<Vec<u8>> {
        answer.metadata.id = query.metadata.id;
        answer.metadata.message_type = MessageType::Response;
        answer.metadata.op_code = query.metadata.op_code;
        answer.metadata.recursion_desired = query.metadata.recursion_desired;
        answer.metadata.recursion_available = true;
        answer.queries = query.queries.clone();
        answer.signature = None;
        answer.edns = query.edns.as_ref().map(|qe| {
            let mut e = Edns::new();
            e.set_max_payload(self.max_udp_payload);
            e.set_dnssec_ok(qe.flags().dnssec_ok);
            e
        });
        match answer.metadata.response_code {
            ResponseCode::NXDomain => Metrics::inc(&self.metrics.nxdomain),
            ResponseCode::ServFail => Metrics::inc(&self.metrics.servfail),
            _ => {}
        }
        if ctx.transport == Transport::Udp
            && let Some(q) = query.queries.first()
        {
            let rrl = &self.settings().policy.rrl;
            match self
                .limiter
                .rrl(rrl, ctx.client, &q.name().to_ascii(), q.query_type().into())
            {
                RrlVerdict::Send => {}
                RrlVerdict::Drop => {
                    Metrics::inc(&self.metrics.rate_limited);
                    return None;
                }
                RrlVerdict::Slip => {
                    Metrics::inc(&self.metrics.rate_limited);
                    return encode(&answer.truncate());
                }
            }
        }
        let bytes = encode(&answer)?;
        if let Some(q) = query.queries.first() {
            let settings = self.settings();
            self.querylog.record(
                &settings.logging,
                ctx.client,
                &q.name().to_ascii(),
                &q.query_type().to_string(),
                &answer.metadata.response_code.to_string(),
                ctx.start.elapsed().as_secs_f64() * 1000.0,
                src,
                ctx.transport == Transport::Tcp,
            );
        }
        let limit = usize::from(query.max_payload().min(self.max_udp_payload.max(512)));
        if ctx.transport == Transport::Udp && bytes.len() > limit {
            return encode(&answer.truncate()).filter(|b| b.len() <= limit);
        }
        Some(bytes)
    }
}

struct InflightGuard<'a> {
    resolver: &'a Resolver,
    key: &'a Key,
}

impl InflightGuard<'_> {
    fn take(self) -> Option<broadcast::Sender<Result<Message, String>>> {
        let tx = self.remove();
        std::mem::forget(self);
        tx
    }

    fn remove(&self) -> Option<broadcast::Sender<Result<Message, String>>> {
        let mut inflight = self
            .resolver
            .inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        inflight.remove(self.key)
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.remove();
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The query we send upstream: only the question, RD=1, our own EDNS (no client cookies or ECS).
pub fn upstream_query(q: &Query, dnssec_ok: bool, cd: bool) -> Message {
    let mut m = Message::query();
    m.metadata.recursion_desired = true;
    m.metadata.checking_disabled = cd;
    m.add_query(q.clone());
    let mut e = Edns::new();
    e.set_max_payload(hickory_proto::op::DEFAULT_MAX_PAYLOAD_LEN);
    e.set_dnssec_ok(dnssec_ok);
    m.set_edns(e);
    m
}

/// Empty answer with an error code; `finish` fills in the rest.
fn error_reply(code: ResponseCode) -> Message {
    Message::error_msg(0, OpCode::Query, code)
}

/// Error reply built from the raw header only (for queries we cannot or will not parse): echoes the ID,
/// opcode and RD bit, with no sections (RFC 1035 §4.1.1).
fn error_raw(packet: &[u8], code: ResponseCode) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[0..2].copy_from_slice(&packet[0..2]);
    out[2] = 0x80 | (packet[2] & 0x79); // QR=1, keep opcode and RD
    out[3] = 0x80 | (u16::from(code) as u8 & 0x0f); // RA=1, RCODE
    out
}

/// Answer for a blocked name, per `policy.block_response`.
fn blocked_reply(policy: &PolicyConfig, q: &Query) -> Message {
    let mut m = Message::response(0, OpCode::Query);
    let ttl = policy.block_ttl;
    let name = q.name().clone();
    let a = |ip: Ipv4Addr| Record::from_rdata(name.clone(), ttl, RData::A(A(ip)));
    let aaaa = |ip: Ipv6Addr| Record::from_rdata(name.clone(), ttl, RData::AAAA(AAAA(ip)));
    match (policy.block_response, q.query_type()) {
        (BlockResponse::Nxdomain, _) => m.metadata.response_code = ResponseCode::NXDomain,
        (BlockResponse::Refused, _) => m.metadata.response_code = ResponseCode::Refused,
        (BlockResponse::Null, RecordType::A) => {
            m.add_answer(a(Ipv4Addr::UNSPECIFIED));
        }
        (BlockResponse::Null, RecordType::AAAA) => {
            m.add_answer(aaaa(Ipv6Addr::UNSPECIFIED));
        }
        (BlockResponse::Redirect, RecordType::A) => {
            if let Some(ip) = policy.redirect_ipv4 {
                m.add_answer(a(ip));
            }
        }
        (BlockResponse::Redirect, RecordType::AAAA) => {
            if let Some(ip) = policy.redirect_ipv6 {
                m.add_answer(aaaa(ip));
            }
        }
        _ => {} // other types: empty NOERROR
    }
    m
}

fn encode(msg: &Message) -> Option<Vec<u8>> {
    match msg.to_vec() {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(error = %e, "could not encode reply");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formerr_echoes_id_and_opcode() {
        let packet = [
            0xab, 0xcd, 0x01, 0x00, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0xde, 0xad,
        ];
        let out = error_raw(&packet, ResponseCode::FormErr);
        let msg = Message::from_vec(&out).expect("valid header");
        assert_eq!(msg.metadata.id, 0xabcd);
        assert_eq!(msg.metadata.response_code, ResponseCode::FormErr);
        assert!(msg.metadata.recursion_desired);
        assert_eq!(msg.metadata.message_type, MessageType::Response);
    }
}
