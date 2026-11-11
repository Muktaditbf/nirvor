//! HTTP API (`/api/v1`), live stream (SSE), Prometheus metrics and the embedded dashboard.
//! Every `/api/v1` route except login, session and health needs a session; see `docs/api.md`.

pub mod auth;
pub mod connectivity;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use nirvor_core::Config;
use nirvor_core::handler::Resolver;
use nirvor_core::mode::Control;
use nirvor_upstream::UpstreamSpec;
use serde::Deserialize;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};

pub use auth::Auth;

/// Shared state for handlers.
#[derive(Clone)]
pub struct AppState {
    /// The running resolver.
    pub resolver: Arc<Resolver>,
    /// Sessions.
    pub auth: Auth,
    /// Config file the dashboard writes changes to.
    pub config_path: PathBuf,
}

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Dashboard;

type ApiResult = Result<Response, (StatusCode, Json<serde_json::Value>)>;

fn bad(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": msg.into() })))
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/auth/password", post(auth::change_password))
        .route("/api/v1/overview", get(overview))
        .route("/api/v1/stats/series", get(series))
        .route("/api/v1/stats/history", get(history))
        .route("/api/v1/stream", get(stream_events))
        .route("/api/v1/queries", get(queries))
        .route("/api/v1/top/{kind}", get(top))
        .route("/api/v1/upstreams", get(upstreams).post(add_upstream))
        .route(
            "/api/v1/upstreams/{index}",
            put(edit_upstream).delete(delete_upstream),
        )
        .route("/api/v1/resilience", get(resilience))
        .route("/api/v1/resilience/mode", post(set_mode))
        .route("/api/v1/resilience/pinned", put(set_pinned))
        .route("/api/v1/resilience/root/refresh", post(refresh_root))
        .route("/api/v1/resilience/snapshot", post(save_snapshot))
        .route("/api/v1/policy", get(policy).put(set_policy))
        .route("/api/v1/policy/blocklists/reload", post(reload_blocklists))
        .route("/api/v1/config", get(get_config).put(put_config))
        .route("/api/v1/cache/flush", post(flush_cache))
        .route("/api/v1/audit", get(audit))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));
    Router::new()
        .route("/api/v1/auth/login", post(auth::login))
        .route("/api/v1/auth/session", get(auth::session))
        .route(
            "/api/v1/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route("/metrics", get(metrics))
        .merge(protected)
        .fallback(static_file)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// Security headers on every response.
async fn security_headers(req: axum::extract::Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        ),
    );
    resp
}

/// Serves the embedded dashboard; unknown paths get `index.html` (client-side routing).
async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return bad(StatusCode::NOT_FOUND, "no such endpoint").into_response();
    }
    let file = Dashboard::get(path)
        .filter(|_| !path.is_empty())
        .or_else(|| Dashboard::get("index.html"));
    match file {
        Some(f) => {
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, f.metadata.mimetype().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                f.data,
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            "dashboard not built: run `npm run build` in web/",
        )
            .into_response(),
    }
}

async fn overview(State(s): State<AppState>) -> Json<serde_json::Value> {
    let r = &s.resolver;
    let mut ups = r.upstreams.status();
    for u in &mut ups {
        u.history.clear();
    }
    Json(serde_json::json!({
        "summary": r.metrics.summary(),
        "mode": r.mode_state(),
        "upstreams": ups,
        "cache_entries": r.cache.len(),
        "root_zone": r.root.status(),
        "blocklist_entries": r.blocklists.current().len(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[derive(Deserialize)]
struct SeriesQuery {
    seconds: Option<usize>,
}

async fn series(
    State(s): State<AppState>,
    Query(q): Query<SeriesQuery>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!(
        s.resolver
            .metrics
            .series(q.seconds.unwrap_or(300).min(3600))
    ))
}

#[derive(Deserialize)]
struct HistoryQuery {
    hours: Option<i64>,
}

async fn history(State(s): State<AppState>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 90);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rows = s
        .resolver
        .storage
        .history(now - hours * 3600)
        .await
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows).into_response())
}

/// Server-sent events: `tick` every second (latest sample, summary, mode) and `query` per answered query.
async fn stream_events(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let r = s.resolver.clone();
    let ticks = IntervalStream::new(tokio::time::interval(Duration::from_secs(1))).map(move |_| {
        let data = serde_json::json!({
            "sample": r.metrics.series(1).pop(),
            "summary": r.metrics.summary(),
            "mode": r.mode_state(),
            "cache_entries": r.cache.len(),
        });
        Ok(Event::default().event("tick").data(data.to_string()))
    });
    let queries =
        BroadcastStream::new(s.resolver.querylog.subscribe()).filter_map(|ev| async move {
            ev.ok()
                .and_then(|e| serde_json::to_string(&e).ok())
                .map(|d| Ok(Event::default().event("query").data(d)))
        });
    Sse::new(stream::select(ticks, queries)).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

async fn queries(
    State(s): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "privacy": s.resolver.settings().logging.query_log,
        "queries": s.resolver.querylog.recent(q.limit.unwrap_or(500).min(5000)),
    }))
}

