mod alerts;
mod api;
mod config;
mod fleet;
mod systemd;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::routing::{get, post};
use clap::Parser;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::Config;

const DEFAULT_CONFIG_PATH: &str = "/etc/lighthouse/config.toml";

/// Lighthouse — a dashboard for the status and logs of systemd services.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the configuration file. Created from the baseline if absent.
    #[arg(short, long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
}

/// Shared, read-only application state.
pub struct AppState {
    pub config: Config,
    /// HTTP client for relaying deploy requests to the tugboat daemon.
    pub http: reqwest::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load up front so a broken config fails at startup, and to learn the
    // listen address, port, and static directory.
    let config = Config::load_or_create(&cli.config)?;
    let addr = SocketAddr::new(config.bind, config.port);
    // The frontend does client-side path routing (e.g. /services/<unit>), so a
    // direct navigation or refresh on such a path must return the SPA shell
    // rather than 404. `/` and real asset paths are served as files; every
    // other non-API path falls back to index.html so the app can route.
    let serve_dir = ServeDir::new(&config.static_dir)
        .fallback(ServeFile::new(config.static_dir.join("index.html")));

    // Start the alert watcher in the background (it no-ops if no [alerts] config).
    if config.alerts.is_some() {
        tokio::spawn(alerts::run(config.clone()));
    }

    let state = Arc::new(AppState {
        config,
        http: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/api/services", get(api::list_services))
        .route("/api/services/{unit}/logs", get(api::get_logs))
        .route("/api/services/{unit}/logs/stream", get(api::stream_logs))
        .route(
            "/api/services/{unit}/control/{action}",
            post(api::control_service),
        )
        .route("/api/deploy-status", get(api::deploy_status))
        .route(
            "/api/services/{unit}/deploy-history",
            get(api::deploy_history),
        )
        .route(
            "/api/services/{unit}/deploy-log/{id}",
            get(api::deploy_log),
        )
        .route("/api/services/{unit}/deploy", post(api::deploy_service))
        .route(
            "/api/services/{unit}/deploy/{job}/stream",
            get(api::deploy_stream),
        )
        .fallback_service(serve_dir)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    println!("lighthouse listening on http://{addr}");
    axum::serve(listener, app).await.context("server error")?;
    Ok(())
}
