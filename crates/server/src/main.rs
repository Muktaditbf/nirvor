//! nirvor: resilient caching DNS resolver.

use anyhow::{Context, Result, bail};
use clap::Parser;
use nirvor_core::{Config, Server};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Resilient caching DNS resolver")]
struct Args {
    /// Path to config.toml.
    #[arg(short, long, default_value = "config/config.toml")]
    config: PathBuf,
    /// Log as JSON lines instead of text.
    #[arg(long)]
    json_logs: bool,
    /// Check the config file and exit.
    #[arg(long)]
    check_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // TLS (root zone, blocklist downloads, HTTPS dashboard) uses ring, which the DNSSEC code already links.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if args.json_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", args.config.display()))?;
    if let Err(e) = config.validate() {
        bail!("invalid config: {e}");
    }
    if args.check_config {
        println!("{} is valid", args.config.display());
        return Ok(());
    }

    let server = Server::bind(&config).await.context("binding listeners")?;
    let resolver = server.resolver();
    if let Some(path) =
        nirvor_api::auth::ensure_admin_password(&resolver.storage, &config.resilience.data_dir)
            .await
            .map_err(anyhow::Error::msg)?
    {
        warn!(file = %path.display(), "first start: an admin password was generated; read it from this file, sign in, change it, then delete the file");
    }

    let tls = config.api.tls_cert.is_some();
    let state = nirvor_api::AppState {
        resolver: resolver.clone(),
        auth: nirvor_api::Auth::new(tls),
        config_path: args.config.clone(),
    };
    let app = nirvor_api::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let handle = axum_server::Handle::new();
    let api = {
        let handle = handle.clone();
        let addr = config.api.listen;
        let tls_files = config.api.tls_cert.clone().zip(config.api.tls_key.clone());
        async move {
            match tls_files {
                Some((cert, key)) => {
                    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                        .await
                        .with_context(|| format!("loading TLS certificate {}", cert.display()))?;
                    info!(%addr, "dashboard and API listening (HTTPS)");
                    axum_server::bind_rustls(addr, tls)
                        .handle(handle)
                        .serve(app)
                        .await
                        .context("API server")
                }
                None => {
                    info!(%addr, "dashboard and API listening (HTTP)");
                    axum_server::bind(addr)
                        .handle(handle)
                        .serve(app)
                        .await
                        .context("API server")
                }
            }
        }
    };
    if let Some(addr) = config.isolation.connectivity_responder {
        let l = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding connectivity responder on {addr}"))?;
        info!(%addr, "connectivity-check responder listening");
        tokio::spawn(async move { axum::serve(l, nirvor_api::connectivity::router()).await });
    }

    tokio::select! {
        r = server.run() => r.context("DNS server stopped")?,
        r = api => r?,
        _ = shutdown_signal() => info!("shutting down"),
    }
    // Graceful shutdown: stop the dashboard, give in-flight DNS queries a moment, then persist the cache.
    handle.graceful_shutdown(Some(Duration::from_secs(2)));
    tokio::time::sleep(Duration::from_millis(500)).await;
    match resolver.save_snapshot() {
        Ok(n) => info!(entries = n, "cache snapshot saved"),
        Err(e) => warn!(error = %e, "could not save cache snapshot"),
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
