//! Reader for the IANA root zone file (`root.zone`): one record per line, fully qualified names,
//! `owner TTL class type rdata`. Each line is turned into wire format and decoded by Hickory, which
//! also covers DNSKEY and ZONEMD that Hickory's text parser does not read.

use data_encoding::{BASE64, HEXUPPER_PERMISSIVE};
use hickory_proto::rr::{Record, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinDecoder};
use std::str::FromStr;

/// ZONEMD type code (RFC 8976); Hickory has no named variant.
pub const ZONEMD: u16 = 63;

/// Parses a whole root zone file.
pub fn parse(text: &str) -> Result<Vec<Record>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let wire = line_to_wire(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        let mut d = BinDecoder::new(&wire);
        let rec = Record::read(&mut d).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(rec);
    }
    Ok(out)
}

fn type_code(s: &str) -> Result<u16, String> {
    if s.eq_ignore_ascii_case("ZONEMD") {
        return Ok(ZONEMD);
    }
    if let Some(n) = s.strip_prefix("TYPE") {
        return n.parse().map_err(|_| format!("bad type {s}"));
    }
    RecordType::from_str(s)
        .map(u16::from)
        .map_err(|_| format!("unknown type {s}"))
}

/// Uncompressed wire form of a fully qualified name.
fn name_wire(s: &str, out: &mut Vec<u8>) -> Result<(), String> {
    if !s.ends_with('.') {
        return Err(format!("name not fully qualified: {s}"));
    }
    for label in s.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        if label.len() > 63 || label.contains('\\') {
            return Err(format!("unsupported label in {s}"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(())
}

fn num<T: FromStr>(s: Option<&str>) -> Result<T, String> {
    s.ok_or("missing field")?
        .parse()
        .map_err(|_| "bad number".to_string())
}

/// `YYYYMMDDHHmmSS` (RFC 4034 §3.2) to seconds since the epoch, modulo 2^32.
fn sig_time(s: Option<&str>) -> Result<u32, String> {
    let s = s.ok_or("missing time")?;
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().map_err(|_| format!("bad time {s}"));
    }
    let n = |a: usize, b: usize| s[a..b].parse::<i64>().unwrap_or(0);
    let (y, m, d) = (n(0, 4), n(4, 6), n(6, 8));
    // Days from civil date (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + n(8, 10) * 3600 + n(10, 12) * 60 + n(12, 14);
    Ok(secs as u32)
}

fn type_bitmap(types: &[u16]) -> Vec<u8> {
    let mut sorted = types.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let window = (sorted[i] >> 8) as u8;
        let mut bits = [0u8; 32];
        let mut len = 0;
        while i < sorted.len() && (sorted[i] >> 8) as u8 == window {
            let low = (sorted[i] & 0xff) as usize;
            bits[low / 8] |= 0x80 >> (low % 8);
            len = len.max(low / 8 + 1);
            i += 1;
        }
        out.push(window);
        out.push(len as u8);
        out.extend_from_slice(&bits[..len]);
    }
    out
}

fn line_to_wire(line: &str) -> Result<Vec<u8>, String> {
    let mut f = line.split_whitespace();
    let owner = f.next().ok_or("empty")?;
    let ttl: u32 = num(f.next())?;
    let class = f.next().ok_or("missing class")?;
    if !class.eq_ignore_ascii_case("IN") {
        return Err(format!("unsupported class {class}"));
    }
    let rtype_s = f.next().ok_or("missing type")?;
    let rtype = type_code(rtype_s)?;

    let mut rd = Vec::new();
    match rtype_s.to_ascii_uppercase().as_str() {
        "A" => {
            let ip: std::net::Ipv4Addr = num(f.next())?;
            rd.extend_from_slice(&ip.octets());
        }
        "AAAA" => {
            let ip: std::net::Ipv6Addr = num(f.next())?;
            rd.extend_from_slice(&ip.octets());
        }
        "NS" | "CNAME" | "PTR" => name_wire(f.next().ok_or("missing name")?, &mut rd)?,
        "SOA" => {
            name_wire(f.next().ok_or("missing mname")?, &mut rd)?;
            name_wire(f.next().ok_or("missing rname")?, &mut rd)?;
            for _ in 0..5 {
                rd.extend_from_slice(&num::<u32>(f.next())?.to_be_bytes());
            }
        }
        "DS" => {
            rd.extend_from_slice(&num::<u16>(f.next())?.to_be_bytes());
            rd.push(num(f.next())?);
            rd.push(num(f.next())?);
            let hex: String = f.collect();
            rd.extend(
                HEXUPPER_PERMISSIVE
                    .decode(hex.as_bytes())
                    .map_err(|e| e.to_string())?,
            );
        }
        "DNSKEY" => {
            rd.extend_from_slice(&num::<u16>(f.next())?.to_be_bytes());
            rd.push(num(f.next())?);
            rd.push(num(f.next())?);
            let b64: String = f.collect();
            rd.extend(BASE64.decode(b64.as_bytes()).map_err(|e| e.to_string())?);
        }
        "RRSIG" => {
            rd.extend_from_slice(
                &type_code(f.next().ok_or("missing covered type")?)?.to_be_bytes(),
            );
            rd.push(num(f.next())?);
            rd.push(num(f.next())?);
            rd.extend_from_slice(&num::<u32>(f.next())?.to_be_bytes());
            rd.extend_from_slice(&sig_time(f.next())?.to_be_bytes());
            rd.extend_from_slice(&sig_time(f.next())?.to_be_bytes());
            rd.extend_from_slice(&num::<u16>(f.next())?.to_be_bytes());
            name_wire(f.next().ok_or("missing signer")?, &mut rd)?;
            let b64: String = f.collect();
            rd.extend(BASE64.decode(b64.as_bytes()).map_err(|e| e.to_string())?);
        }
        "NSEC" => {
            name_wire(f.next().ok_or("missing next name")?, &mut rd)?;
            let types = f.map(type_code).collect::<Result<Vec<_>, _>>()?;
            rd.extend(type_bitmap(&types));
        }
        "ZONEMD" => {
            rd.extend_from_slice(&num::<u32>(f.next())?.to_be_bytes());
            rd.push(num(f.next())?);
            rd.push(num(f.next())?);
            let hex: String = f.collect();
            rd.extend(
                HEXUPPER_PERMISSIVE
                    .decode(hex.as_bytes())
                    .map_err(|e| e.to_string())?,
            );
        }
        other => return Err(format!("unsupported type {other}")),
    }

    let mut wire = Vec::with_capacity(rd.len() + 64);
    name_wire(owner, &mut wire)?;
    wire.extend_from_slice(&rtype.to_be_bytes());
    wire.extend_from_slice(&1u16.to_be_bytes());
    wire.extend_from_slice(&ttl.to_be_bytes());
    wire.extend_from_slice(
        &u16::try_from(rd.len())
            .map_err(|_| "rdata too long")?
            .to_be_bytes(),
    );
    wire.extend(rd);
    Ok(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::dnssec::rdata::DNSSECRData;
    use hickory_proto::rr::RData;

    #[test]
    fn sig_time_matches_known_epoch() {
        assert_eq!(sig_time(Some("19700101000000")).unwrap(), 0);
        assert_eq!(sig_time(Some("20261124050000")).unwrap(), 1_795_496_400);
    }

    #[test]
    fn type_bitmap_matches_rfc4034_example() {
        // RFC 4034 §4.3: A MX RRSIG NSEC TYPE1234
        let b = type_bitmap(&[1, 15, 46, 47, 1234]);
        let mut want = vec![0x00, 0x06, 0x40, 0x01, 0x00, 0x00, 0x00, 0x03, 0x04, 0x1b];
        want.extend([0u8; 26]);
        want.push(0x20);
        assert_eq!(b, want);
    }

    #[test]
    fn parses_each_root_zone_type() {
        let text = ".\t86400\tIN\tSOA\ta.root-servers.net. nstld.verisign-grs.com. 2026111100 1800 900 604800 86400
bd.\t172800\tIN\tNS\tbd-ns.bd.
bd-ns.bd.\t172800\tIN\tA\t203.112.194.231
a.root-servers.net.\t518400\tIN\tAAAA\t2001:503:ba3e::2:30
bd.\t86400\tIN\tDS\t46405 8 2 0E6D4E5ED68C1E1A97B6B1D3F6B4A0D2E1E5B7E6D4C3B2A1908F7E6D5C4B3A29
.\t172800\tIN\tDNSKEY\t257 3 8 AwEAAaz/tAm8yTn4Mfeh5eyI96WSVexTBAvkMgJzkKTOiW1vkIbzxeF3+/4RgWOq7HrxRixHlFlExOLAJr5emLvN7SWXgnLh4+B5xQlNVz8Og8kvArMtNROxVQuCaSnIDdD5LKyWbRd2n9WGe2R8PzgCmr3EgVLrjyBxWezF0jLHwVN8efS3rCj/EWgvIWgb9tarpVUDK/b58Da+sqqls3eNbuv7pr+eoZG+SrDK6nWeL3c6H5Apxz7LjVc1uTIdsIXxuOLYA4/ilBmSVIzuDWfdRUfhHdY6+cn8HFRm+2hM8AnXGXws9555KrUB5qihylGa8subX2Nn6UwNR1AkUTV74bU=
.\t86400\tIN\tNSEC\taaa. NS SOA RRSIG NSEC DNSKEY ZONEMD
.\t86400\tIN\tZONEMD\t2026111100 1 1 983F2173537C7B6C28A7EDDAB6959D326882777B228AF173B784817452D0CA3160CA6041CE8C3C606B36D07E07084603
.\t86400\tIN\tRRSIG\tZONEMD 8 0 86400 20261124050000 20261111040000 8763 . AAAA
";
        let recs = parse(text).unwrap();
        assert_eq!(recs.len(), 9);
        assert!(matches!(recs[0].data, RData::SOA(_)));
        assert!(matches!(
            recs[5].data,
            RData::DNSSEC(DNSSECRData::DNSKEY(_))
        ));
        assert_eq!(u16::from(recs[7].record_type()), ZONEMD);
        let RData::DNSSEC(DNSSECRData::RRSIG(sig)) = &recs[8].data else {
            panic!("not RRSIG")
        };
        assert_eq!(u16::from(sig.input().type_covered), ZONEMD);
        assert_eq!(sig.input().key_tag, 8763);
    }
}
