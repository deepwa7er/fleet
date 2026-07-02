//! HTTP server: JSON API for both the web view and the worker CLI, plus static
//! serving of the built web SPA. The same `Store` backs every route, so a human
//! answer posted from the browser is visible to the worker's next call instantly.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::core::model::{Priority, Pulse, State as TicketState, Ticket, TicketDetail, TicketType};
use crate::core::{NewTicket, Store};
use crate::error::{Error, Result};

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    stale_hours: i64,
}

pub struct ServerConfig {
    pub addr: SocketAddr,
    pub web_dir: PathBuf,
    pub stale_hours: i64,
}

pub async fn run(store: Arc<Store>, config: ServerConfig) -> std::io::Result<()> {
    let state = AppState {
        store,
        stale_hours: config.stale_hours,
    };

    let index = config.web_dir.join("index.html");
    let static_files = ServeDir::new(&config.web_dir).fallback(ServeFile::new(index));

    let app = Router::new()
        .route("/api/tickets", get(list).post(create))
        .route("/api/tickets/next", get(next))
        .route("/api/tickets/{id}", get(detail))
        .route("/api/tickets/{id}/claim", post(claim))
        .route("/api/tickets/{id}/needs-input", post(needs_input))
        .route("/api/tickets/{id}/block", post(block))
        .route("/api/tickets/{id}/resolve", post(resolve))
        .route("/api/tickets/{id}/report", post(report))
        .route("/api/tickets/{id}/answer", post(answer))
        .route("/api/tickets/{id}/unblock", post(unblock))
        .route("/api/tickets/{id}/done", post(done))
        .route("/api/tickets/{id}/close", post(close))
        .route("/api/worker", get(worker))
        .route("/api/worker/heartbeat", post(heartbeat))
        // Unknown /api paths must 404 as JSON, not fall through to the SPA shell.
        .route("/api/{*rest}", any(api_not_found))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("drydock listening on http://{}", config.addr);
    axum::serve(listener, app).await
}

// ---- request bodies -------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    state: Option<TicketState>,
    /// Free-text filter over title/goal/acceptance/target (used by spyglass's
    /// federated search, and the web UI's filter box).
    #[serde(default)]
    q: Option<String>,
}

#[derive(Deserialize)]
struct CreateReq {
    title: String,
    #[serde(rename = "type", default)]
    ticket_type: Option<TicketType>,
    target: String,
    priority: Priority,
    goal: String,
    #[serde(default)]
    acceptance: Option<String>,
    #[serde(default)]
    constraints: Option<String>,
}

#[derive(Deserialize)]
struct ClaimReq {
    /// Present for feature work; absent for an investigate ticket (no branch).
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Deserialize)]
struct BodyReq {
    body: String,
}

#[derive(Deserialize)]
struct ResolveReq {
    pr_url: String,
}

#[derive(Deserialize)]
struct UnblockReq {
    #[serde(default)]
    note: Option<String>,
}

// ---- handlers -------------------------------------------------------------

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown endpoint" })),
    )
}

