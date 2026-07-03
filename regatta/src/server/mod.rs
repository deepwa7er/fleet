//! HTTP server: JSON API for the web view plus static serving of the built SPA.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use fleet_common::http::{api_not_found, healthz, spa};
use serde::Deserialize;
use tower_http::trace::TraceLayer;

use crate::core::model::{Activity, Category, Proposal};
use crate::core::{NewActivity, NewProposal, NewStep, Store};
use fleet_common::{Error, Result};

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
}

pub struct ServerConfig {
    pub addr: SocketAddr,
    pub web_dir: PathBuf,
}

pub async fn run(store: Arc<Store>, config: ServerConfig) -> std::io::Result<()> {
    let state = AppState { store };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/api/categories",
            get(list_categories).post(create_category),
        )
        .route(
            "/api/categories/{id}",
            axum::routing::put(rename_category).delete(delete_category),
        )
        .route(
            "/api/activities",
            get(list_activities).post(create_activity),
        )
        .route(
            "/api/activities/{id}",
            axum::routing::delete(delete_activity),
        )
        .route("/api/proposals", get(list_proposals).post(create_proposal))
        .route(
            "/api/proposals/{id}",
            axum::routing::delete(delete_proposal),
        )
        .route(
            "/api/proposals/{id}/vote",
            post(cast_vote).delete(retract_vote),
        )
        // Unknown /api paths must 404 as JSON, not fall through to the SPA shell.
        .route("/api/{*rest}", any(api_not_found))
        .fallback_service(spa(&config.web_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("regatta listening on http://{}", config.addr);
    axum::serve(listener, app).await
}

// ---- request bodies ---------------------------------------------------------

#[derive(Deserialize)]
struct CategoryReq {
    name: String,
}

#[derive(Deserialize)]
struct ActivityReq {
    name: String,
    category_id: i64,
    unit: String,
}

#[derive(Deserialize)]
struct ProposalReq {
    title: String,
    author: String,
    steps: Vec<StepReq>,
}

#[derive(Deserialize)]
struct StepReq {
    activity_id: i64,
    quantity: f64,
}

#[derive(Deserialize)]
struct VoteReq {
    voter: String,
}

/// `?voter=` on the board request marks which proposals that voter has backed.
#[derive(Deserialize)]
struct BoardQuery {
    #[serde(default)]
    voter: Option<String>,
}

// ---- helpers ----------------------------------------------------------------

fn require(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(Error::BadRequest(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

/// Trim a required text field; blank values never reach the store, so a vote
/// row always has a real owner and names are always displayable.
fn name(field: &str, value: &str) -> Result<String> {
    require(field, value)?;
    Ok(value.trim().to_string())
}

// ---- category handlers --------------------------------------------------------

async fn list_categories(State(st): State<AppState>) -> Result<Json<Vec<Category>>> {
    Ok(Json(st.store.categories()?))
}

async fn create_category(
    State(st): State<AppState>,
    Json(req): Json<CategoryReq>,
) -> Result<(StatusCode, Json<Category>)> {
    let category = st.store.create_category(&name("name", &req.name)?)?;
    Ok((StatusCode::CREATED, Json(category)))
}

async fn rename_category(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<CategoryReq>,
) -> Result<Json<Category>> {
    Ok(Json(
        st.store.rename_category(id, &name("name", &req.name)?)?,
    ))
}

async fn delete_category(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_category(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- activity handlers --------------------------------------------------------

async fn list_activities(State(st): State<AppState>) -> Result<Json<Vec<Activity>>> {
    Ok(Json(st.store.activities()?))
}

async fn create_activity(
    State(st): State<AppState>,
    Json(req): Json<ActivityReq>,
) -> Result<(StatusCode, Json<Activity>)> {
    let activity = st.store.create_activity(NewActivity {
        name: name("name", &req.name)?,
        category_id: req.category_id,
        unit: name("unit", &req.unit)?,
    })?;
    Ok((StatusCode::CREATED, Json(activity)))
}

async fn delete_activity(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_activity(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- proposal handlers --------------------------------------------------------

async fn list_proposals(
    State(st): State<AppState>,
    Query(q): Query<BoardQuery>,
) -> Result<Json<Vec<Proposal>>> {
    let voter = q.voter.as_deref().map(str::trim).filter(|v| !v.is_empty());
    Ok(Json(st.store.proposals(voter)?))
}

async fn create_proposal(
    State(st): State<AppState>,
    Json(req): Json<ProposalReq>,
) -> Result<(StatusCode, Json<Proposal>)> {
    let proposal = st.store.create_proposal(NewProposal {
        title: name("title", &req.title)?,
        author: name("author", &req.author)?,
        steps: req
            .steps
            .into_iter()
            .map(|s| NewStep {
                activity_id: s.activity_id,
                quantity: s.quantity,
            })
            .collect(),
    })?;
    Ok((StatusCode::CREATED, Json(proposal)))
}

async fn delete_proposal(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_proposal(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- vote handlers ------------------------------------------------------------

async fn cast_vote(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<VoteReq>,
) -> Result<Json<Proposal>> {
    Ok(Json(st.store.cast_vote(id, &name("voter", &req.voter)?)?))
}

async fn retract_vote(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<VoteReq>,
) -> Result<Json<Proposal>> {
    Ok(Json(
        st.store.retract_vote(id, &name("voter", &req.voter)?)?,
    ))
}
