//! Cache snapshots on disk, so the cache survives restarts.
//!
//! Format: magic `NIRVORC1`, then per entry: name length (u16), name, type (u16), class (u16),
//! flags (u8: 1 = DO, 2 = CD), stored unix seconds (u64), TTL (u32), wire length (u32), wire.
//! All integers big-endian.

use crate::rootzone::save_atomic;
use nirvor_cache::{Key, SnapshotEntry};
use std::path::Path;

const MAGIC: &[u8; 8] = b"NIRVORC1";

/// Encodes entries.
pub fn encode(entries: &[SnapshotEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + entries.len() * 128);
    out.extend_from_slice(MAGIC);
    for e in entries {
        let (Ok(name_len), Ok(wire_len)) =
            (u16::try_from(e.key.name.len()), u32::try_from(e.wire.len()))
        else {
            continue;
        };
        out.extend_from_slice(&name_len.to_be_bytes());
        out.extend_from_slice(e.key.name.as_bytes());
        out.extend_from_slice(&e.key.rtype.to_be_bytes());
        out.extend_from_slice(&e.key.class.to_be_bytes());
        out.push(u8::from(e.key.dnssec_ok) | (u8::from(e.key.checking_disabled) << 1));
        out.extend_from_slice(&e.stored_unix.to_be_bytes());
        out.extend_from_slice(&e.ttl.to_be_bytes());
        out.extend_from_slice(&wire_len.to_be_bytes());
        out.extend_from_slice(&e.wire);
    }
    out
}

/// Decodes a snapshot. Stops at the first damaged entry and returns what was read before it.
pub fn decode(bytes: &[u8]) -> Result<Vec<SnapshotEntry>, String> {
    let Some(mut rest) = bytes.strip_prefix(MAGIC) else {
        return Err("not a nirvor cache snapshot".into());
    };
    let mut out = Vec::new();
    while !rest.is_empty() {
        match read_entry(rest) {
            Some((e, next)) => {
                out.push(e);
                rest = next;
            }
            None => break,
        }
    }
    Ok(out)
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if b.len() < n {
        return None;
    }
    let (head, tail) = b.split_at(n);
    *b = tail;
    Some(head)
}

fn read_entry(mut b: &[u8]) -> Option<(SnapshotEntry, &[u8])> {
    let name_len = u16::from_be_bytes(take(&mut b, 2)?.try_into().ok()?) as usize;
    let name = std::str::from_utf8(take(&mut b, name_len)?)
        .ok()?
        .to_string();
    let rtype = u16::from_be_bytes(take(&mut b, 2)?.try_into().ok()?);
    let class = u16::from_be_bytes(take(&mut b, 2)?.try_into().ok()?);
    let flags = take(&mut b, 1)?[0];
    let stored_unix = u64::from_be_bytes(take(&mut b, 8)?.try_into().ok()?);
    let ttl = u32::from_be_bytes(take(&mut b, 4)?.try_into().ok()?);
    let wire_len = u32::from_be_bytes(take(&mut b, 4)?.try_into().ok()?) as usize;
    let wire = take(&mut b, wire_len)?.to_vec();
    let key = Key {
        name,
        rtype,
        class,
        dnssec_ok: flags & 1 != 0,
        checking_disabled: flags & 2 != 0,
    };
    Some((
        SnapshotEntry {
            key,
            wire,
            stored_unix,
            ttl,
        },
        b,
    ))
}

/// Saves the cache to `path` atomically. Returns the number of entries written.
pub fn save(cache: &nirvor_cache::Cache, path: &Path) -> std::io::Result<usize> {
    let entries = cache.export();
    save_atomic(path, &encode(&entries))?;
    Ok(entries.len())
}

/// Loads `path` into the cache. Returns the number of entries still usable.
pub fn load(cache: &nirvor_cache::Cache, path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(cache.import(decode(&bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, flags: (bool, bool)) -> SnapshotEntry {
        SnapshotEntry {
            key: Key {
                name: name.into(),
                rtype: 1,
                class: 1,
                dnssec_ok: flags.0,
                checking_disabled: flags.1,
            },
            wire: vec![1, 2, 3, 4],
            stored_unix: 1_700_000_000,
            ttl: 300,
        }
    }

    #[test]
    fn round_trip() {
        let e = vec![
            entry("a.test.", (false, false)),
            entry("b.test.", (true, true)),
        ];
        let d = decode(&encode(&e)).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].key, e[1].key);
        assert_eq!(d[1].wire, e[1].wire);
        assert_eq!(d[0].stored_unix, 1_700_000_000);
    }

    #[test]
    fn truncated_file_keeps_complete_entries() {
        let mut bytes = encode(&[
            entry("a.test.", (false, false)),
            entry("b.test.", (false, false)),
        ]);
        bytes.truncate(bytes.len() - 3);
        assert_eq!(decode(&bytes).unwrap().len(), 1);
        assert!(decode(b"garbage!").is_err());
    }
}
