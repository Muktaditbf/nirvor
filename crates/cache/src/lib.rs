//! TTL-aware DNS answer cache with negative caching (RFC 2308), serve-stale (RFC 8767) and prefetch hints.
//!
//! Entries live for `ttl + stale_window`. While `age < ttl` they are fresh; after that they are stale and
//! only returned by [`Cache::get_stale`], which the resolver uses when upstreams fail.

use hickory_proto::op::{Message, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use moka::Expiry;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// Cache settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheConfig {
    /// Maximum number of cached answers.
    pub max_entries: u64,
    /// TTLs below this are raised to it (seconds).
    pub min_ttl: u32,
    /// TTLs above this are lowered to it (seconds).
    pub max_ttl: u32,
    /// Cap for NXDOMAIN/NODATA TTLs (RFC 2308 §5 suggests 1–3 hours).
    pub negative_max_ttl: u32,
    /// Prefetch when this percentage of the TTL is left …
    pub prefetch_percent: u8,
    /// … and the entry was read at least this many times.
    pub prefetch_min_hits: u32,
    /// How long after expiry an answer may still be served stale (RFC 8767 suggests 1–3 days), seconds.
    pub stale_window: u64,
    /// Stale window while the resolver is isolated from international upstreams, seconds.
    pub isolation_stale_window: u64,
    /// Stale window for pinned domains, seconds.
    pub pinned_stale_window: u64,
    /// TTL put on stale answers (RFC 8767 §4 recommends 30 s).
    pub stale_answer_ttl: u32,
    /// When a stale answer exists, wait this long for a refresh before answering stale (RFC 8767 §5 suggests 1.8 s), ms.
    pub client_response_timer_ms: u64,
    /// After an upstream failure, serve stale without retrying for this long (RFC 8767 §5 suggests 30 s).
    pub failure_recheck_secs: u64,
    /// After an upstream failure with nothing stale, answer SERVFAIL without retrying for this long (RFC 2308 §7.1 allows up to 5 min).
    pub servfail_cache_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 200_000,
            min_ttl: 0,
            max_ttl: 86_400,
            negative_max_ttl: 3_600,
            prefetch_percent: 10,
            prefetch_min_hits: 3,
            stale_window: 86_400,
            isolation_stale_window: 7 * 86_400,
            pinned_stale_window: 30 * 86_400,
            stale_answer_ttl: 30,
            client_response_timer_ms: 1800,
            failure_recheck_secs: 30,
            servfail_cache_secs: 5,
        }
    }
}

/// What a cached answer is for. DO and CD change the answer, so they are part of the key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    /// Lower-cased query name.
    pub name: String,
    /// Query type.
    pub rtype: u16,
    /// Query class.
    pub class: u16,
    /// Client set the DNSSEC OK bit.
    pub dnssec_ok: bool,
    /// Client set Checking Disabled.
    pub checking_disabled: bool,
}

impl Key {
    /// Builds the key for a query.
    pub fn new(
        name: &Name,
        rtype: RecordType,
        class: DNSClass,
        dnssec_ok: bool,
        checking_disabled: bool,
    ) -> Self {
        Self {
            name: name.to_lowercase().to_ascii(),
            rtype: rtype.into(),
            class: class.into(),
            dnssec_ok,
            checking_disabled,
        }
    }
}

/// One cached answer.
#[derive(Debug)]
pub struct Entry {
    msg: Message,
    stored: Instant,
    ttl: u32,
    hits: AtomicU32,
    prefetching: AtomicBool,
    pinned: bool,
}

impl Entry {
    /// Seconds until the entry goes stale (0 when already stale).
    pub fn remaining(&self) -> u32 {
        self.ttl.saturating_sub(age_secs(self.stored))
    }

    /// TTL the entry was stored with.
    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    /// The stored answer with TTLs as originally received (after clamping).
    pub fn message(&self) -> &Message {
        &self.msg
    }
}

fn age_secs(since: Instant) -> u32 {
    u32::try_from(since.elapsed().as_secs()).unwrap_or(u32::MAX)
}

/// A cache read result.
#[derive(Debug)]
pub struct Hit {
    /// Answer with TTLs counted down (or set to the stale TTL), ID 0 and no OPT record.
    pub msg: Message,
    /// The caller should refresh this entry in the background.
    pub prefetch: bool,
}

