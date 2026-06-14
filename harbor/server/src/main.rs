//! harbor-server — reads a checkout of the secondbrain repo, parses the
//! project/area frontmatter, enriches it with live GitHub commit activity and
//! lighthouse VPS service health, and serves it as JSON for the harbor new-tab
//! extension. Designed to run on the tailnet (see `harbor.toml`).

mod api;
mod config;
mod git;
mod github;
mod lighthouse;
mod secondbrain;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::Router;
use axum::http::Method;
use axum::routing::get;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::config::Config;
use crate::github::Activity;
use crate::secondbrain::{Entry, State};

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

    let updater = Arc::new(Updater::new(config.clone())?);

    // Build the first snapshot before serving, so the first request gets data.
    let initial = updater.build().await.context("building initial snapshot")?;
    let shared: api::Shared = Arc::new(RwLock::new(initial));

    spawn_refresh_loop(Arc::clone(&updater), config.refresh_secs, Arc::clone(&shared));

    let cors = build_cors(&config.allow_origin)?;
    let app = Router::new()
        .route("/api/state", get(api::get_state))
        .route("/api/note/{name}", get(api::get_note))
        .route("/healthz", get(api::healthz))
        // Self-hosted extension distribution: serves harbor.crx + updates.xml so
        // tailnet devices force-install and auto-update over the tailnet.
        .nest_service("/extension", ServeDir::new(&config.extension_dir))
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

/// Cached recent-commit list with its fetch time, for TTL expiry.
struct CachedActivity {
    fetched: Instant,
    value: Vec<Activity>,
}

/// How many recent commits to fetch per project.
const RECENT_COMMITS: u32 = 5;

/// Builds snapshots: git sync + frontmatter parse (blocking), then async
/// enrichment — GitHub commit activity (TTL-cached) and lighthouse health.
struct Updater {
    config: Config,
    http: reqwest::Client,
    activity: Mutex<HashMap<String, CachedActivity>>,
}

impl Updater {
    fn new(config: Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            config,
            http,
            activity: Mutex::new(HashMap::new()),
        })
    }

    /// Produce a fresh snapshot.
    async fn build(&self) -> Result<State> {
        let source_dir = self.config.source_dir.clone();
        let remote = self.config.git_remote.clone();
        let branch = self.config.branch.clone();

        // git + filesystem work is blocking; keep it off the async runtime.
        let mut state = tokio::task::spawn_blocking(move || -> Result<State> {
            if let Some(remote) = remote.as_deref() {
                git::sync(&source_dir, remote, &branch)
                    .with_context(|| format!("syncing {source_dir:?} from {remote}"))?;
            }
            secondbrain::build(&source_dir, remote.as_deref())
        })
        .await
        .context("refresh task panicked")??;

        self.attach_recent(&mut state.projects).await;

        if let Some(url) = self.config.lighthouse_url.as_deref() {
            match lighthouse::services(&self.http, url).await {
                Ok(services) => state.services = services,
                Err(e) => tracing::warn!(error = %e, "lighthouse health fetch failed"),
            }
        }

        Ok(state)
    }

    /// Attach recent commits to each project that has a `repo`, fetching from
    /// GitHub only for repos whose cached value has expired.
    async fn attach_recent(&self, projects: &mut [Entry]) {
        let ttl = Duration::from_secs(self.config.activity_ttl_secs);
        let token = self.config.github_token.clone();

        let mut repos: Vec<String> = projects.iter().filter_map(|p| p.repo.clone()).collect();
        repos.sort();
        repos.dedup();

        // Split into cache hits and repos that need a fetch.
        let mut resolved: HashMap<String, Vec<Activity>> = HashMap::new();
        let mut need: Vec<String> = Vec::new();
        {
            let cache = self.activity.lock().await;
            for repo in &repos {
                match cache.get(repo) {
                    Some(c) if c.fetched.elapsed() < ttl => {
                        resolved.insert(repo.clone(), c.value.clone());
                    }
                    _ => need.push(repo.clone()),
                }
            }
        }

        if !need.is_empty() {
            let results = futures::future::join_all(need.into_iter().map(|repo| {
                let token = token.clone();
                async move {
                    let commits =
                        github::recent_commits(&self.http, token.as_deref(), &repo, RECENT_COMMITS)
                            .await;
                    (repo, commits)
                }
            }))
            .await;

            let mut cache = self.activity.lock().await;
            for (repo, result) in results {
                let value = result.unwrap_or_else(|e| {
                    tracing::warn!(repo = %repo, error = %e, "activity fetch failed");
                    Vec::new()
                });
                cache.insert(
                    repo.clone(),
                    CachedActivity {
                        fetched: Instant::now(),
                        value: value.clone(),
                    },
                );
                resolved.insert(repo, value);
            }
        }

        for project in projects.iter_mut() {
            if let Some(repo) = &project.repo {
                project.recent = resolved.get(repo).cloned().unwrap_or_default();
            }
        }
    }
}

/// Periodically rebuild the shared snapshot. A failed refresh is logged and the
/// previous snapshot keeps serving.
fn spawn_refresh_loop(updater: Arc<Updater>, refresh_secs: u64, shared: api::Shared) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(refresh_secs));
        ticker.tick().await; // consume the immediate first tick; we already built once
        loop {
            ticker.tick().await;
            match updater.build().await {
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
