//! depot's HTTP surface, fronted by breakwater at
//! <https://depot.intern.deepwa7er.net>.
//!
//! Two kinds of endpoint: one **write** path for emitters that can't be pulled
//! (tugboat runs on the dev box, which sleeps, so it pushes), and **read** paths
//! that answer the questions the warehouse exists for.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Json, Query, State};
use axum::routing::{get, post};
use axum::Router;
use fleet_common::http::Result;
use serde::{Deserialize, Serialize};

use crate::store::{DeployRecord, HostUsage, Store};

pub struct Config {
    pub addr: SocketAddr,
    pub web_dir: PathBuf,
}

#[derive(Deserialize)]
pub struct UsageQuery {
    /// Window, in days. Defaults to a week.
    #[serde(default = "default_days")]
    days: i64,
    /// Include lighthouse's reachability probe. Off by default — see
    /// [`crate::store::PROBE_USER_AGENT`].
    #[serde(default)]
    include_probe: bool,
}

fn default_days() -> i64 {
    7
}

#[derive(Deserialize)]
pub struct DeploysQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// What a push added — `stored: false` means depot already had it, which is a
/// normal outcome of a retry, not an error.
#[derive(Serialize)]
pub struct Ingested {
    stored: bool,
}

pub async fn run(store: Arc<Store>, config: Config) -> std::io::Result<()> {
    let app = Router::new()
        .route("/healthz", get(fleet_common::http::healthz))
        .route("/api/summary", get(summary))
        .route("/api/usage", get(usage))
        .route("/api/deploys", get(deploys).post(push_deploy))
        .route("/api/events/deploy", post(push_deploy))
        .fallback_service(fleet_common::http::spa(&config.web_dir))
        .with_state(store);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("depot on {}", config.addr);
    axum::serve(listener, app).await
}

async fn summary(State(store): State<Arc<Store>>) -> Result<Json<crate::store::Summary>> {
    Ok(Json(store.summary()?))
}

async fn usage(
    State(store): State<Arc<Store>>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Vec<HostUsage>>> {
    // Clamp rather than reject: a nonsensical window is a caller bug, not a
    // reason to fail a dashboard.
    let days = q.days.clamp(1, 3_650);
    let since_ms = now_ms() - days * 86_400_000;
    Ok(Json(store.usage_since(since_ms, q.include_probe)?))
}

async fn deploys(
    State(store): State<Arc<Store>>,
    Query(q): Query<DeploysQuery>,
) -> Result<Json<Vec<DeployRecord>>> {
    Ok(Json(store.recent_deploys(q.limit.clamp(1, 1_000))?))
}

/// Accept one deploy event from tugboat. Idempotent: re-pushing an event depot
/// already holds is a success, so a retry after a network failure is always safe.
async fn push_deploy(
    State(store): State<Arc<Store>>,
    Json(record): Json<DeployRecord>,
) -> Result<Json<Ingested>> {
    let stored = store.insert_deploy(&record)?;
    Ok(Json(Ingested { stored }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
