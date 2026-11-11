// Typed client for the nirvor API (see docs/api.md). Cookies carry the session; the CSRF token
// from login/session is sent on every non-GET request.

export type Mode = "normal" | "degraded" | "isolation";
export type Control = "auto" | "normal" | "isolation";

export interface Totals {
  queries: number;
  cache_hits: number;
  cache_misses: number;
  stale_served: number;
  prefetches: number;
  servfail: number;
  nxdomain: number;
  blocked: number;
  rate_limited: number;
  refused: number;
  local_answers: number;
  recursive_answers: number;
}
export interface Summary {
  qps: number;
  hit_ratio: number;
  p50: number;
  p95: number;
  p99: number;
  totals: Totals;
}
export interface Sample {
  ts: number;
  qps: number;
  hits: number;
  misses: number;
  p50: number;
  p95: number;
  p99: number;
}
export interface ModeState {
  mode: Mode;
  control: Control;
  since: number;
  reason: string;
  international_healthy: number;
  international_total: number;
}
export interface UpstreamSpec {
  name: string;
  addr: string;
  international: boolean;
  enabled: boolean;
}
export interface CheckSample {
  ts: number;
  ok: boolean;
  rtt_ms: number;
}
export interface UpstreamStatus extends UpstreamSpec {
  healthy: boolean;
  srtt_ms: number | null;
  timeout_ms: number;
  consecutive_failures: number;
  queries: number;
  failures: number;
  last_error: string | null;
  history: CheckSample[];
}
export interface RootZoneStatus {
  enabled: boolean;
  active: boolean;
  serial: number | null;
  records: number;
  tlds: number;
  last_success: number | null;
  last_attempt: number | null;
  last_error: string | null;
  expires_at: number | null;
}
export interface Overview {
  summary: Summary;
  mode: ModeState;
  upstreams: UpstreamStatus[];
  cache_entries: number;
  root_zone: RootZoneStatus;
  blocklist_entries: number;
  version: string;
}
export type Source = "cache" | "upstream" | "stale" | "local" | "blocked" | "error";
export interface QueryEvent {
  ts_ms: number;
  client: string;
  name: string;
  qtype: string;
  rcode: string;
  latency_ms: number;
  source: Source;
  tcp: boolean;
}
export type Privacy = "full" | "anonymized" | "off";
export interface TopEntry {
  key: string;
  total: number;
  spark: number[];
}
export interface Resilience {
  mode: ModeState;
  isolation: {
    isolation_after_secs: number;
    recover_after_secs: number;
    check_interval_secs: number;
    overrides: { name: string; ttl: number; records: string[] }[];
    connectivity_responder: string | null;
  };
  root_zone: RootZoneStatus;
  snapshot: { path: string; bytes: number | null; saved_at: number | null; interval_secs: number };
  pinned_domains: string[];
  cache_entries: number;
}
export interface BlocklistSource {
  name: string;
  path?: string | null;
  url?: string | null;
  enabled: boolean;
  refresh_secs: number;
}
export interface BlocklistStatus {
  name: string;
  source: string;
  enabled: boolean;
  entries: number;
  invalid_lines: number;
  loaded_at: number | null;
  error: string | null;
}
export interface PolicyConfig {
  allow: string[];
  deny_action: "drop" | "refuse";
  rate_limit: { per_client_qps: number; burst: number; ipv6_prefix: number };
  rrl: { responses_per_second: number; slip: number; ipv4_prefix: number; ipv6_prefix: number };
  block_response: "nxdomain" | "refused" | "null" | "redirect";
  redirect_ipv4: string | null;
  redirect_ipv6: string | null;
  block_ttl: number;
  blocklists: BlocklistSource[];
  reload_check_secs: number;
}
export interface PolicyView {
  policy: PolicyConfig;
  blocklists: BlocklistStatus[];
  blocked_entries: number;
}
export interface AuditEntry {
  id: number;
  ts: number;
  actor: string;
  action: string;
  detail: string;
}
export interface MinuteStat {
  ts: number;
  queries: number;
  hits: number;
  misses: number;
  blocked: number;
  servfail: number;
  p50: number;
  p95: number;
  p99: number;
}
export interface ApplyResult {
  restart_required: string[];
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

let csrf = "";
let onUnauthorized: () => void = () => {};

export function setUnauthorizedHandler(f: () => void) {
  onUnauthorized = f;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrf;
  const res = await fetch(path, {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 401 && !path.endsWith("/auth/login")) onUnauthorized();
  if (!res.ok) {
    let msg = res.statusText;
    try {
      msg = (await res.json()).error ?? msg;
    } catch {
      /* not JSON */
    }
    throw new ApiError(res.status, msg);
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}

export const api = {
  async login(password: string) {
    const r = await request<{ csrf: string }>("POST", "/api/v1/auth/login", { password });
    csrf = r.csrf;
  },
  async session(): Promise<boolean> {
    try {
      const r = await request<{ csrf: string }>("GET", "/api/v1/auth/session");
      csrf = r.csrf;
      return true;
    } catch {
      return false;
    }
  },
  logout: () => request<void>("POST", "/api/v1/auth/logout"),
  changePassword: (current: string, next: string) => request<void>("POST", "/api/v1/auth/password", { current, new: next }),
  overview: () => request<Overview>("GET", "/api/v1/overview"),
  series: (seconds = 300) => request<Sample[]>("GET", `/api/v1/stats/series?seconds=${seconds}`),
  history: (hours = 24) => request<MinuteStat[]>("GET", `/api/v1/stats/history?hours=${hours}`),
  queries: (limit = 500) => request<{ privacy: Privacy; queries: QueryEvent[] }>("GET", `/api/v1/queries?limit=${limit}`),
  top: (kind: "domains" | "clients", limit = 25) => request<TopEntry[]>("GET", `/api/v1/top/${kind}?limit=${limit}`),
  upstreams: () => request<UpstreamStatus[]>("GET", "/api/v1/upstreams"),
  addUpstream: (u: UpstreamSpec) => request<ApplyResult>("POST", "/api/v1/upstreams", u),
  editUpstream: (i: number, u: UpstreamSpec) => request<ApplyResult>("PUT", `/api/v1/upstreams/${i}`, u),
  deleteUpstream: (i: number) => request<ApplyResult>("DELETE", `/api/v1/upstreams/${i}`),
  resilience: () => request<Resilience>("GET", "/api/v1/resilience"),
  setMode: (control: Control) => request<ModeState>("POST", "/api/v1/resilience/mode", { control }),
  setPinned: (domains: string[]) => request<ApplyResult>("PUT", "/api/v1/resilience/pinned", { domains }),
  refreshRoot: () => request<{ serial: number }>("POST", "/api/v1/resilience/root/refresh"),
  saveSnapshot: () => request<{ entries: number }>("POST", "/api/v1/resilience/snapshot"),
  policy: () => request<PolicyView>("GET", "/api/v1/policy"),
  setPolicy: (p: PolicyConfig) => request<ApplyResult>("PUT", "/api/v1/policy", p),
  reloadBlocklists: () => request<BlocklistStatus[]>("POST", "/api/v1/policy/blocklists/reload"),
  config: () => request<{ toml: string; path: string }>("GET", "/api/v1/config"),
  setConfig: (toml: string) => request<ApplyResult>("PUT", "/api/v1/config", { toml }),
  flushCache: () => request<{ ok: boolean }>("POST", "/api/v1/cache/flush"),
  audit: (limit = 200) => request<AuditEntry[]>("GET", `/api/v1/audit?limit=${limit}`),
};
