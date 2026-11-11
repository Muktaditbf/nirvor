# HTTP API

Base: `http(s)://<api.listen>`. JSON in and out. Errors come back as `{"error": "..."}` with a 4xx or 5xx status.

**Auth.**

- `POST /api/v1/auth/login` sets an `HttpOnly; SameSite=Strict` session cookie and returns `{"csrf": "..."}`.
- Every route below marked 🔒 needs the cookie.
- 🔒 routes that are not GET also need the header `X-CSRF-Token: <csrf>`.

Responses: 401 without a session, 403 with a wrong CSRF token, 429 after 10 failed logins per address in 15 minutes.

Routes that change config apply the change and save the config file. They return `{"restart_required": ["server", ...]}`, listing the sections that only take effect after a restart.

## Public

| Method | Path | Description |
|---|---|---|
| POST | `/api/v1/auth/login` | Body `{"password"}` → `{"csrf"}` + cookie |
| GET | `/api/v1/auth/session` | `{"csrf"}` for the current session, or 401 |
| GET | `/api/v1/health` | `{"status": "ok"}` |
| GET | `/metrics` | Prometheus text format (bind `api.listen` to a management network) |

## Session and account 🔒

| Method | Path | Description |
|---|---|---|
| POST | `/api/v1/auth/logout` | Ends the session |
| POST | `/api/v1/auth/password` | `{"current", "new"}`. The new password needs at least 10 characters. Signs out other sessions |

## Monitoring 🔒

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/overview` | `summary` (QPS, hit ratio, p50/p95/p99 over the last minute, lifetime totals), `mode`, `upstreams`, `cache_entries`, `root_zone`, `blocklist_entries`, `version` |
| GET | `/api/v1/stats/series?seconds=300` | Per-second samples `{ts, qps, hits, misses, p50, p95, p99}`, at most 3600 |
| GET | `/api/v1/stats/history?hours=24` | Per-minute rows from SQLite `{ts, queries, hits, misses, blocked, servfail, p50, p95, p99}` |
| GET | `/api/v1/stream` | Server-sent events. `tick` (every second): `{sample, summary, mode, cache_entries}`. `query` (each answered query): see below |
| GET | `/api/v1/queries?limit=500` | `{privacy, queries: [...]}`, newest first |
| GET | `/api/v1/top/domains?limit=25` | `[{key, total, spark}]` over the last 30 minutes, with one sparkline value per minute |
| GET | `/api/v1/top/clients?limit=25` | Same for clients (anonymized unless `query_log = "full"`, empty if `"off"`) |
| GET | `/api/v1/audit?limit=200` | `[{id, ts, actor, action, detail}]`, newest first |

A query event looks like `{ts_ms, client, name, qtype, rcode, latency_ms, source, tcp}`. `source` is one of `cache`, `upstream`, `stale`, `local`, `blocked` or `error`.

## Upstreams 🔒

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/upstreams` | `[{name, addr, international, enabled, healthy, srtt_ms, timeout_ms, consecutive_failures, queries, failures, last_error, history: [{ts, ok, rtt_ms}]}]` |
| POST | `/api/v1/upstreams` | Body `{name, addr, international, enabled}`. Adds one |
| PUT | `/api/v1/upstreams/{index}` | Replaces the upstream at `index` (config order) |
| DELETE | `/api/v1/upstreams/{index}` | Removes it |

## Resilience 🔒

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/resilience` | `mode`, `isolation` settings, `root_zone` status, `snapshot` `{path, bytes, saved_at, interval_secs}`, `pinned_domains`, `cache_entries` |
| POST | `/api/v1/resilience/mode` | `{"control": "auto" \| "normal" \| "isolation"}` → mode state. Audited |
| PUT | `/api/v1/resilience/pinned` | `{"domains": [...]}` replaces the pinned list |
| POST | `/api/v1/resilience/root/refresh` | Downloads and verifies the root zone now → `{serial}`, or 502 with the reason |
| POST | `/api/v1/resilience/snapshot` | Saves the cache snapshot now → `{entries}` |

Mode state is `{mode: "normal"|"degraded"|"isolation", control, since, reason, international_healthy, international_total}`.

Root zone status is `{enabled, active, serial, records, tlds, last_success, last_attempt, last_error, expires_at}`.

## Policy and settings 🔒

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/policy` | `{policy, blocklists: [{name, source, enabled, entries, invalid_lines, loaded_at, error}], blocked_entries}` |
| PUT | `/api/v1/policy` | The whole `[policy]` section as JSON (see the configuration reference) |
| POST | `/api/v1/policy/blocklists/reload` | Reloads every list now → statuses |
| GET | `/api/v1/config` | `{toml, path}`: the effective config |
| PUT | `/api/v1/config` | `{"toml"}`: validates, applies and saves. 400 with the reason if invalid (bad TOML, open-resolver ACL, bad zone record, …) |
| POST | `/api/v1/cache/flush` | Clears the cache. Audited |

## Example

```sh
B=http://127.0.0.1:8053
CSRF=$(curl -s -c jar -H 'content-type: application/json' -d '{"password":"…"}' $B/api/v1/auth/login | jq -r .csrf)
curl -s -b jar $B/api/v1/overview | jq .summary
curl -s -b jar -H "x-csrf-token: $CSRF" -H 'content-type: application/json' \
     -d '{"control":"isolation"}' $B/api/v1/resilience/mode
```