struct StaleExpiry {
    /// Longest stale window for ordinary entries (normal or isolated).
    stale_window: Duration,
    pinned_window: Duration,
}

impl Expiry<Key, Arc<Entry>> for StaleExpiry {
    fn expire_after_create(&self, _k: &Key, v: &Arc<Entry>, _at: Instant) -> Option<Duration> {
        let window = if v.pinned {
            self.pinned_window
        } else {
            self.stale_window
        };
        // Entries imported from a snapshot were stored in the past; count their age.
        let life = Duration::from_secs(u64::from(v.ttl)) + window;
        Some(life.saturating_sub(v.stored.elapsed()))
    }
}

/// The answer cache. Cheap to clone; clones share storage.
#[derive(Clone)]
pub struct Cache {
    inner: moka::sync::Cache<Key, Arc<Entry>>,
    config: Arc<CacheConfig>,
    pinned: Arc<RwLock<HashSet<String>>>,
}

impl Cache {
    /// Creates an empty cache.
    pub fn new(config: CacheConfig) -> Self {
        let inner = moka::sync::Cache::builder()
            .max_capacity(config.max_entries)
            .expire_after(StaleExpiry {
                stale_window: Duration::from_secs(
                    config.stale_window.max(config.isolation_stale_window),
                ),
                pinned_window: Duration::from_secs(config.pinned_stale_window),
            })
            .build();
        Self {
            inner,
            config: Arc::new(config),
            pinned: Arc::default(),
        }
    }

    /// Sets the pinned names (any case, with or without the trailing dot). Their answers get the pinned stale window.
    pub fn set_pinned<I: IntoIterator<Item = String>>(&self, names: I) {
        let set = names
            .into_iter()
            .map(|n| {
                let n = n.to_ascii_lowercase();
                if n.ends_with('.') { n } else { format!("{n}.") }
            })
            .collect();
        *self.pinned.write().unwrap_or_else(|p| p.into_inner()) = set;
    }

    /// True when `name` (lower-case, fully qualified) is pinned.
    pub fn is_pinned(&self, name: &str) -> bool {
        self.pinned
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(name)
    }

    /// Fresh answer, if any. Marks it for prefetch when it is popular and close to expiry.
    pub fn get(&self, key: &Key) -> Option<Hit> {
        let entry = self.inner.get(key)?;
        let age = age_secs(entry.stored);
        if age >= entry.ttl {
            return None;
        }
        let hits = entry.hits.fetch_add(1, Ordering::Relaxed) + 1;
        // Milliseconds, so short TTLs (under 10 s) can still reach the prefetch window.
        let ttl_ms = u128::from(entry.ttl) * 1000;
        let left_ms = ttl_ms.saturating_sub(entry.stored.elapsed().as_millis());
        let near_expiry = left_ms * 100 <= ttl_ms * u128::from(self.config.prefetch_percent);
        let prefetch = near_expiry
            && hits >= self.config.prefetch_min_hits
            && !entry.prefetching.swap(true, Ordering::Relaxed);
        Some(Hit {
            msg: with_ttls(&entry.msg, |ttl| ttl.saturating_sub(age).max(1)),
            prefetch,
        })
    }

    /// (seconds left, TTL) for an entry without counting a hit; `None` when absent.
    pub fn remaining(&self, key: &Key) -> Option<(u32, u32)> {
        self.inner.get(key).map(|e| (e.remaining(), e.ttl))
    }

    /// Answer that may be expired, within the stale window: the pinned window for pinned names,
    /// the isolation window when `isolated`, the normal one otherwise. Expired answers get `stale_answer_ttl`.
    pub fn get_stale(&self, key: &Key, isolated: bool) -> Option<Message> {
        let entry = self.inner.get(key)?;
        let age = age_secs(entry.stored);
        if age < entry.ttl {
            return Some(with_ttls(&entry.msg, |ttl| ttl.saturating_sub(age).max(1)));
        }
        let c = &self.config;
        let window = if entry.pinned {
            c.pinned_stale_window
        } else if isolated {
            c.isolation_stale_window
        } else {
            c.stale_window
        };
        if u64::from(age) > u64::from(entry.ttl) + window {
            return None;
        }
        let stale_ttl = self.config.stale_answer_ttl;
        Some(with_ttls(&entry.msg, |_| stale_ttl))
    }

