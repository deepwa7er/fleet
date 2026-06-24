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
use serde::Deserialize;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::core::model::{Priority, State as TicketState, Ticket, TicketDetail};
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
        .route("/api/tickets/{id}/answer", post(answer))
        .route("/api/tickets/{id}/unblock", post(unblock))
        .route("/api/tickets/{id}/done", post(done))
        .route("/api/tickets/{id}/close", post(close))
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
}

#[derive(Deserialize)]
struct CreateReq {
    title: String,
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
    branch: String,
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
    Ok(Json(st.store.list(q.state)?))
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
        target: req.target,
        priority: req.priority,
        goal: req.goal,
        acceptance: req.acceptance,
        constraints: req.constraints,
    })?;
    Ok((StatusCode::CREATED, Json(ticket)))
}

async fn next(State(st): State<AppState>) -> Result<Json<Option<Ticket>>> {
    Ok(Json(st.store.next(st.stale_hours)?))
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
    require("branch", &req.branch)?;
    Ok(Json(st.store.claim(id, &req.branch)?))
}

async fn needs_input(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    Ok(Json(st.store.needs_input(id, &req.body)?))
}

async fn block(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<BodyReq>,
) -> Result<Json<Ticket>> {
    require("body", &req.body)?;
    Ok(Json(st.store.block(id, &req.body)?))
}

async fn resolve(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ResolveReq>,
) -> Result<Json<Ticket>> {
    require("pr_url", &req.pr_url)?;
    Ok(Json(st.store.resolve(id, &req.pr_url)?))
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
