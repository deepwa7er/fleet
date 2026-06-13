//! HTTP handlers.
//!
//! The set of monitored services is discovered at request time from the
//! configured systemd target (plus any units pinned in the config). That same
//! set is the allowlist: every handler that touches a unit first resolves it
//! against the monitored set, and an unknown unit returns `404` before any
//! subprocess runs.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt};

use crate::AppState;
use crate::config::Config;
use crate::systemd::{self, ServiceStatus};

const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 5_000;

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    lines: Option<u32>,
}

/// A monitored service: its unit name and resolved display label.
struct Monitored {
    unit: String,
    name: String,
}

/// `GET /api/services` — status of every monitored service.
pub async fn list_services(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ServiceStatus>>, StatusCode> {
    let services = monitored(&state.config).await;
    let mut statuses = Vec::with_capacity(services.len());
    for svc in services {
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
    let svc = resolve(&state.config, &unit).await?;
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES).clamp(1, MAX_LOG_LINES);

    match systemd::recent_logs(&svc.unit, lines).await {
        Ok(entries) => Ok(Json(entries)),
        Err(err) => {
            eprintln!("failed to read logs for {}: {err:#}", svc.unit);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /api/services/{unit}/control/{action}` — start, stop, or restart a
/// service. Returns the unit's status after the action completes so the UI can
/// update immediately.
pub async fn control_service(
    State(state): State<Arc<AppState>>,
    Path((unit, action)): Path<(String, String)>,
) -> Result<Json<ServiceStatus>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let action = systemd::ServiceAction::parse(&action).ok_or(StatusCode::BAD_REQUEST)?;

    if let Err(err) = systemd::control(&svc.unit, action).await {
        eprintln!("failed to {action:?} {}: {err:#}", svc.unit);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    match systemd::status(&svc.unit, &svc.name).await {
        Ok(status) => Ok(Json(status)),
        Err(err) => {
            eprintln!("failed to read status for {} after {action:?}: {err:#}", svc.unit);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `GET /api/services/{unit}/logs/stream` — live log tail over Server-Sent Events.
pub async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;

    let entries = systemd::follow_logs(&svc.unit).map_err(|err| {
        eprintln!("failed to follow logs for {}: {err:#}", svc.unit);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let events = entries.map(|entry| Event::default().json_data(entry));
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// The monitored set: units that are members of the target, unioned with units
/// pinned in the config, each paired with its display label. This is both the
/// dashboard's service list and the allowlist for logs/control.
async fn monitored(config: &Config) -> Vec<Monitored> {
    let mut units = match systemd::discover_units(&config.target).await {
        Ok(units) => units,
        Err(err) => {
            eprintln!("service discovery failed for target {}: {err:#}", config.target);
            Vec::new()
        }
    };
    for pinned in config.pinned_units() {
        if !units.iter().any(|u| u == pinned) {
            units.push(pinned.to_owned());
        }
    }
    units.sort();
    units.dedup();
    units
        .into_iter()
        .map(|unit| {
            let name = config.display_name(&unit);
            Monitored { unit, name }
        })
        .collect()
}

/// Resolve a single unit against the monitored set (the allowlist gate). Unknown
/// units yield `404` before any command runs.
async fn resolve(config: &Config, unit: &str) -> Result<Monitored, StatusCode> {
    monitored(config)
        .await
        .into_iter()
        .find(|m| m.unit == unit)
        .ok_or(StatusCode::NOT_FOUND)
}