async fn top(
    State(s): State<AppState>,
    Path(kind): Path<String>,
    Query(q): Query<LimitQuery>,
) -> ApiResult {
    let clients = match kind.as_str() {
        "domains" => false,
        "clients" => true,
        _ => {
            return Err(bad(
                StatusCode::NOT_FOUND,
                "kind must be domains or clients",
            ));
        }
    };
    Ok(Json(
        s.resolver
            .querylog
            .top(clients, q.limit.unwrap_or(25).min(500)),
    )
    .into_response())
}

async fn upstreams(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(s.resolver.upstreams.status()))
}

/// Applies a change to a copy of the current config, then applies and saves it.
async fn update_config(
    s: &AppState,
    change: impl FnOnce(&mut Config) -> Result<(), String>,
) -> ApiResult {
    let mut cfg = (*s.resolver.settings()).clone();
    change(&mut cfg).map_err(|e| bad(StatusCode::BAD_REQUEST, e))?;
    apply_and_save(s, cfg).await
}

async fn apply_and_save(s: &AppState, cfg: Config) -> ApiResult {
    let text = toml::to_string_pretty(&cfg)
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let restart = s
        .resolver
        .apply_config(cfg, "admin")
        .await
        .map_err(|e| bad(StatusCode::BAD_REQUEST, e))?;
    if let Err(e) = save_atomic(&s.config_path, text.as_bytes()) {
        return Err(bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "applied, but could not save {}: {e}",
                s.config_path.display()
            ),
        ));
    }
    Ok(Json(serde_json::json!({ "restart_required": restart })).into_response())
}

fn save_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

async fn add_upstream(State(s): State<AppState>, Json(spec): Json<UpstreamSpec>) -> ApiResult {
    update_config(&s, |c| {
        c.upstream.servers.push(spec);
        Ok(())
    })
    .await
}

async fn edit_upstream(
    State(s): State<AppState>,
    Path(i): Path<usize>,
    Json(spec): Json<UpstreamSpec>,
) -> ApiResult {
    update_config(&s, |c| {
        let slot = c
            .upstream
            .servers
            .get_mut(i)
            .ok_or("no upstream at that index")?;
        *slot = spec;
        Ok(())
    })
    .await
}

async fn delete_upstream(State(s): State<AppState>, Path(i): Path<usize>) -> ApiResult {
    update_config(&s, |c| {
        if i >= c.upstream.servers.len() {
            return Err("no upstream at that index".into());
        }
        c.upstream.servers.remove(i);
        Ok(())
    })
    .await
}

async fn resilience(State(s): State<AppState>) -> Json<serde_json::Value> {
    let r = &s.resolver;
    let settings = r.settings();
    let snap = settings.resilience.data_dir.join("cache.snapshot");
    let meta = std::fs::metadata(&snap).ok();
    let saved_at = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    Json(serde_json::json!({
        "mode": r.mode_state(),
        "isolation": settings.isolation,
        "root_zone": r.root.status(),
        "snapshot": {
            "path": snap.display().to_string(),
            "bytes": meta.map(|m| m.len()),
            "saved_at": saved_at,
            "interval_secs": settings.resilience.snapshot_interval_secs,
        },
        "pinned_domains": settings.resilience.pinned_domains,
        "cache_entries": r.cache.len(),
    }))
}

#[derive(Deserialize)]
struct ModeBody {
    control: Control,
}

async fn set_mode(State(s): State<AppState>, Json(b): Json<ModeBody>) -> Json<serde_json::Value> {
    s.resolver.set_control(b.control, "admin").await;
    Json(serde_json::json!(s.resolver.mode_state()))
}

#[derive(Deserialize)]
struct PinnedBody {
    domains: Vec<String>,
}

