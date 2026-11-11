//! Dashboard authentication: one admin password (argon2id), session cookies, CSRF tokens and
//! a limit on failed logins.

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use nirvor_storage::Storage;
use rand::RngExt;
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

use crate::AppState;

const COOKIE: &str = "nirvor_session";
const HASH_KEY: &str = "admin_password_hash";
/// Failed logins allowed per client address per 15 minutes.
const MAX_FAILURES: u32 = 10;
/// Shortest accepted password.
pub const MIN_PASSWORD_LEN: usize = 10;

#[derive(Clone)]
struct Session {
    csrf: String,
}

/// Session store and login limiter.
#[derive(Clone)]
pub struct Auth {
    sessions: moka::sync::Cache<String, Arc<Session>>,
    failures: moka::sync::Cache<IpAddr, u32>,
    /// Mark cookies `Secure` (set when the dashboard is served over HTTPS).
    pub secure_cookies: bool,
}

impl Auth {
    /// Sessions expire after 12 hours without use.
    pub fn new(secure_cookies: bool) -> Self {
        Self {
            sessions: moka::sync::Cache::builder()
                .max_capacity(1000)
                .time_to_idle(Duration::from_secs(12 * 3600))
                .build(),
            failures: moka::sync::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(15 * 60))
                .build(),
            secure_cookies,
        }
    }

    fn cookie(&self, value: &str, max_age: i64) -> HeaderValue {
        let secure = if self.secure_cookies { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
        ))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    fn session_from(&self, headers: &HeaderMap) -> Option<(String, Arc<Session>)> {
        let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
        let token = cookies
            .split(';')
            .filter_map(|c| c.trim().split_once('='))
            .find(|(k, _)| *k == COOKIE)
            .map(|(_, v)| v.to_string())?;
        let s = self.sessions.get(&token)?;
        Some((token, s))
    }
}

fn random_token(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

/// Constant-time string comparison, so tokens cannot be guessed by timing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Hashes a password with argon2id and a random salt.
pub fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Checks a password against a stored hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// Makes sure an admin password exists. On first start it creates a random one and writes it to
/// `<data_dir>/initial-admin-password` (mode 0600). Returns that file's path when one was created.
pub async fn ensure_admin_password(
    storage: &Storage,
    data_dir: &Path,
) -> Result<Option<std::path::PathBuf>, String> {
    if storage
        .get_setting(HASH_KEY)
        .await
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(None);
    }
    let password = random_token(20);
    let hash = tokio::task::spawn_blocking(move || hash_password(&password).map(|h| (h, password)))
        .await
        .map_err(|e| e.to_string())??;
    let path = data_dir.join("initial-admin-password");
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&path).map_err(|e| e.to_string())?;
        writeln!(f, "{}", hash.1).map_err(|e| e.to_string())?;
    }
    storage
        .set_setting(HASH_KEY, &hash.0)
        .await
        .map_err(|e| e.to_string())?;
    storage
        .audit(
            "system",
            "password_created",
            "initial admin password generated",
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(path))
}

/// Login request body.
#[derive(Deserialize)]
pub struct LoginBody {
    password: String,
}

/// `POST /api/v1/auth/login`
pub async fn login(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginBody>,
) -> Response {
    let ip = peer.ip();
    if s.auth.failures.get(&ip).unwrap_or(0) >= MAX_FAILURES {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many failed logins, try again in 15 minutes"})),
        )
            .into_response();
    }
    let storage = &s.resolver.storage;
    let Ok(Some(hash)) = storage.get_setting(HASH_KEY).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "no admin password set"})),
        )
            .into_response();
    };
    let ok = tokio::task::spawn_blocking(move || verify_password(&body.password, &hash))
        .await
        .unwrap_or(false);
    if !ok {
        let n = s.auth.failures.get(&ip).unwrap_or(0) + 1;
        s.auth.failures.insert(ip, n);
        let _ = storage
            .audit(&ip.to_string(), "login_failed", "wrong password")
            .await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "wrong password"})),
        )
            .into_response();
    }
    s.auth.failures.invalidate(&ip);
    let token = random_token(43);
    let csrf = random_token(32);
    s.auth
        .sessions
        .insert(token.clone(), Arc::new(Session { csrf: csrf.clone() }));
    let _ = storage.audit("admin", "login", &format!("from {ip}")).await;
    let mut resp = Json(serde_json::json!({ "csrf": csrf })).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, s.auth.cookie(&token, 12 * 3600));
    resp
}

/// `POST /api/v1/auth/logout`
pub async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some((token, _)) = s.auth.session_from(&headers) {
        s.auth.sessions.invalidate(&token);
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, s.auth.cookie("", 0));
    resp
}

/// `GET /api/v1/auth/session`: the CSRF token for the current session, or 401.
pub async fn session(State(s): State<AppState>, headers: HeaderMap) -> Response {
    match s.auth.session_from(&headers) {
        Some((_, sess)) => Json(serde_json::json!({ "csrf": sess.csrf })).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Password change body.
#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

/// `POST /api/v1/auth/password`
pub async fn change_password(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    if body.new.chars().count() < MIN_PASSWORD_LEN {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("new password must be at least {MIN_PASSWORD_LEN} characters")}))).into_response();
    }
    let storage = &s.resolver.storage;
    let Ok(Some(hash)) = storage.get_setting(HASH_KEY).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let current = body.current;
    if !tokio::task::spawn_blocking(move || verify_password(&current, &hash))
        .await
        .unwrap_or(false)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "current password is wrong"})),
        )
            .into_response();
    }
    let new = body.new;
    let Ok(Ok(new_hash)) = tokio::task::spawn_blocking(move || hash_password(&new)).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if storage.set_setting(HASH_KEY, &new_hash).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    // Sign out every other session.
    let keep = s.auth.session_from(&headers).map(|(t, _)| t);
    let others: Vec<String> = s
        .auth
        .sessions
        .iter()
        .map(|(k, _)| (*k).clone())
        .filter(|k| Some(k) != keep.as_ref())
        .collect();
    for k in others {
        s.auth.sessions.invalidate(&k);
    }
    let _ = storage
        .audit("admin", "password_changed", "admin password changed")
        .await;
    StatusCode::NO_CONTENT.into_response()
}

/// Requires a session for every API route except login; non-GET requests also need the CSRF header.
pub async fn require_session(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let Some((_, sess)) = s.auth.session_from(req.headers()) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "not signed in"})),
        )
            .into_response();
    };
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        let sent = req
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !same(sent, &sess.csrf) {
            warn!("request with missing or wrong CSRF token");
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "missing or wrong CSRF token"})),
            )
                .into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn constant_time_compare() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn tokens_are_random_and_sized() {
        let a = random_token(43);
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_token(43));
    }
}
