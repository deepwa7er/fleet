use std::sync::Arc;

use axum::Json;
use axum::extract::State as AxumState;
use tokio::sync::RwLock;

use crate::secondbrain::State;

/// Shared, atomically-swappable snapshot of the secondbrain.
pub type Shared = Arc<RwLock<State>>;

/// `GET /api/state` — the current portfolio snapshot.
pub async fn get_state(AxumState(shared): AxumState<Shared>) -> Json<State> {
    Json(shared.read().await.clone())
}

/// `GET /healthz` — liveness probe.
pub async fn healthz() -> &'static str {
    "ok"
}
