# Operations runbook

## Install with systemd

```sh
(cd web && npm ci && npm run build) && cargo build --release
sudo useradd --system --no-create-home --shell /usr/sbin/nologin nirvor
sudo install -m 0755 target/release/nirvor /usr/local/bin/nirvor
sudo install -d -o nirvor -g nirvor -m 0750 /etc/nirvor
sudo install -m 0640 -o nirvor -g nirvor config/config.toml /etc/nirvor/config.toml
sudoedit /etc/nirvor/config.toml   # listen on :53, policy.allow, data_dir = "/var/lib/nirvor", api.listen
sudo install -m 0644 deploy/nirvor.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now nirvor
sudo cat /var/lib/nirvor/initial-admin-password   # sign in, change it, then delete the file
```

The unit:

- runs as `nirvor` and gets only `CAP_NET_BIND_SERVICE`;
- validates the config before starting (`ExecStartPre`);
- uses `ProtectSystem=strict` and a syscall filter;
- can write only `/var/lib/nirvor` and `/etc/nirvor`, so dashboard edits can be saved.

## Docker

```sh
docker compose -f deploy/docker-compose.yml up -d --build
```

Edit `deploy/config.docker.toml` first. Set `policy.allow` to the networks that reach the container. The dashboard is published only on 127.0.0.1.

## Daily checks

- **Dashboard → Overview.** The mode banner, hit ratio (usually 70–95% once warm), p99 latency and upstream health.
- **Prometheus.** `nirvor_mode` (0 normal, 1 degraded, 2 isolation), `nirvor_upstream_up`, `nirvor_root_zone_active`, `rate(nirvor_servfail_total[5m])` and `rate(nirvor_stale_served_total[5m])`. A rising stale count means upstreams are slow or failing.
- **Resilience page.** The root zone should show "Active" with a recent "Last verified".

Example alerts:

```text
nirvor_mode == 2 for 1m                      → international isolation
nirvor_root_zone_active == 0 for 1h          → local root zone unavailable
sum(nirvor_upstream_up) == 0                 → no healthy upstreams
```

## During an international outage

nirvor handles this by itself. Here is what to expect and what you can do.

1. After about 15 s with every international upstream down, the mode turns **Isolation**, and an audit entry and a warning log line are written.
2. While isolated, nirvor:
   - answers cached names, even expired ones, for up to 7 days;
   - answers pinned names for up to 30 days;
   - sends new names to your local upstreams (`international = false`) or resolves them itself from the local root zone, which works for names whose authoritative servers are reachable inside the country, as much of `.bd` is;
   - answers OS connectivity checks locally, if you configured overrides and the responder.
3. If you know links are down before detection, force isolation on the Resilience page, which avoids per-query timeouts. Set it back to **Automatic** afterwards.
4. After the international upstreams have been back for 30 s, the mode returns to Normal and stale entries refresh as they are queried.

Prepare for outages:

- Configure at least one `international = false` upstream.
- Pin the local sites your subscribers use.
- Run a drill now and then: force isolation, check that local sites resolve, then go back to Automatic.

## Common tasks

| Task | How |
|---|---|
| Add or remove an upstream | Dashboard → Upstreams, or edit `[[upstream.servers]]` and save from Settings |
| Add a blocklist | Dashboard → Policy → Blocklists. Files reload when they change; URLs reload on their schedule; or press Reload |
| Change who may query | Dashboard → Policy → ACL. Applies immediately |
| Flush the cache | Dashboard → Settings → Flush cache. Avoid this during an outage |
| Rotate the admin password | Dashboard → Settings. Other sessions are signed out |
| Forgotten password | Stop nirvor, run `sqlite3 /var/lib/nirvor/nirvor.db "DELETE FROM settings WHERE key='admin_password_hash'"`, start it again; a new initial password is written |
| HTTPS dashboard | Set `api.tls_cert` and `api.tls_key`, then restart |

## Files in `data_dir`

| File | Contents |
|---|---|
| `root.zone` | Last verified root zone (re-verified on load) |
| `cache.snapshot` | Cache snapshot (binary). Safe to delete |
| `nirvor.db` (+ `-wal`, `-shm`) | Audit log, admin password hash, traffic history |
| `initial-admin-password` | Only until you delete it |

## Troubleshooting

- **Root zone "Not available".** Check the last error on the Resilience page. The machine needs HTTPS access to www.internic.net. A copy on disk keeps working for up to 7 days.
- **Queries time out.** Check that the client is in `policy.allow`: denied queries are dropped silently by default, so set `deny_action = "refuse"` while testing. Also check the rate limits on the Policy page.
- **Iterative resolution fails.** Some networks intercept outbound port 53. A non-recursive query to a TLD server answered with `ra` set and SERVFAIL means a middlebox is answering. Forwarding still works.
- **Debug logging.** Set `RUST_LOG=info,nirvor_core=debug,nirvor_upstream=debug`.