async fn set_pinned(State(s): State<AppState>, Json(b): Json<PinnedBody>) -> ApiResult {
    update_config(&s, |c| {
        let mut domains: Vec<String> = b
            .domains
            .into_iter()
            .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|d| !d.is_empty())
            .collect();
        domains.sort();
        domains.dedup();
        c.resilience.pinned_domains = domains;
        Ok(())
    })
    .await
}

async fn refresh_root(State(s): State<AppState>) -> ApiResult {
    match s.resolver.root.refresh().await {
        Ok(serial) => Ok(Json(serde_json::json!({ "serial": serial })).into_response()),
        Err(e) => Err(bad(StatusCode::BAD_GATEWAY, e)),
    }
}

async fn save_snapshot(State(s): State<AppState>) -> ApiResult {
    let r = s.resolver.clone();
    match tokio::task::spawn_blocking(move || r.save_snapshot()).await {
        Ok(Ok(n)) => Ok(Json(serde_json::json!({ "entries": n })).into_response()),
        Ok(Err(e)) => Err(bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err(e) => Err(bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn policy(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "policy": s.resolver.settings().policy,
        "blocklists": s.resolver.blocklists.status(),
        "blocked_entries": s.resolver.blocklists.current().len(),
    }))
}

async fn set_policy(
    State(s): State<AppState>,
    Json(p): Json<nirvor_policy::PolicyConfig>,
) -> ApiResult {
    update_config(&s, |c| {
        c.policy = p;
        Ok(())
    })
    .await
}

async fn reload_blocklists(State(s): State<AppState>) -> Json<serde_json::Value> {
    s.resolver.blocklists.reload(true).await;
    Json(serde_json::json!(s.resolver.blocklists.status()))
}

async fn get_config(State(s): State<AppState>) -> ApiResult {
    let text = toml::to_string_pretty(&*s.resolver.settings())
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(
        Json(serde_json::json!({ "toml": text, "path": s.config_path.display().to_string() }))
            .into_response(),
    )
}

#[derive(Deserialize)]
struct ConfigBody {
    toml: String,
}

async fn put_config(State(s): State<AppState>, Json(b): Json<ConfigBody>) -> ApiResult {
    let cfg: Config = toml::from_str(&b.toml)
        .map_err(|e| bad(StatusCode::BAD_REQUEST, format!("invalid TOML: {e}")))?;
    apply_and_save(&s, cfg).await
}

async fn flush_cache(State(s): State<AppState>) -> Json<serde_json::Value> {
    s.resolver.cache.clear();
    let _ = s
        .resolver
        .storage
        .audit("admin", "cache_flush", "cache cleared")
        .await;
    Json(serde_json::json!({ "ok": true }))
}

async fn audit(State(s): State<AppState>, Query(q): Query<LimitQuery>) -> ApiResult {
    let rows = s
        .resolver
        .storage
        .audit_log(q.limit.unwrap_or(200) as i64)
        .await
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows).into_response())
}

async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    let r = &s.resolver;
    let mut x = String::new();
    let mode = r.mode_state();
    let root = r.root.status();
    let gauges = [
        ("cache_entries", "Answers in cache", r.cache.len()),
        (
            "mode",
            "Operating mode (0 normal, 1 degraded, 2 isolation)",
            mode.mode as u64,
        ),
        (
            "blocklist_entries",
            "Blocklist entries loaded",
            r.blocklists.current().len() as u64,
        ),
        (
            "root_zone_active",
            "Local root zone in use (1 yes)",
            u64::from(root.active),
        ),
    ];
    for (name, help, v) in gauges {
        let _ = writeln!(
            x,
            "# HELP nirvor_{name} {help}\n# TYPE nirvor_{name} gauge\nnirvor_{name} {v}"
        );
    }
    let _ = writeln!(
        x,
        "# HELP nirvor_upstream_up Upstream health (1 up, 0 down)\n# TYPE nirvor_upstream_up gauge"
    );
    let _ = writeln!(
        x,
        "# HELP nirvor_upstream_srtt_seconds Smoothed upstream round-trip time\n# TYPE nirvor_upstream_srtt_seconds gauge"
    );
    for u in r.upstreams.status() {
        let label = format!("name=\"{}\",addr=\"{}\"", escape(&u.spec.name), u.spec.addr);
        let _ = writeln!(x, "nirvor_upstream_up{{{label}}} {}", u8::from(u.healthy));
        if let Some(ms) = u.srtt_ms {
            let _ = writeln!(x, "nirvor_upstream_srtt_seconds{{{label}}} {}", ms / 1000.0);
        }
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        r.metrics.prometheus(&x),
    )
}

/// Escapes a Prometheus label value.
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
