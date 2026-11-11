//! API tests against a real server: auth, CSRF, login limits, config validation, headers.

use nirvor_api::{AppState, Auth, auth, router};
use nirvor_core::Server;
use nirvor_core::config::Config;
use reqwest::StatusCode;
use std::net::SocketAddr;

async fn start() -> (String, String, std::path::PathBuf) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = std::env::temp_dir().join(format!(
        "nirvor-api-test-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("config.toml");
    let text = format!(
        "[server]\nlisten = [\"127.0.0.1:0\"]\n\n[[upstream.servers]]\nname = \"x\"\naddr = \"127.0.0.1:9\"\n\n[resilience]\ndata_dir = \"{}\"\n\n[resilience.root_zone]\nenabled = false\n",
        dir.display()
    );
    std::fs::write(&cfg_path, &text).unwrap();
    let config: Config = toml::from_str(&text).unwrap();
    let server = Server::bind(&config).await.unwrap();
    let resolver = server.resolver();
    let pw_file = auth::ensure_admin_password(&resolver.storage, &dir)
        .await
        .unwrap()
        .unwrap();
    let password = std::fs::read_to_string(pw_file).unwrap().trim().to_string();
    let app = router(AppState {
        resolver,
        auth: Auth::new(false),
        config_path: cfg_path.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    tokio::spawn(server.run());
    (format!("http://{addr}"), password, cfg_path)
}

fn rand_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

async fn login(base: &str, password: &str) -> (String, String) {
    let r = reqwest::Client::new()
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
        "{cookie}"
    );
    let session = cookie.split(';').next().unwrap().to_string();
    let csrf = r.json::<serde_json::Value>().await.unwrap()["csrf"]
        .as_str()
        .unwrap()
        .to_string();
    (session, csrf)
}

#[tokio::test]
async fn auth_session_and_csrf() {
    let (base, password, _) = start().await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("{base}/api/v1/overview"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "no session");
    assert_eq!(r.headers()["x-frame-options"], "DENY");

    let r = c
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({ "password": "wrong" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let (session, csrf) = login(&base, &password).await;
    let r = c
        .get(format!("{base}/api/v1/overview"))
        .header("cookie", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_ne!(
        body["mode"]["mode"], "isolation",
        "dead upstream: degraded, not yet isolated"
    );

    let mode = serde_json::json!({ "control": "isolation" });
    let r = c
        .post(format!("{base}/api/v1/resilience/mode"))
        .header("cookie", &session)
        .json(&mode)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "missing CSRF header");
    let r = c
        .post(format!("{base}/api/v1/resilience/mode"))
        .header("cookie", &session)
        .header("x-csrf-token", &csrf)
        .json(&mode)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["mode"],
        "isolation"
    );

    let audit: serde_json::Value = c
        .get(format!("{base}/api/v1/audit"))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let actions: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(
        actions.contains(&"login")
            && actions.contains(&"login_failed")
            && actions.contains(&"mode_change"),
        "{actions:?}"
    );

    let r = c
        .post(format!("{base}/api/v1/auth/logout"))
        .header("cookie", &session)
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = c
        .get(format!("{base}/api/v1/overview"))
        .header("cookie", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "session ended");
}

#[tokio::test]
async fn login_attempts_are_limited() {
    let (base, password, _) = start().await;
    let c = reqwest::Client::new();
    for _ in 0..10 {
        c.post(format!("{base}/api/v1/auth/login"))
            .json(&serde_json::json!({ "password": "nope" }))
            .send()
            .await
            .unwrap();
    }
    let r = c
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "even the right password is refused while locked"
    );
}

#[tokio::test]
async fn config_edit_validates_applies_and_saves() {
    let (base, password, cfg_path) = start().await;
    let (session, csrf) = login(&base, &password).await;
    let c = reqwest::Client::new();
    let put = |body: serde_json::Value| {
        c.put(format!("{base}/api/v1/config"))
            .header("cookie", &session)
            .header("x-csrf-token", &csrf)
            .json(&body)
            .send()
    };
    let r = put(serde_json::json!({ "toml": "this is not toml" }))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let current: serde_json::Value = c
        .get(format!("{base}/api/v1/config"))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut text = current["toml"].as_str().unwrap().to_string();
    let open = text.replace("\"127.0.0.0/8\"", "\"0.0.0.0/0\"");
    let r = put(serde_json::json!({ "toml": open })).await.unwrap();
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "open resolver config rejected"
    );
    text = text.replace("pinned_domains = []", "pinned_domains = [\"example.bd\"]");
    let r = put(serde_json::json!({ "toml": text })).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK, "{:?}", r.text().await);
    assert!(
        std::fs::read_to_string(cfg_path)
            .unwrap()
            .contains("example.bd"),
        "saved to disk"
    );
}
