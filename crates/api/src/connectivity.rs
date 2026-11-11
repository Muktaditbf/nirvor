//! Built-in responder for OS connectivity checks, used with isolation overrides so devices keep
//! showing "connected" while only local services are reachable. Plain HTTP; checks over HTTPS
//! cannot be answered locally.

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

const APPLE: &str = "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>";
const FIREFOX: &str = "<meta http-equiv=\"refresh\" content=\"0;url=https://support.mozilla.org/kb/captive-portal\"/>";

fn text(body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/plain"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}

fn html(body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}

/// Routes answering the well-known check URLs of Android/ChromeOS, Apple, Windows, Firefox and Linux desktops.
pub fn router() -> Router {
    Router::new()
        // Android, ChromeOS, Ubuntu (connectivity-check.ubuntu.com/)
        .route("/generate_204", get(|| async { StatusCode::NO_CONTENT }))
        .route("/gen_204", get(|| async { StatusCode::NO_CONTENT }))
        .route("/", get(|| async { StatusCode::NO_CONTENT }))
        // Apple captive.apple.com
        .route("/hotspot-detect.html", get(|| async { html(APPLE) }))
        .route("/library/test/success.html", get(|| async { html(APPLE) }))
        // Windows NCSI (also needs dns.msftncsi.com → 131.107.255.255 in the overrides)
        .route(
            "/connecttest.txt",
            get(|| async { text("Microsoft Connect Test") }),
        )
        .route("/ncsi.txt", get(|| async { text("Microsoft NCSI") }))
        // Firefox detectportal.firefox.com
        .route("/success.txt", get(|| async { text("success\n") }))
        .route("/canonical.html", get(|| async { html(FIREFOX) }))
        // NetworkManager on Arch and Fedora
        .route(
            "/nm-check.txt",
            get(|| async { text("NetworkManager is online\n") }),
        )
        .route("/static/hotspot.txt", get(|| async { text("OK") }))
}
