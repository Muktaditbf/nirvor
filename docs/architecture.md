# Architecture

## Crates

| Crate | Role |
|---|---|
| `core` | Listeners (UDP with SO_REUSEPORT, TCP), the query pipeline (`handler.rs`), metrics, mode detection, query log, config |
| `cache` | TTL cache on `moka` with negative caching, serve-stale windows, prefetch hints, pinned entries, snapshot export/import |
| `upstream` | Upstream pool: UDP exchange with TCP fallback, RTT estimation, adaptive timeouts, health checks |
| `resilience` | Root zone reader and verifier (DNSSEC, ZONEMD), root manager, cache snapshot file format, iterative resolver |
| `zones` | Static local zones (zone-file syntax), authoritative answers |
| `policy` | ACL, token-bucket rate limits, RRL, blocklists with hot reload |
| `storage` | SQLite (sqlx): audit log, settings (admin hash), per-minute history |
| `api` | axum: `/api/v1`, SSE stream, auth, `/metrics`, embedded dashboard, connectivity-check responder |
| `server` | The `nirvor` binary: config loading, wiring, HTTPS, graceful shutdown |
| `bench` | `nirvor-bench`, a dnsperf-style load generator |

`web/` is the React dashboard. It is built to `web/dist` and embedded with `rust-embed`, so the release binary is self-contained.

## Query pipeline

`Resolver::handle` (`crates/core/src/handler.rs`) runs these steps in order:

1. **Size, ACL and rate limit.** Oversized packets, responses and packets under 12 bytes are dropped. Clients outside `policy.allow` are dropped or refused. Clients over their token bucket are dropped.
2. **Parse.** Unparseable queries get FORMERR built from the raw header, opcodes other than QUERY get NOTIMP, and anything with a QDCOUNT other than 1 gets FORMERR.
3. **Blocklist.** The answer depends on `block_response`.
4. **Isolation overrides** (only in isolation), then **local zones**.
5. **Local root zone.** Root apex queries are answered locally. Names under a TLD that doesn't exist get NXDOMAIN, unless the query has DO=1, because DNSSEC clients need the NSEC proof from upstream.
6. **Cache** (fresh hit). When a popular entry is near expiry, a background prefetch starts.
7. **Failure recheck** (RFC 8767 §5). A recent failure for this key means answering stale, or SERVFAIL, without asking upstream again.
8. **Fetch.** Concurrent identical misses share one upstream query (coalescing). The pool tries healthy upstreams fastest first. In isolation it uses only local ones. If they all fail and a verified root zone is loaded, the **iterative resolver** takes over.
9. **Serve-stale.** If a stale copy exists, the fetch races a 1.8 s client-response timer. If the timer wins, the stale copy is answered and the fetch continues in the background to refresh the cache.
10. **Finish.** The client's ID, question spelling and RD bit are restored, with our own OPT record (DO echoed) if the client used EDNS. RRL applies to UDP (send, slip TC=1, or drop). Replies too big for the client's or our UDP limit are truncated. The query is also logged.

Every query has an overall timeout (`limits.query_timeout_ms`). UDP queries in flight and TCP connections are bounded by semaphores, so memory stays bounded under load.

## Cache

Keys are (lower-cased name, type, class, DO, CD).

- **TTL.** The minimum TTL across answer and authority records, clamped. Negative answers use min(SOA TTL, SOA MINIMUM) per RFC 2308, capped by `negative_max_ttl`. Truncated answers, SERVFAIL/REFUSED, TTL 0 and negative answers without an SOA are not cached.
- **Lifetime.** Entries live for `ttl + window` in moka. The window is the longest stale window, or the pinned window for pinned names. `get_stale` then applies the window for the current situation: 1 day normally, 7 days in isolation, 30 days for pinned names.
- **TTLs on the way out.** Served TTLs count down. Stale answers carry a 30 s TTL (RFC 8767 §4).
- **Snapshots.** These use a compact binary format (`resilience/src/snapshot.rs`), written atomically every 5 minutes and on shutdown. Entry age is preserved across restarts.

## Upstream pool

Each upstream keeps a smoothed RTT and RTT variance as in RFC 6298 (Jacobson/Karels).

- **Timeouts.** The per-attempt timeout is `srtt + 4·rttvar`, clamped to `[min_timeout_ms, timeout_ms]`. Unmeasured upstreams get the maximum.
- **Ordering.** Healthy upstreams come first, then lowest RTT.
- **Marking down.** `failure_threshold` failures in a row mark an upstream down. One success brings it back.
- **Health checks.** Every `health_interval_secs`, a check sends `. NS`.
- **Answer handling.** SERVFAIL makes the pool try the next upstream without counting against health. REFUSED counts as a failure.
- **Query privacy.** Upstream queries carry only the question and our own EDNS. No client cookies or ECS are forwarded.

## Root zone (RFC 8806)

`root.zone` is downloaded over HTTPS from IANA's internic.net. A small reader (`resilience/src/zonefile.rs`) turns each line into wire format, and Hickory decodes it. This covers DNSKEY and ZONEMD, which Hickory's text parser does not read.

Verification (`rootzone.rs::verify`) runs in this order:

1. The apex DNSKEY RRset must be signed by a key matching the IANA trust anchors (KSK-2017, KSK-2024), with a signature inside its validity window.
2. The ZONEMD and SOA RRsets must be signed by a key from that validated set.
3. The SHA-384 SIMPLE digest (RFC 8976 §3.3.1) over every record in canonical form and order must equal the ZONEMD digest, and the ZONEMD serial must equal the SOA serial.

After that:

- A verified zone is saved to `data_dir/root.zone` and re-verified when loaded at startup.
- A copy older than its SOA expire (7 days) without a successful refresh is no longer used (RFC 8806 §2).
- An older serial never replaces a newer one.

## Iterative resolution

`resilience/src/recursor.rs` starts from the TLD delegation in the local root zone and follows referrals with RD=0.

- It accepts glue only from inside the zone it just asked (bailiwick).
- It resolves nameserver addresses that have no glue, up to depth 3.
- It follows CNAMEs, up to 8.
- It caches zone cuts for an hour.
- It treats NODATA without an AA flag or SOA as a lame server.

It is used when every allowed upstream fails, which is always the case in isolation unless local upstreams are configured. Answers are not DNSSEC-validated.

## Isolation mode

`mode.rs::next_mode` is a pure function of international upstream health:

- **Normal:** all healthy.
- **Degraded:** some failing.
- **Isolation:** all down for `isolation_after_secs`. It leaves isolation only after an international upstream has been back for `recover_after_secs`, so flapping does not toggle the mode.

The operator can force Normal or Isolation. Every control and mode change goes to the audit log.

## Dashboard and API

- **API.** axum serves `/api/v1` (see [api.md](api.md)). Live data comes through one SSE stream with `tick` events every second and `query` events.
- **Login.** Auth uses one admin password stored as argon2id in SQLite. Sessions are random 43-character tokens in `HttpOnly; SameSite=Strict` cookies, with `Secure` under HTTPS, and expire after 12 h idle.
- **CSRF.** Every non-GET request must send the session's CSRF token in `X-CSRF-Token`. Failed logins are limited to 10 per 15 minutes per address.
- **Headers.** Every response carries CSP, `X-Frame-Options: DENY`, `nosniff` and `no-referrer`.
- **Frontend stack.** React 19 with TypeScript and Vite, Tailwind CSS 4 and shadcn/ui (Radix), Recharts, Framer Motion, TanStack Query and react-i18next.
