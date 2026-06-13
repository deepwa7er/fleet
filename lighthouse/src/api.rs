//! HTTP handlers. Every handler that touches a service first looks the unit up
//! in the configured allowlist; an unconfigured unit returns `404` and no
//! subprocess is ever spawned for it.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt};

use crate::AppState;
use crate::systemd::{self, ServiceStatus};

const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 5_000;

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    lines: Option<u32>,
}

/// `GET /api/services` — status of every configured service.
pub async fn list_services(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ServiceStatus>>, StatusCode> {
    let mut statuses = Vec::with_capacity(state.config.services.len());
    for svc in &state.config.services {
        match systemd::status(&svc.unit, &svc.name).await {
            Ok(status) => statuses.push(status),
            Err(err) => {
                eprintln!("failed to query {}: {err:#}", svc.unit);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
    Ok(Json(statuses))
}

/// `GET /api/services/{unit}/logs?lines=N` — most recent log entries.
pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Vec<systemd::LogEntry>>, StatusCode> {
    let unit = allowlisted_unit(&state, &unit)?;
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES).clamp(1, MAX_LOG_LINES);

    match systemd::recent_logs(&unit, lines).await {
        Ok(entries) => Ok(Json(entries)),
        Err(err) => {
            eprintln!("failed to read logs for {unit}: {err:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `GET /api/services/{unit}/logs/stream` — live log tail over Server-Sent Events.
pub async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, StatusCode> {
    let unit = allowlisted_unit(&state, &unit)?;

    let entries = systemd::follow_logs(&unit).map_err(|err| {
        eprintln!("failed to follow logs for {unit}: {err:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let events = entries.map(|entry| Event::default().json_data(entry));
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// Resolve `unit` against the configured allowlist, returning its canonical unit
/// name. Unknown units yield `404` before any command runs.
fn allowlisted_unit(state: &AppState, unit: &str) -> Result<String, StatusCode> {
    state
        .config
        .find_unit(unit)
        .map(|svc| svc.unit.clone())
        .ok_or(StatusCode::NOT_FOUND)
}