    /// Stores an upstream answer if it is cacheable. Returns the TTL used, or `None` if not cached.
    pub fn insert(&self, key: Key, msg: &Message) -> Option<u32> {
        let ttl = self.cache_ttl(msg)?;
        self.insert_with_ttl(key, msg, ttl, Instant::now());
        Some(ttl)
    }

    fn insert_with_ttl(&self, key: Key, msg: &Message, ttl: u32, stored: Instant) {
        let mut stored_msg = with_ttls(msg, |t| t.clamp(self.config.min_ttl, self.config.max_ttl));
        stored_msg.metadata.id = 0;
        stored_msg.edns = None;
        stored_msg.signature = None;
        let pinned = self.is_pinned(&key.name);
        self.inner.insert(
            key,
            Arc::new(Entry {
                msg: stored_msg,
                stored,
                ttl,
                hits: AtomicU32::new(0),
                prefetching: AtomicBool::new(false),
                pinned,
            }),
        );
    }

    /// TTL to cache `msg` for, or `None` if it must not be cached.
    pub fn cache_ttl(&self, msg: &Message) -> Option<u32> {
        let c = &self.config;
        if msg.metadata.truncation {
            return None;
        }
        let ttl = match msg.metadata.response_code {
            ResponseCode::NoError if !msg.answers.is_empty() => msg
                .answers
                .iter()
                .chain(&msg.authorities)
                .map(|r| r.ttl)
                .min()?
                .clamp(c.min_ttl, c.max_ttl),
            // NODATA or NXDOMAIN: RFC 2308 §5, TTL is min(SOA TTL, SOA MINIMUM); no SOA means do not cache.
            ResponseCode::NoError | ResponseCode::NXDomain => msg
                .authorities
                .iter()
                .find_map(|r| match &r.data {
                    RData::SOA(soa) => Some(r.ttl.min(soa.minimum)),
                    _ => None,
                })?
                .clamp(c.min_ttl, c.negative_max_ttl),
            _ => return None,
        };
        (ttl > 0).then_some(ttl)
    }

    /// Copy of `msg` with record TTLs clamped to `min_ttl..=max_ttl`, as clients see them from the cache.
    pub fn clamp_ttls(&self, msg: &Message) -> Message {
        with_ttls(msg, |t| t.clamp(self.config.min_ttl, self.config.max_ttl))
    }

    /// Drops one entry.
    pub fn remove(&self, key: &Key) {
        self.inner.invalidate(key);
    }

    /// Drops everything.
    pub fn clear(&self) {
        self.inner.invalidate_all();
    }

    /// Approximate number of entries.
    pub fn len(&self) -> u64 {
        self.inner.run_pending_tasks();
        self.inner.entry_count()
    }

    /// True when the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Settings in use.
    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    /// Clears the prefetch flag after a refresh attempt, so a failed refresh can be retried.
    pub fn prefetch_done(&self, key: &Key) {
        if let Some(e) = self.inner.get(key) {
            e.prefetching.store(false, Ordering::Relaxed);
        }
    }

    /// Exports entries for a disk snapshot: (key, wire answer, unix time stored, ttl).
    pub fn export(&self) -> Vec<SnapshotEntry> {
        let now_sys = SystemTime::now();
        self.inner
            .iter()
            .filter_map(|(k, e)| {
                let stored = now_sys.checked_sub(e.stored.elapsed())?;
                Some(SnapshotEntry {
                    key: (*k).clone(),
                    wire: e.msg.to_vec().ok()?,
                    stored_unix: stored
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .ok()?
                        .as_secs(),
                    ttl: e.ttl,
                })
            })
            .collect()
    }

    /// Imports snapshot entries, skipping ones past the stale window. Returns how many were loaded.
    pub fn import(&self, entries: Vec<SnapshotEntry>) -> usize {
        let now_unix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let now = Instant::now();
        let mut loaded = 0;
        for e in entries {
            let age = now_unix.saturating_sub(e.stored_unix);
            let window = self.config.pinned_stale_window.max(
                self.config
                    .stale_window
                    .max(self.config.isolation_stale_window),
            );
            if age >= u64::from(e.ttl) + window {
                continue;
            }
            let (Ok(msg), Some(stored)) = (
                Message::from_vec(&e.wire),
                now.checked_sub(Duration::from_secs(age)),
            ) else {
                continue;
            };
            self.insert_with_ttl(e.key, &msg, e.ttl, stored);
            loaded += 1;
        }
        loaded
    }
}

