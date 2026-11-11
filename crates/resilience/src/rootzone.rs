//! Local copy of the root zone (RFC 8806): download, verification (DNSSEC + ZONEMD, RFC 8976),
//! storage on disk, and lookups used by the resolver.

use crate::zonefile::{self, ZONEMD};
use hickory_proto::dnssec::rdata::DNSSECRData;
use hickory_proto::dnssec::{TrustAnchors, Verifier};
use hickory_proto::op::{Message, OpCode, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::{BinEncodable, BinEncoder, NameEncoding};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Root zone settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RootZoneConfig {
    /// Keep and use a local root zone copy.
    pub enabled: bool,
    /// Where to download `root.zone` from, tried in order.
    pub sources: Vec<String>,
    /// Seconds between refresh attempts (the root SOA refresh value is 1800).
    pub refresh_secs: u64,
}

impl Default for RootZoneConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sources: vec!["https://www.internic.net/domain/root.zone".into()],
            refresh_secs: 3600,
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A verified root zone.
pub struct RootZone {
    /// SOA serial.
    pub serial: u32,
    /// SOA record, for negative answers.
    soa: Record,
    /// SOA expire field: stop using the copy this long after the last successful refresh.
    pub expire_secs: u32,
    /// Records grouped by lower-cased owner name.
    by_name: HashMap<Name, Vec<Record>>,
    /// Lower-cased top-level labels that exist.
    tlds: HashSet<String>,
    /// Number of records.
    pub record_count: usize,
}

/// Why a zone failed verification.
pub type VerifyError = String;

impl RootZone {
    /// Parses and verifies zone text. `now` is unix seconds, for signature validity.
    pub fn from_text(text: &str, now: u64) -> Result<Self, VerifyError> {
        let records = zonefile::parse(text)?;
        verify(&records, now)?;
        Self::index(records)
    }

    fn index(records: Vec<Record>) -> Result<Self, VerifyError> {
        let soa = records
            .iter()
            .find(|r| r.name.is_root() && r.record_type() == RecordType::SOA)
            .cloned()
            .ok_or("no SOA at the apex")?;
        let RData::SOA(s) = &soa.data else {
            return Err("SOA record has no SOA data".into());
        };
        let (serial, expire) = (s.serial, u32::try_from(s.expire).unwrap_or(604_800));
        let record_count = records.len();
        let mut by_name: HashMap<Name, Vec<Record>> = HashMap::new();
        let mut tlds = HashSet::new();
        for r in records {
            let owner = r.name.to_lowercase();
            if owner.num_labels() == 1 && r.record_type() == RecordType::NS {
                tlds.insert(owner.to_ascii());
            }
            by_name.entry(owner).or_default().push(r);
        }
        Ok(Self {
            serial,
            soa,
            expire_secs: expire,
            by_name,
            tlds,
            record_count,
        })
    }

    /// Number of top-level domains.
    pub fn tld_count(&self) -> usize {
        self.tlds.len()
    }

    /// True if the top-level label of `name` does not exist in the root zone.
    pub fn tld_missing(&self, name: &Name) -> bool {
        if name.is_root() {
            return false;
        }
        let tld = name.trim_to(1).to_lowercase().to_ascii();
        !self.tlds.contains(&tld)
    }

    /// NXDOMAIN answer for a name under a missing TLD, with the root SOA (RFC 2308 §3).
    pub fn nxdomain(&self) -> Message {
        let mut m = Message::error_msg(0, OpCode::Query, ResponseCode::NXDomain);
        let mut soa = self.soa.clone();
        if let RData::SOA(s) = &soa.data {
            soa.ttl = soa.ttl.min(s.minimum);
        }
        m.add_authority(soa);
        m
    }

    /// Answer for a query at the root apex (`. NS`, `. SOA`, `. DNSKEY`, …), with RRSIGs when `dnssec_ok`.
    pub fn apex_answer(&self, qtype: RecordType, dnssec_ok: bool) -> Option<Message> {
        let records = self.by_name.get(&Name::root())?;
        let mut m = Message::response(0, OpCode::Query);
        for r in records {
            let covered = match &r.data {
                RData::DNSSEC(DNSSECRData::RRSIG(sig)) => Some(sig.input().type_covered),
                _ => None,
            };
            if r.record_type() == qtype || (dnssec_ok && covered == Some(qtype)) {
                m.add_answer(r.clone());
            }
        }
        if m.answers.is_empty() {
            m.add_authority(self.nxdomain().authorities[0].clone()); // NODATA
        }
        Some(m)
    }

    /// Delegation for the TLD of `name`: its NS records and glue addresses.
    pub fn delegation(&self, name: &Name) -> Option<(Name, Vec<Record>, Vec<SocketAddr>)> {
        if name.is_root() {
            return None;
        }
        let tld = name.trim_to(1).to_lowercase();
        let ns: Vec<Record> = self
            .by_name
            .get(&tld)?
            .iter()
            .filter(|r| r.record_type() == RecordType::NS)
            .cloned()
            .collect();
        let mut addrs = Vec::new();
        for n in &ns {
            let RData::NS(target) = &n.data else { continue };
            for g in self
                .by_name
                .get(&target.0.to_lowercase())
                .into_iter()
                .flatten()
            {
                match &g.data {
                    RData::A(a) => addrs.push(SocketAddr::from((a.0, 53))),
                    RData::AAAA(a) => addrs.push(SocketAddr::from((a.0, 53))),
                    _ => {}
                }
            }
        }
        // IPv4 first: many networks have no working IPv6 route.
        addrs.sort_by_key(|a| a.is_ipv6());
        Some((tld, ns, addrs))
    }
}

/// Verifies the zone: the DNSKEY set against the root trust anchors, the ZONEMD and SOA signatures
/// against that key set, and the ZONEMD digest over the whole zone (RFC 8976 §4).
pub fn verify(records: &[Record], now: u64) -> Result<(), VerifyError> {
    let root = Name::root();
    let apex: Vec<&Record> = records.iter().filter(|r| r.name.is_root()).collect();
    let rrset = |t: RecordType| {
        apex.iter()
            .filter(move |r| r.record_type() == t)
            .map(|r| (*r).clone())
            .collect::<Vec<_>>()
    };
    let sigs = |t: RecordType| {
        apex.iter()
            .filter_map(move |r| match &r.data {
                RData::DNSSEC(DNSSECRData::RRSIG(s)) if s.input().type_covered == t => {
                    Some(s.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let dnskey_records = rrset(RecordType::DNSKEY);
    let keys: Vec<_> = dnskey_records
        .iter()
        .filter_map(|r| match &r.data {
            RData::DNSSEC(DNSSECRData::DNSKEY(k)) if k.zone_key() && !k.revoke() => Some(k.clone()),
            _ => None,
        })
        .collect();
    let anchors = TrustAnchors::default();
    let now32 = now as u32;
    let in_time = |s: &hickory_proto::dnssec::rdata::RRSIG| {
        // RFC 4034 §3.1.5: serial-number arithmetic on 32-bit times.
        let inc = s.input().sig_inception.get();
        let exp = s.input().sig_expiration.get();
        (now32.wrapping_sub(inc) as i32) >= 0 && (exp.wrapping_sub(now32) as i32) >= 0
    };

    // 1. DNSKEY RRset signed by a key that matches a trust anchor.
    let dnskey_ok = sigs(RecordType::DNSKEY).iter().any(|sig| {
        in_time(sig)
            && keys.iter().any(|k| {
                anchors.contains(k.public_key())
                    && k.calculate_key_tag().ok() == Some(sig.input().key_tag)
                    && k.verify_rrsig(&root, DNSClass::IN, sig, dnskey_records.iter())
                        .is_ok()
            })
    });
    if !dnskey_ok {
        return Err("DNSKEY set is not signed by a trusted root key".into());
    }

    // 2. ZONEMD and SOA RRsets signed by a key in the validated set.
    let zonemd_type = RecordType::Unknown(ZONEMD);
    for (t, label) in [(zonemd_type, "ZONEMD"), (RecordType::SOA, "SOA")] {
        let set = rrset(t);
        let ok = !set.is_empty()
            && sigs(t).iter().any(|sig| {
                in_time(sig)
                    && keys.iter().any(|k| {
                        k.calculate_key_tag().ok() == Some(sig.input().key_tag)
                            && k.verify_rrsig(&root, DNSClass::IN, sig, set.iter()).is_ok()
                    })
            });
        if !ok {
            return Err(format!("{label} RRset has no valid signature"));
        }
    }

    // 3. ZONEMD digest (scheme 1 SIMPLE, hash 1 SHA-384) must match and carry the SOA serial.
    let serial = match rrset(RecordType::SOA).first().map(|r| &r.data) {
        Some(RData::SOA(s)) => s.serial,
        _ => return Err("no SOA".into()),
    };
    let computed = zone_digest(records)?;
    let matched = rrset(zonemd_type).iter().any(|r| {
        let RData::Unknown { rdata, .. } = &r.data else {
            return false;
        };
        let b = &rdata.anything;
        b.len() > 6
            && u32::from_be_bytes([b[0], b[1], b[2], b[3]]) == serial
            && b[4] == 1
            && b[5] == 1
            && b[6..] == computed[..]
    });
    if !matched {
        return Err("ZONEMD digest does not match the zone contents".into());
    }
    Ok(())
}

/// RFC 8976 §3.3.1 SIMPLE digest: SHA-384 over every RR in canonical form and order, except the
/// apex ZONEMD RRset and the RRSIGs covering it, with duplicates removed.
pub fn zone_digest(records: &[Record]) -> Result<Vec<u8>, VerifyError> {
    let mut rrs: Vec<(Name, u16, u16, Vec<u8>, u32)> = Vec::with_capacity(records.len());
    for r in records {
        let t = u16::from(r.record_type());
        if r.name.is_root() {
            if t == ZONEMD {
                continue;
            }
            if let RData::DNSSEC(DNSSECRData::RRSIG(s)) = &r.data
                && u16::from(s.input().type_covered) == ZONEMD
            {
                continue;
            }
        }
        let mut rdata = Vec::new();
        {
            let mut e = BinEncoder::new(&mut rdata);
            e.set_canonical_form(true);
            e.set_name_encoding(NameEncoding::Uncompressed);
            r.data.emit(&mut e).map_err(|e| e.to_string())?;
        }
        rrs.push((
            r.name.to_lowercase(),
            u16::from(r.dns_class),
            t,
            rdata,
            r.ttl,
        ));
    }
    // Canonical order: owner (RFC 4034 §6.1, Name's Ord), class, type, then RDATA bytes (§6.3).
    rrs.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    rrs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2 && a.3 == b.3);

    let mut ctx = ring::digest::Context::new(&ring::digest::SHA384);
    let mut buf = Vec::new();
    for (owner, class, t, rdata, ttl) in &rrs {
        buf.clear();
        {
            let mut e = BinEncoder::new(&mut buf);
            e.set_name_encoding(NameEncoding::UncompressedLowercase);
            owner.emit(&mut e).map_err(|e| e.to_string())?;
        }
        buf.extend_from_slice(&t.to_be_bytes());
        buf.extend_from_slice(&class.to_be_bytes());
        buf.extend_from_slice(&ttl.to_be_bytes());
        buf.extend_from_slice(
            &u16::try_from(rdata.len())
                .map_err(|_| "rdata too long")?
                .to_be_bytes(),
        );
        buf.extend_from_slice(rdata);
        ctx.update(&buf);
    }
    Ok(ctx.finish().as_ref().to_vec())
}

/// Status for the API and dashboard.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RootZoneStatus {
    /// Feature switched on.
    pub enabled: bool,
    /// A verified copy is loaded and in use.
    pub active: bool,
    /// SOA serial of the copy in use.
    pub serial: Option<u32>,
    /// Records in the copy.
    pub records: usize,
    /// Top-level domains in the copy.
    pub tlds: usize,
    /// Unix time of the last successful refresh (or of loading a verified copy from disk).
    pub last_success: Option<u64>,
    /// Unix time of the last attempt.
    pub last_attempt: Option<u64>,
    /// Last error, if the last attempt failed.
    pub last_error: Option<String>,
    /// Unix time after which the copy is no longer used if not refreshed (RFC 8806 §2, SOA expire).
    pub expires_at: Option<u64>,
}

/// Owns the current root zone copy and refreshes it.
#[derive(Clone)]
pub struct RootManager {
    zone: Arc<RwLock<Option<Arc<RootZone>>>>,
    status: Arc<RwLock<RootZoneStatus>>,
    config: RootZoneConfig,
    file: PathBuf,
}

impl RootManager {
    /// Creates the manager and loads a verified copy from `data_dir` if one is there.
    pub fn new(config: RootZoneConfig, data_dir: &std::path::Path) -> Self {
        let m = Self {
            zone: Arc::default(),
            status: Arc::new(RwLock::new(RootZoneStatus {
                enabled: config.enabled,
                ..Default::default()
            })),
            config,
            file: data_dir.join("root.zone"),
        };
        if m.config.enabled
            && let Ok(text) = std::fs::read_to_string(&m.file)
        {
            let mtime = std::fs::metadata(&m.file)
                .and_then(|md| md.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            match RootZone::from_text(&text, now_unix()) {
                Ok(z) => {
                    info!(serial = z.serial, "loaded verified root zone from disk");
                    m.install(z, mtime);
                }
                Err(e) => warn!(error = %e, "root zone on disk failed verification, ignoring it"),
            }
        }
        m
    }

    fn install(&self, zone: RootZone, fetched: u64) {
        {
            let mut s = self.status.write().unwrap_or_else(|p| p.into_inner());
            s.active = true;
            s.serial = Some(zone.serial);
            s.records = zone.record_count;
            s.tlds = zone.tld_count();
            s.last_success = Some(fetched);
            s.last_error = None;
            s.expires_at = Some(fetched + u64::from(zone.expire_secs));
        }
        *self.zone.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(zone));
    }

    /// The copy in use, unless disabled or past its SOA expire time without a refresh.
    pub fn zone(&self) -> Option<Arc<RootZone>> {
        let expires = self
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .expires_at?;
        if now_unix() > expires {
            return None;
        }
        self.zone.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Current status.
    pub fn status(&self) -> RootZoneStatus {
        let mut s = self
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        s.active = self.zone().is_some();
        s
    }

    /// Downloads, verifies and installs a fresh copy; on success also saves it to disk.
    pub async fn refresh(&self) -> Result<u32, String> {
        let now = now_unix();
        self.status
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .last_attempt = Some(now);
        let result = self.fetch_verified().await;
        match result {
            Ok((zone, text)) => {
                let serial = zone.serial;
                let current = self
                    .zone
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                    .map(|z| z.serial);
                if current.is_some_and(|c| (serial.wrapping_sub(c) as i32) < 0) {
                    let msg = format!(
                        "downloaded serial {serial} is older than {}",
                        current.unwrap_or(0)
                    );
                    self.status
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .last_error = Some(msg.clone());
                    return Err(msg);
                }
                if let Err(e) = save_atomic(&self.file, text.as_bytes()) {
                    warn!(error = %e, "could not save root zone to disk");
                }
                info!(
                    serial,
                    records = zone.record_count,
                    "root zone refreshed and verified"
                );
                self.install(zone, now);
                Ok(serial)
            }
            Err(e) => {
                warn!(error = %e, "root zone refresh failed");
                self.status
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    async fn fetch_verified(&self) -> Result<(RootZone, String), String> {
        // reqwest is built without a default TLS backend; make sure ring is installed (no-op if it already is).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("nirvor/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        let mut last = "no sources configured".to_string();
        for url in &self.config.sources {
            let text = match client
                .get(url)
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                Ok(r) => match r.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        last = format!("{url}: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    last = format!("{url}: {e}");
                    continue;
                }
            };
            // Parsing and RSA checks take a moment; keep them off the async workers.
            let verified = tokio::task::spawn_blocking(move || {
                RootZone::from_text(&text, now_unix()).map(|z| (z, text))
            })
            .await
            .map_err(|e| e.to_string())?;
            match verified {
                Ok(v) => return Ok(v),
                Err(e) => last = format!("{url}: {e}"),
            }
        }
        Err(last)
    }

    /// Refreshes forever at the configured interval.
    pub async fn run(self) {
        if !self.config.enabled {
            return;
        }
        loop {
            let _ = self.refresh().await;
            tokio::time::sleep(Duration::from_secs(self.config.refresh_secs.max(60))).await;
        }
    }
}

/// Writes `bytes` to `path` via a temporary file and rename, so a crash never leaves a half file.
pub fn save_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real root zone is verified when `NIRVOR_ROOT_ZONE` points at a downloaded `root.zone`.
    #[test]
    fn verifies_real_root_zone_if_available() {
        let Ok(path) = std::env::var("NIRVOR_ROOT_ZONE") else {
            eprintln!("NIRVOR_ROOT_ZONE not set, skipping");
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let zone = RootZone::from_text(&text, now_unix()).expect("real root zone must verify");
        assert!(zone.tld_count() > 1000);
        assert!(zone.tld_missing(&Name::from_ascii("printer.lan.").unwrap()));
        assert!(!zone.tld_missing(&Name::from_ascii("www.prothomalo.com.").unwrap()));
        let (tld, ns, addrs) = zone
            .delegation(&Name::from_ascii("seu.edu.bd.").unwrap())
            .unwrap();
        assert_eq!(tld.to_ascii(), "bd.");
        assert!(!ns.is_empty() && !addrs.is_empty());

        // Any change to the zone must break verification.
        let tampered = text.replacen("\tA\t", "\tA\t1", 1);
        assert!(RootZone::from_text(&tampered, now_unix()).is_err());
    }
}
