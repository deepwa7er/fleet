//! atlas's HTTP surface, fronted by breakwater at
//! https://atlas.intern.deepwa7er.net.
//!
//! - `GET  /api/projects`                        → configured projects + index state.
//! - `POST /api/projects/{name}/reindex`         → start a re-index (409 while running).
//! - `GET  /api/projects/{name}/modules`         → module list with item counts.
//! - `GET  /api/projects/{name}/items?crate=&module=` → one module's symbols.
//! - `GET  /api/projects/{name}/search?q=`       → symbol search.
//! - `GET  /api/symbols/{id}`                    → detail + callers/callees/impls.
//! - `GET  /api/symbols/{id}/trace?dir=&depth=`  → call-graph slice for the trace view.
//! - `GET  /healthz`                             → liveness.
//! - everything else                             → the built frontend (SPA fallback).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use crate::config::ProjectConfig;
use crate::error::{Error, Result};
use crate::store::{ProjectMeta, Store, TraceDirection};
use crate::{ingest, scip};

pub struct AppState {
    pub store: Arc<Store>,
    pub rust_analyzer: String,
    pub projects: Vec<ProjectConfig>,
    /// Names of projects with a re-index in flight (one at a time each).
    indexing: Mutex<HashSet<String>>,
}

impl AppState {
    pub fn new(store: Arc<Store>, rust_analyzer: String, projects: Vec<ProjectConfig>) -> Self {
        AppState {
            store,
            rust_analyzer,
            projects,
            indexing: Mutex::new(HashSet::new()),
        }
    }

    fn indexing(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.indexing.lock().expect("indexing set mutex poisoned")
    }
}

type Shared = Arc<AppState>;

pub fn router(state: Shared, web_dir: &Path) -> Router {
    let api = Router::new()
        .route("/projects", get(projects))
        .route("/projects/{name}/reindex", post(reindex))
        .route("/projects/{name}/modules", get(modules))
        .route("/projects/{name}/items", get(items))
        .route("/projects/{name}/search", get(search))
        .route("/symbols/{id}", get(symbol))
        .route("/symbols/{id}/trace", get(trace))
        .route("/{*rest}", any(fleet_common::http::api_not_found))
        .with_state(state);

    Router::new()
        .route("/healthz", get(fleet_common::http::healthz))
        .nest("/api", api)
        .fallback_service(fleet_common::http::spa(web_dir))
        .layer(TraceLayer::new_for_http())
}

// ── /api/projects ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ProjectEntry {
    #[serde(flatten)]
    meta: ProjectMeta,
    indexing: bool,
}

async fn projects(State(state): State<Shared>) -> Result<Json<Vec<ProjectEntry>>> {
    let metas = state.store.projects()?;
    let indexing = state.indexing();
    let entries = metas
        .into_iter()
        .map(|meta| ProjectEntry {
            indexing: indexing.contains(&meta.name),
            meta,
        })
        .collect();
    Ok(Json(entries))
}

// ── POST /api/projects/{name}/reindex ────────────────────────────────────────

/// Kick off `rust-analyzer scip` + ingest for one project in a blocking task.
/// Roughly a `cargo check` of the workspace — the caller polls
/// `GET /api/projects` for completion.
async fn reindex(
    State(state): State<Shared>,
    UrlPath(name): UrlPath<String>,
) -> Result<StatusCode> {
    let project = state
        .projects
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| Error::NotFound(format!("project {name}")))?
        .clone();
    let project_id = state.store.project_id(&name)?;

    if !state.indexing().insert(name.clone()) {
        return Err(Error::Conflict(format!(
            "project {name} is already being indexed"
        )));
    }

    let state_for_task = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let result = index_project(&state_for_task, project_id, &project);
        state_for_task.indexing().remove(&project.name);
        match result {
            Ok(stats) => tracing::info!("re-indexed {}: {stats:?}", project.name),
            Err(e) => tracing::error!("re-indexing {} failed: {e:#}", project.name),
        }
    });
    Ok(StatusCode::ACCEPTED)
}

/// The whole index-one-project pipeline; shared by the CLI.
pub fn index_project(
    state: &AppState,
    project_id: i64,
    project: &ProjectConfig,
) -> anyhow::Result<ingest::IngestStats> {
    let started = Instant::now();
    let index = scip::generate_index(&state.rust_analyzer, &project.path)?;
    let graph = ingest::build_graph(&index);
    let commit = scip::commit_hash(&project.path);
    state.store.replace_graph(
        project_id,
        &graph,
        commit.as_deref(),
        started.elapsed().as_millis() as i64,
    )?;
    Ok(graph.stats)
}

// ── /api/projects/{name}/modules ─────────────────────────────────────────────

async fn modules(
    State(state): State<Shared>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Vec<crate::store::ModuleRow>>> {
    let project_id = state.store.project_id(&name)?;
    Ok(Json(state.store.modules(project_id)?))
}

// ── /api/projects/{name}/items ───────────────────────────────────────────────

#[derive(Deserialize)]
struct ItemsQuery {
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(default)]
    module: String,
}

async fn items(
    State(state): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<ItemsQuery>,
) -> Result<Json<Vec<crate::store::SymbolSummary>>> {
    let project_id = state.store.project_id(&name)?;
    Ok(Json(state.store.module_items(
        project_id,
        &q.crate_name,
        &q.module,
    )?))
}

// ── /api/projects/{name}/search ──────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(state): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<crate::store::SymbolSummary>>> {
    let q = query.q.trim();
    if q.is_empty() {
        return Err(Error::BadRequest("empty query".into()));
    }
    let project_id = state.store.project_id(&name)?;
    Ok(Json(state.store.search(project_id, q, 50)?))
}

// ── /api/symbols/{id} ────────────────────────────────────────────────────────

async fn symbol(
    State(state): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> Result<Json<crate::store::SymbolDetail>> {
    Ok(Json(state.store.symbol_detail(id)?))
}

// ── /api/symbols/{id}/trace ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct TraceQuery {
    #[serde(default = "default_direction")]
    dir: String,
    #[serde(default = "default_depth")]
    depth: u32,
    /// Include calls into std/deps; off by default (they bury the flow).
    #[serde(default)]
    externals: bool,
}

fn default_direction() -> String {
    "out".into()
}

fn default_depth() -> u32 {
    3
}

async fn trace(
    State(state): State<Shared>,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<TraceQuery>,
) -> Result<Json<crate::store::TraceGraph>> {
    let direction = match q.dir.as_str() {
        "out" => TraceDirection::Out,
        "in" => TraceDirection::In,
        other => {
            return Err(Error::BadRequest(format!(
                "dir must be 'out' or 'in', got '{other}'"
            )));
        }
    };
    if q.depth == 0 {
        return Err(Error::BadRequest("depth must be at least 1".into()));
    }
    Ok(Json(state.store.trace(
        id,
        direction,
        q.depth,
        q.externals,
    )?))
}
