use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State as AxumState};
use axum::http::StatusCode;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::secondbrain::{self, State};

/// Shared, atomically-swappable snapshot of the secondbrain.
pub type Shared = Arc<RwLock<State>>;

/// `GET /api/state` — the current portfolio snapshot.
pub async fn get_state(AxumState(shared): AxumState<Shared>) -> Json<State> {
    Json(shared.read().await.clone())
}

/// A rendered note for the detail view.
#[derive(Serialize)]
pub struct Note {
    name: String,
    html: String,
}

/// `GET /api/note/{name}` — the rendered markdown body of a project/area note.
pub async fn get_note(
    AxumState(shared): AxumState<Shared>,
    Path(name): Path<String>,
) -> Result<Json<Note>, StatusCode> {
    let path = shared.read().await.note_paths.get(&name).cloned();
    let Some(path) = path else {
        return Err(StatusCode::NOT_FOUND);
    };
    match secondbrain::render_note(&path) {
        Ok(html) => Ok(Json(Note { name, html })),
        Err(e) => {
            tracing::warn!(error = %e, "rendering note");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `GET /healthz` — liveness probe.
pub async fn healthz() -> &'static str {
    "ok"
}