/// One cache entry in a disk snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Cache key.
    pub key: Key,
    /// Answer in wire format.
    pub wire: Vec<u8>,
    /// When it was stored, unix seconds.
    pub stored_unix: u64,
    /// Fresh lifetime in seconds.
    pub ttl: u32,
}

/// Copy of `msg` with every record TTL (OPT excluded, it has none here) mapped through `f`.
fn with_ttls(msg: &Message, f: impl Fn(u32) -> u32) -> Message {
    let mut m = msg.clone();
    for r in m
        .answers
        .iter_mut()
        .chain(m.authorities.iter_mut())
        .chain(m.additionals.iter_mut())
    {
        r.ttl = f(r.ttl);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::{OpCode, Query};
    use hickory_proto::rr::Record;
    use hickory_proto::rr::rdata::{A, SOA};
    use std::net::Ipv4Addr;

    fn name(s: &str) -> Name {
        Name::from_ascii(s).unwrap()
    }

    fn key(n: &str) -> Key {
        Key::new(&name(n), RecordType::A, DNSClass::IN, false, false)
    }

    fn answer(n: &str, ttl: u32) -> Message {
        let mut m = Message::response(42, OpCode::Query);
        m.add_query(Query::query(name(n), RecordType::A));
        m.add_answer(Record::from_rdata(
            name(n),
            ttl,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
        ));
        m
    }

    fn negative(n: &str, rcode: ResponseCode, soa_ttl: u32, minimum: u32) -> Message {
        let mut m = Message::response(1, OpCode::Query);
        m.metadata.response_code = rcode;
        m.add_query(Query::query(name(n), RecordType::A));
        let soa = SOA::new(
            name("ns.test."),
            name("admin.test."),
            1,
            3600,
            600,
            86400,
            minimum,
        );
        m.add_authority(Record::from_rdata(name("test."), soa_ttl, RData::SOA(soa)));
        m
    }

    #[test]
    fn stores_and_serves_with_id_cleared() {
        let c = Cache::new(CacheConfig::default());
        assert_eq!(c.insert(key("a.test."), &answer("a.test.", 300)), Some(300));
        let hit = c.get(&key("A.TEST.")).expect("case-insensitive hit");
        assert_eq!(hit.msg.metadata.id, 0);
        assert_eq!(hit.msg.answers[0].ttl, 300);
        assert!(!hit.prefetch);
    }

    #[test]
    fn clamps_ttl() {
        let c = Cache::new(CacheConfig {
            min_ttl: 60,
            max_ttl: 600,
            ..Default::default()
        });
        assert_eq!(c.insert(key("lo.test."), &answer("lo.test.", 5)), Some(60));
        assert_eq!(
            c.insert(key("hi.test."), &answer("hi.test.", 99_999)),
            Some(600)
        );
        assert_eq!(c.get(&key("hi.test.")).unwrap().msg.answers[0].ttl, 600);
    }

    #[test]
    fn negative_ttl_is_min_of_soa_ttl_and_minimum() {
        let c = Cache::new(CacheConfig::default());
        assert_eq!(
            c.insert(
                key("nx.test."),
                &negative("nx.test.", ResponseCode::NXDomain, 900, 300)
            ),
            Some(300)
        );
        assert_eq!(
            c.insert(
                key("nd.test."),
                &negative("nd.test.", ResponseCode::NoError, 120, 300)
            ),
            Some(120)
        );
        let mut no_soa = answer("x.test.", 60);
        no_soa.answers.clear();
        no_soa.metadata.response_code = ResponseCode::NXDomain;
        assert_eq!(
            c.insert(key("x.test."), &no_soa),
            None,
            "RFC 2308 §5: no SOA, no caching"
        );
    }

    #[test]
    fn negative_ttl_capped() {
        let c = Cache::new(CacheConfig::default());
        assert_eq!(
            c.insert(
                key("nx.test."),
                &negative("nx.test.", ResponseCode::NXDomain, 999_999, 999_999)
            ),
            Some(3600)
        );
    }

    #[test]
    fn does_not_cache_errors_truncation_or_zero_ttl() {
        let c = Cache::new(CacheConfig::default());
        let mut sf = answer("s.test.", 60);
        sf.metadata.response_code = ResponseCode::ServFail;
        assert_eq!(c.insert(key("s.test."), &sf), None);
        let mut tc = answer("t.test.", 60);
        tc.metadata.truncation = true;
        assert_eq!(c.insert(key("t.test."), &tc), None);
        assert_eq!(c.insert(key("z.test."), &answer("z.test.", 0)), None);
    }

    #[test]
    fn stale_after_expiry_with_stale_ttl() {
        let c = Cache::new(CacheConfig::default());
        let past = Instant::now() - Duration::from_secs(120);
        c.insert_with_ttl(key("old.test."), &answer("old.test.", 60), 60, past);
        assert!(
            c.get(&key("old.test.")).is_none(),
            "expired entry is not fresh"
        );
        let stale = c
            .get_stale(&key("old.test."), false)
            .expect("stale copy kept");
        assert_eq!(stale.answers[0].ttl, 30);
    }

    #[test]
    fn stale_windows_normal_isolated_and_pinned() {
        let c = Cache::new(CacheConfig::default());
        c.set_pinned(["Pinned.Test".to_string()]);
        let two_days_ago = Instant::now() - Duration::from_secs(2 * 86_400);
        c.insert_with_ttl(key("old.test."), &answer("old.test.", 60), 60, two_days_ago);
        c.insert_with_ttl(
            key("pinned.test."),
            &answer("pinned.test.", 60),
            60,
            two_days_ago,
        );
        assert!(
            c.get_stale(&key("old.test."), false).is_none(),
            "past the 1-day normal window"
        );
        assert!(
            c.get_stale(&key("old.test."), true).is_some(),
            "inside the 7-day isolation window"
        );
        assert!(
            c.get_stale(&key("pinned.test."), false).is_some(),
            "pinned: 30-day window"
        );
    }

    #[test]
    fn ttl_counts_down() {
        let c = Cache::new(CacheConfig::default());
        let past = Instant::now() - Duration::from_secs(100);
        c.insert_with_ttl(key("d.test."), &answer("d.test.", 300), 300, past);
        let ttl = c.get(&key("d.test.")).unwrap().msg.answers[0].ttl;
        assert!((199..=200).contains(&ttl), "ttl was {ttl}");
    }

    #[test]
    fn prefetch_flag_once_when_popular_and_near_expiry() {
        let c = Cache::new(CacheConfig::default());
        let past = Instant::now() - Duration::from_secs(95);
        c.insert_with_ttl(key("p.test."), &answer("p.test.", 100), 100, past);
        let flags: Vec<bool> = (0..5)
            .map(|_| c.get(&key("p.test.")).unwrap().prefetch)
            .collect();
        assert_eq!(flags, [false, false, true, false, false]);
        c.prefetch_done(&key("p.test."));
        assert!(c.get(&key("p.test.")).unwrap().prefetch);
    }

    #[test]
    fn prefetch_works_for_short_ttls() {
        let c = Cache::new(CacheConfig::default());
        let past = Instant::now() - Duration::from_millis(4_700);
        c.insert_with_ttl(key("short.test."), &answer("short.test.", 5), 5, past);
        let flags: Vec<bool> = (0..3)
            .map(|_| c.get(&key("short.test.")).unwrap().prefetch)
            .collect();
        assert_eq!(flags, [false, false, true]);
    }

    #[test]
    fn dnssec_ok_is_a_separate_entry() {
        let c = Cache::new(CacheConfig::default());
        c.insert(key("a.test."), &answer("a.test.", 300));
        let do_key = Key::new(&name("a.test."), RecordType::A, DNSClass::IN, true, false);
        assert!(c.get(&do_key).is_none());
    }

    #[test]
    fn snapshot_round_trip_keeps_age() {
        let c = Cache::new(CacheConfig::default());
        let past = Instant::now() - Duration::from_secs(50);
        c.insert_with_ttl(key("s.test."), &answer("s.test.", 300), 300, past);
        let snap = c.export();
        let c2 = Cache::new(CacheConfig::default());
        assert_eq!(c2.import(snap), 1);
        let ttl = c2.get(&key("s.test.")).unwrap().msg.answers[0].ttl;
        assert!((249..=250).contains(&ttl), "ttl was {ttl}");
    }

    #[test]
    fn snapshot_skips_entries_past_stale_window() {
        let c = Cache::new(CacheConfig::default());
        let entry = SnapshotEntry {
            key: key("gone.test."),
            wire: answer("gone.test.", 60).to_vec().unwrap(),
            stored_unix: 1,
            ttl: 60,
        };
        assert_eq!(c.import(vec![entry]), 0);
    }
}
