//! harbor-server — reads a checkout of the secondbrain repo, parses the
//! project/area frontmatter, and serves it as JSON for the harbor new-tab
//! extension. Designed to run on the tailnet (see `harbor.toml`).

mod api;
mod config;
mod git;
mod secondbrain;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::Method;
use axum::routing::get;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "harbor_server=info".into()),
        )
        .init();

    let config_path = config_path_from_args();
    let config = Config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;
    tracing::info!(path = %config_path.display(), "loaded config");

    // Build the first snapshot before we start serving, so the very first
    // request gets real data rather than an empty page.
    let initial = refresh(&config)
        .await
        .context("building initial snapshot")?;
    let shared: api::Shared = Arc::new(RwLock::new(initial));

    spawn_refresh_loop(config.clone(), Arc::clone(&shared));

    let cors = build_cors(&config.allow_origin)?;
    let app = Router::new()
        .route("/api/state", get(api::get_state))
        .route("/healthz", get(api::healthz))
        .layer(cors)
        .with_state(shared);

    let addr = SocketAddr::new(config.bind, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "harbor listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

/// Re-sync the secondbrain (if a remote is configured) and rebuild the snapshot.
async fn refresh(config: &Config) -> Result<secondbrain::State> {
    let source_dir = config.source_dir.clone();
    let remote = config.git_remote.clone();
    let branch = config.branch.clone();

    // git + filesystem work is blocking; keep it off the async runtime.
    tokio::task::spawn_blocking(move || -> Result<secondbrain::State> {
        if let Some(remote) = remote.as_deref() {
            git::sync(&source_dir, remote, &branch)
                .with_context(|| format!("syncing {source_dir:?} from {remote}"))?;
        }
        secondbrain::build(&source_dir, remote.as_deref())
    })
    .await
    .context("refresh task panicked")?
}

/// Periodically refresh the shared snapshot. A failed refresh is logged and the
/// previous snapshot is kept serving.
fn spawn_refresh_loop(config: Config, shared: api::Shared) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(config.refresh_secs));
        ticker.tick().await; // consume the immediate first tick; we already built once
        loop {
            ticker.tick().await;
            match refresh(&config).await {
                Ok(state) => {
                    let n = state.projects.len() + state.areas.len();
                    *shared.write().await = state;
                    tracing::info!(entries = n, "refreshed snapshot");
                }
                Err(e) => tracing::warn!(error = %e, "refresh failed; keeping previous snapshot"),
            }
        }
    });
}

fn build_cors(allow_origin: &str) -> Result<CorsLayer> {
    let layer = CorsLayer::new().allow_methods([Method::GET]);
    if allow_origin == "*" {
        Ok(layer.allow_origin(Any))
    } else {
        let origin: axum::http::HeaderValue = allow_origin
            .parse()
            .with_context(|| format!("invalid allow_origin {allow_origin:?}"))?;
        Ok(layer.allow_origin(origin))
    }
}

/// Config path: first CLI arg, or `harbor.toml` in the working directory.
fn config_path_from_args() -> PathBuf {
    std::env::args()
        .nth(1)
        .map_or_else(|| Path::new("harbor.toml").to_path_buf(), PathBuf::from)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
