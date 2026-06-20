//! HTTP handlers.
//!
//! The set of monitored services is discovered at request time from the
//! configured systemd target (plus any units pinned in the config). That same
//! set is the allowlist: every handler that touches a unit first resolves it
//! against the monitored set, and an unknown unit returns `404` before any
//! subprocess runs.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
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

/// One entry of the tugboat daemon's `/services` response.
#[derive(Debug, Deserialize)]
struct TugboatService {
    name: String,
    manifest_present: bool,
}

/// `GET /api/deployable` — the units the dashboard can deploy: monitored units
/// whose tugboat service exists and has a manifest. Returns an empty list (no
/// Deploy buttons) when deploy integration is unconfigured or the daemon is
/// unreachable, so the dashboard degrades gracefully.
pub async fn deployable_units(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    let Some(cfg) = &state.config.deploy else {
        return Json(Vec::new());
    };
    let url = format!("{}/services", cfg.tugboat_url.trim_end_matches('/'));
    let services: Vec<TugboatService> = match state
        .http
        .get(&url)
        .bearer_auth(&cfg.token)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(resp) => match resp.json().await {
            Ok(services) => services,
            Err(err) => {
                eprintln!("parsing tugboat /services failed: {err}");
                return Json(Vec::new());
            }
        },
        Err(err) => {
            eprintln!("tugboat /services unreachable: {err}");
            return Json(Vec::new());
        }
    };

    let deployable: HashSet<String> = services
        .into_iter()
        .filter(|s| s.manifest_present)
        .map(|s| s.name)
        .collect();

    let units = monitored(&state.config)
        .await
        .into_iter()
        .map(|m| m.unit)
        .filter(|unit| deployable.contains(&state.config.deploy_name(unit)))
        .collect();
    Json(units)
}

/// `POST /api/services/{unit}/deploy` — relay a deploy request to the tugboat
/// daemon. The daemon's response (a `{job_id}` on success, or an error status)
/// is passed through unchanged; the token never leaves this server.
pub async fn deploy_service(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
) -> Response {
    let svc = match resolve(&state.config, &unit).await {
        Ok(svc) => svc,
        Err(code) => return (code, "unknown service").into_response(),
    };
    let Some(cfg) = &state.config.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy integration not configured").into_response();
    };

    let name = state.config.deploy_name(&svc.unit);
    let url = format!("{}/deploy/{}", cfg.tugboat_url.trim_end_matches('/'), name);
    let resp = match state.http.post(&url).bearer_auth(&cfg.token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("deploy relay to {url} failed: {err}");
            return (StatusCode::BAD_GATEWAY, format!("tugboat daemon unreachable: {err}"))
                .into_response();
        }
    };

    let status = resp.status();
    match resp.bytes().await {
        Ok(body) => (status, [(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Err(err) => {
            (StatusCode::BAD_GATEWAY, format!("reading tugboat response: {err}")).into_response()
        }
    }
}

/// `GET /api/services/{unit}/deploy/{job}/stream` — proxy the daemon's live
/// deploy transcript (Server-Sent Events) straight through to the browser.
pub async fn deploy_stream(
    State(state): State<Arc<AppState>>,
    Path((unit, job)): Path<(String, String)>,
) -> Response {
    if let Err(code) = resolve(&state.config, &unit).await {
        return (code, "unknown service").into_response();
    }
    let Some(cfg) = &state.config.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy integration not configured").into_response();
    };

    let url = format!("{}/jobs/{}/stream", cfg.tugboat_url.trim_end_matches('/'), job);
    let resp = match state.http.get(&url).bearer_auth(&cfg.token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            return (StatusCode::BAD_GATEWAY, format!("tugboat daemon unreachable: {err}"))
                .into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.bytes().await.unwrap_or_default();
        return (status, body).into_response();
    }

    let body = Body::from_stream(resp.bytes_stream());
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}
