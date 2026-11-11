# Configuration reference

The config is one TOML file, passed with `-c`. Unknown keys are rejected.

- Check a file without starting: `nirvor --check-config -c file.toml`.
- The dashboard can edit the file. Changes are validated, applied live where possible, then written atomically, without comments. The reply lists the sections that need a restart: `server`, `cache`, `api`, the connection limits and the root zone source.

[`config/config.toml`](../config/config.toml) is a complete annotated example.

## `[server]` (restart to change)

| Key | Default | Meaning |
|---|---|---|
| `listen` | (required) | Addresses for UDP and TCP, e.g. `["0.0.0.0:53", "[::]:53"]` |
| `max_udp_payload` | `1232` | Largest UDP reply sent. Bigger answers are truncated (TC=1) |
| `udp_sockets` | CPUs | SO_REUSEPORT sockets per address |

## `[upstream]` (live)

| Key | Default | Meaning |
|---|---|---|
| `servers` | (required) | List of `{ name, addr, international = true, enabled = true }` |
| `timeout_ms` | `1500` | Longest wait per upstream attempt |
| `min_timeout_ms` | `150` | Shortest adaptive timeout |
| `max_attempts` | `3` | Upstreams tried per query |
| `health_interval_secs` | `5` | Probe interval |
| `failure_threshold` | `3` | Consecutive failures before an upstream is marked down |
| `probe_name` | `"."` | Name probed (type NS) |

Mark resolvers reachable without international links (BDIX, your own) with `international = false`. They are the only upstreams used in isolation.

## `[cache]` (restart to change)

| Key | Default | Meaning |
|---|---|---|
| `max_entries` | `200000` | Capacity |
| `min_ttl` / `max_ttl` | `0` / `86400` | TTL clamps |
| `negative_max_ttl` | `3600` | Cap for NXDOMAIN/NODATA (RFC 2308) |
| `prefetch_percent` / `prefetch_min_hits` | `10` / `3` | Refresh popular entries in the last N% of their TTL |
| `stale_window` | `86400` | Serve-stale window, normal mode |
| `isolation_stale_window` | `604800` | Serve-stale window, isolation |
| `pinned_stale_window` | `2592000` | Serve-stale window, pinned names |
| `stale_answer_ttl` | `30` | TTL on stale answers |
| `client_response_timer_ms` | `1800` | Answer stale if a refresh is slower than this |
| `failure_recheck_secs` | `30` | After a failure, serve stale without retrying |
| `servfail_cache_secs` | `5` | After a failure with nothing stale, answer SERVFAIL without retrying |

## `[limits]`

| Key | Default | Live? |
|---|---|---|
| `max_inflight_udp` | `4096` | restart |
| `max_tcp_connections` | `512` | restart |
| `tcp_idle_timeout_ms` | `10000` | restart |
| `max_query_size` | `4096` | live |
| `query_timeout_ms` | `4000` | live |

## `[api]` (restart to change)

| Key | Default | Meaning |
|---|---|---|
| `listen` | `127.0.0.1:8053` | Dashboard, API and `/metrics` |
| `tls_cert`, `tls_key` | unset | PEM files. When set, HTTPS is used and cookies are `Secure` |

## `[policy]` (live)

| Key | Default | Meaning |
|---|---|---|
| `allow` | loopback | CIDR list. `/0` is rejected (no open resolver) |
| `deny_action` | `"drop"` | `"drop"` or `"refuse"` |
| `rate_limit.per_client_qps` / `burst` / `ipv6_prefix` | `300` / `600` / `64` | Token bucket per client. IPv6 is grouped by prefix. `0` turns it off |
| `rrl.responses_per_second` / `slip` / `ipv4_prefix` / `ipv6_prefix` | `50` / `2` / `24` / `56` | Response rate limiting for UDP. `0` turns it off |
| `block_response` | `"nxdomain"` | `"nxdomain"`, `"refused"`, `"null"` (0.0.0.0/::) or `"redirect"` |
| `redirect_ipv4` / `redirect_ipv6` | unset | Block-notice page for `redirect` |
| `block_ttl` | `60` | TTL of blocked answers |
| `blocklists` | `[]` | `{ name, path or url, enabled = true, refresh_secs = 21600 }` |
| `reload_check_secs` | `30` | How often files are checked for changes |

Blocklist lines can be in these forms:

- `example.com`: that name only.
- `*.example.com`: names below it.
- hosts-file lines such as `0.0.0.0 example.com`.
- `||example.com^`: the name and everything below it.

`#` and `!` start comments. If a reload fails, the previous contents stay in force.

## `[[zones]]` (live)

```toml
[[zones]]
name = "isp.example"
ttl = 300
records = ["@ A 10.0.0.1", "portal A 10.0.0.10", "speedtest CNAME portal", "@ MX 10 mail"]
```

Records use zone-file syntax without class or TTL, relative to the zone. Directives (`$INCLUDE`, `$ORIGIN`) are refused. Answers are authoritative (AA=1), with NXDOMAIN/NODATA and a synthesized SOA.

## `[resilience]`

| Key | Default | Live? | Meaning |
|---|---|---|---|
| `data_dir` | `"data"` | restart | Root zone copy, cache snapshot, `nirvor.db`, initial password |
| `snapshot_interval_secs` | `300` | live | Cache snapshot interval |
| `pinned_domains` | `[]` | live | Names whose A/AAAA are kept warm |
| `pinned_refresh_secs` | `60` | live | Check interval |
| `recursion_timeout_ms` | `1500` | restart | Per-server timeout for iterative resolution |
| `root_zone.enabled` | `true` | restart | Use a local root zone |
| `root_zone.sources` | internic.net | restart | HTTPS URLs tried in order |
| `root_zone.refresh_secs` | `3600` | restart | Refresh interval |

## `[isolation]` (live)

| Key | Default | Meaning |
|---|---|---|
| `isolation_after_secs` | `15` | All international upstreams down this long → isolation |
| `recover_after_secs` | `30` | An international upstream back this long → leave isolation |
| `check_interval_secs` | `2` | Mode check interval |
| `overrides` | `[]` | Zones answered only in isolation (same format as `[[zones]]`) |
| `connectivity_responder` | unset | `ip:port` for the built-in HTTP connectivity-check responder |

To keep devices from showing "No Internet" during isolation:

1. Run the responder on an address your subscribers can reach.
2. Add overrides pointing the OS check names at it:
   - `connectivitycheck.gstatic.com` and `clients3.google.com` (Android)
   - `captive.apple.com` (Apple)
   - `www.msftconnecttest.com` (Windows)
   - `detectportal.firefox.com` (Firefox)
3. Add `dns.msftncsi.com` with exactly `A 131.107.255.255` for Windows.

## `[logging]` (live)

| Key | Default | Meaning |
|---|---|---|
| `query_log` | `"anonymized"` | `"full"`, `"anonymized"` (IPv4 /24, IPv6 /48) or `"off"`. `"off"` keeps no per-query or per-client data. Changing it clears what was collected |
| `buffer` | `2000` | Recent queries kept in memory |
| `history_days` | `30` | Per-minute traffic history kept in SQLite |
| `audit_days` | `365` | Audit log retention |

Process logs go to stderr and use `RUST_LOG` (e.g. `RUST_LOG=info,nirvor_core=debug`). Add `--json-logs` for JSON lines.