fn require(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(Error::BadRequest(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

async fn list(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Ticket>>> {
    Ok(Json(st.store.list(q.state, q.q.as_deref())?))
}

async fn create(
    State(st): State<AppState>,
    Json(req): Json<CreateReq>,
) -> Result<(StatusCode, Json<Ticket>)> {
    require("title", &req.title)?;
    require("target", &req.target)?;
    require("goal", &req.goal)?;
    let ticket = st.store.create(NewTicket {
        title: req.title,
        ticket_type: req.ticket_type.unwrap_or_default(),
        target: req.target,
        priority: req.priority,
        goal: req.goal,
        acceptance: req.acceptance,
        constraints: req.constraints,
    })?;
    Ok((StatusCode::CREATED, Json(ticket)))
}

/// Record a worker pulse without letting a liveness hiccup fail the action.
fn pulse(st: &AppState, ticket_id: Option<i64>, message: String) {
    if let Err(e) = st.store.pulse(ticket_id, &message) {
        tracing::warn!("worker pulse failed: {e}");
    }
}

async fn next(State(st): State<AppState>) -> Result<Json<Option<Ticket>>> {
    let ticket = st.store.next(st.stale_hours)?;
    let msg = match &ticket {
        Some(t) => format!("picked up #{}", t.id),
        None => "checked — no open tickets".to_string(),
    };
    pulse(&st, ticket.as_ref().map(|t| t.id), msg);
    Ok(Json(ticket))
}

async fn detail(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<TicketDetail>> {
    Ok(Json(st.store.detail(id)?))
}

async fn claim(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ClaimReq>,
) -> Result<Json<Ticket>> {
    let branch = req.branch.as_deref().filter(|s| !s.trim().is_empty());
    let ticket = st.store.claim(id, branch)?;
    pulse(&st, Some(id), format!("claimed #{id}"));
    Ok(Json(ticket))
}

async fn needs_input(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    let ticket = st.store.needs_input(id, &req.body)?;
    pulse(&st, Some(id), format!("asked for input on #{id}"));
    Ok(Json(ticket))
}

async fn block(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    let ticket = st.store.block(id, &req.body)?;
    pulse(&st, Some(id), format!("blocked #{id}"));
    Ok(Json(ticket))
}

async fn resolve(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ResolveReq>,
) -> Result<Json<Ticket>> {
    require("pr_url", &req.pr_url)?;
    let ticket = st.store.resolve(id, &req.pr_url)?;
    pulse(&st, Some(id), format!("opened PR for #{id}"));
    Ok(Json(ticket))
}

async fn report(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    let ticket = st.store.report(id, &req.body)?;
    pulse(&st, Some(id), format!("posted report for #{id}"));
    Ok(Json(ticket))
}

async fn answer(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    Ok(Json(st.store.answer(id, &req.body)?))
}

async fn unblock(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UnblockReq>,
) -> Result<Json<Ticket>> {
    Ok(Json(st.store.unblock(id, req.note.as_deref())?))
}

async fn done(State(st): State<AppState>, Path(id): Path<i64>) -> Result<Json<Ticket>> {
    Ok(Json(st.store.mark_done(id)?))
}

async fn close(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UnblockReq>,
) -> Result<Json<Ticket>> {
    Ok(Json(st.store.close(id, req.note.as_deref())?))
}

// ---- worker liveness ------------------------------------------------------

#[derive(Deserialize)]
struct HeartbeatReq {
    #[serde(default)]
    ticket: Option<i64>,
    message: String,
}

#[derive(Serialize)]
struct WorkerView {
    /// "working" | "idle" | "stuck" | "offline" | "unknown".
    status: &'static str,
    last_seen: Option<String>,
    seconds_since: Option<i64>,
    active_ticket: Option<Ticket>,
    recent: Vec<Pulse>,
}

/// An in-progress ticket with no pulse for this long is probably hung.
const STUCK_AFTER_SECS: i64 = 20 * 60;
/// No pulse at all for this long means the worker isn't running (the worker
/// pulses on every `next`, so a healthy idle worker checks in each run).
const OFFLINE_AFTER_SECS: i64 = 90 * 60;

fn worker_status(active: bool, seconds_since: Option<i64>) -> &'static str {
    match seconds_since {
        None => "unknown",
        Some(age) if age >= OFFLINE_AFTER_SECS => "offline",
        Some(age) if active && age >= STUCK_AFTER_SECS => "stuck",
        Some(_) if active => "working",
        Some(_) => "idle",
    }
}

/// Whole seconds between an RFC-3339 timestamp and now (clamped at 0).
fn seconds_since_now(ts: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some((Utc::now() - then.with_timezone(&Utc)).num_seconds().max(0))
}

async fn worker(State(st): State<AppState>) -> Result<Json<WorkerView>> {
    let (last_seen, active_ticket, recent) = st.store.worker_view(20)?;
    let seconds_since = last_seen.as_deref().and_then(seconds_since_now);
    let status = worker_status(active_ticket.is_some(), seconds_since);
    Ok(Json(WorkerView {
        status,
        last_seen,
        seconds_since,
        active_ticket,
        recent,
    }))
}

async fn heartbeat(
    State(st): State<AppState>,
    Json(req): Json<HeartbeatReq>,
) -> Result<Json<serde_json::Value>> {
    require("message", &req.message)?;
    st.store.pulse(req.ticket, &req.message)?;
    Ok(Json(json!({ "ok": true })))
}
