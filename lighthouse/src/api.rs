//! HTTP handlers.
//!
//! Every monitored entity has a [`Source`] (systemd or Docker) and an id. The
//! routes carry both (`/api/services/{source}/{id}/...`), and each handler
//! resolves the id against that source's allowlist — the systemd target (plus
//! pinned units) or the configured Docker container list — returning `404` for
//! anything off the list before it touches `systemctl` or the Docker proxy.

use std::pin::Pin;
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
use crate::model::{LogEntry, ServiceAction, ServiceStatus, Source};
use crate::{docker, systemd};

const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 5_000;

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    lines: Option<u32>,
}

/// A monitored systemd unit: its name and resolved display label.
struct Monitored {
    unit: String,
    name: String,
}

/// `GET /api/services` — status of every monitored service across all sources.
pub async fn list_services(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ServiceStatus>>, StatusCode> {
    let mut statuses = Vec::new();

    for svc in systemd_monitored(&state.config).await {
        match systemd::status(&svc.unit, &svc.name).await {
            Ok(status) => statuses.push(status),
            Err(err) => {
                eprintln!("failed to query {}: {err:#}", svc.unit);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    if let Some(docker_cfg) = &state.config.docker {
        for container in &docker_cfg.containers {
            let name = container.display_name();
            match docker::status(&state.http, &docker_cfg.proxy_url, &container.container, &name)
                .await
            {
                Ok(status) => statuses.push(status),
                // A proxy/transport failure shouldn't blank the whole dashboard
                // or hide the container — surface it as an unreachable row.
                Err(err) => {
                    eprintln!("failed to query container {}: {err:#}", container.container);
                    statuses.push(unreachable_status(&container.container, &name));
                }
            }
        }
    }

    Ok(Json(statuses))
}

/// `GET /api/services/{source}/{id}/logs?lines=N` — most recent log entries.
pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path((source, id)): Path<(String, String)>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Vec<LogEntry>>, StatusCode> {
    let source = Source::parse(&source).ok_or(StatusCode::NOT_FOUND)?;
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES).clamp(1, MAX_LOG_LINES);

    let result = match source {
        Source::Systemd => {
            let svc = resolve_systemd(&state.config, &id).await?;
            systemd::recent_logs(&svc.unit, lines).await
        }
        Source::Docker => {
            let (docker_cfg, _) = resolve_docker(&state.config, &id)?;
            docker::recent_logs(&state.http, &docker_cfg.proxy_url, &id, lines).await
        }
    };

    match result {
        Ok(entries) => Ok(Json(entries)),
        Err(err) => {
            eprintln!("failed to read logs for {source:?} {id}: {err:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /api/services/{source}/{id}/control/{action}` — start, stop, or
/// restart, returning the post-action status so the UI can update at once.
pub async fn control_service(
    State(state): State<Arc<AppState>>,
    Path((source, id, action)): Path<(String, String, String)>,
) -> Result<Json<ServiceStatus>, StatusCode> {
    let source = Source::parse(&source).ok_or(StatusCode::NOT_FOUND)?;
    let action = ServiceAction::parse(&action).ok_or(StatusCode::BAD_REQUEST)?;

    let status = match source {
        Source::Systemd => {
            let svc = resolve_systemd(&state.config, &id).await?;
            control_err(systemd::control(&svc.unit, action).await, source, &id, action)?;
            systemd::status(&svc.unit, &svc.name).await
        }
        Source::Docker => {
            let (docker_cfg, container_cfg) = resolve_docker(&state.config, &id)?;
            let name = container_cfg.display_name();
            let proxy = docker_cfg.proxy_url.clone();
            control_err(
                docker::control(&state.http, &proxy, &id, action).await,
                source,
                &id,
                action,
            )?;
            docker::status(&state.http, &proxy, &id, &name).await
        }
    };

    match status {
        Ok(status) => Ok(Json(status)),
        Err(err) => {
            eprintln!("failed to read status for {source:?} {id} after {action:?}: {err:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `GET /api/services/{source}/{id}/logs/stream` — live log tail over SSE.
pub async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Path((source, id)): Path<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, StatusCode> {
    let source = Source::parse(&source).ok_or(StatusCode::NOT_FOUND)?;

    let entries: Pin<Box<dyn Stream<Item = LogEntry> + Send>> = match source {
        Source::Systemd => {
            let svc = resolve_systemd(&state.config, &id).await?;
            Box::pin(follow_err(systemd::follow_logs(&svc.unit), source, &id)?)
        }
        Source::Docker => {
            let (docker_cfg, _) = resolve_docker(&state.config, &id)?;
            let stream = docker::follow_logs(&state.http, &docker_cfg.proxy_url, &id).await;
            Box::pin(follow_err(stream, source, &id)?)
        }
    };

    let events = entries.map(|entry| Event::default().json_data(entry));
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// Log a control failure and convert it to a 500.
fn control_err(
    result: anyhow::Result<()>,
    source: Source,
    id: &str,
    action: ServiceAction,
) -> Result<(), StatusCode> {
    result.map_err(|err| {
        eprintln!("failed to {action:?} {source:?} {id}: {err:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Log a log-follow setup failure and convert it to a 500.
fn follow_err<T>(result: anyhow::Result<T>, source: Source, id: &str) -> Result<T, StatusCode> {
    result.map_err(|err| {
        eprintln!("failed to follow logs for {source:?} {id}: {err:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Placeholder status for a container the Docker proxy couldn't be reached for.
fn unreachable_status(container: &str, name: &str) -> ServiceStatus {
    ServiceStatus {
        source: Source::Docker,
        id: container.to_owned(),
        name: name.to_owned(),
        active_state: "unknown".to_owned(),
        sub_state: "unreachable".to_owned(),
        description: "docker proxy unreachable".to_owned(),
        since: None,
        memory_bytes: None,
        pid: None,
    }
}

/// The monitored systemd set: target members ∪ pinned units, each with its
/// display label. This is both the dashboard's systemd list and its allowlist.
async fn systemd_monitored(config: &Config) -> Vec<Monitored> {
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

/// Resolve a systemd unit against the monitored set (the allowlist gate).
async fn resolve_systemd(config: &Config, unit: &str) -> Result<Monitored, StatusCode> {
    systemd_monitored(config)
        .await
        .into_iter()
        .find(|m| m.unit == unit)
        .ok_or(StatusCode::NOT_FOUND)
}

/// Resolve a container name against the configured Docker allowlist, returning
/// the Docker settings (for the proxy URL) and the matched container (for its
/// display label).
fn resolve_docker<'a>(
    config: &'a Config,
    container: &str,
) -> Result<(&'a crate::config::DockerConfig, &'a crate::config::DockerContainerConfig), StatusCode>
{
    let docker_cfg = config.docker.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let matched = docker_cfg
        .containers
        .iter()
        .find(|c| c.container == container)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok((docker_cfg, matched))
}
