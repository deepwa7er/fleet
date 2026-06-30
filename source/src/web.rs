//! source's HTTP surface, fronted by breakwater at
//! https://source.internal.deepwa7er.com.
//!
//! - `GET /api/repos`                       → the fleet's repos.
//! - `GET /api/tree?repo=NAME`              → a repo's tracked file paths.
//! - `GET /api/file?repo=NAME&path=REL`     → one file's contents + metadata.
//! - `GET /api/search?q=TERM[&repo=NAME][&regex=1]` → full-text search.
//! - `GET /api/healthz`                     → liveness.
//! - everything else                        → the built frontend (SPA fallback).

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use crate::fleet::Fleet;
use crate::{edit, repo, search};

pub struct AppState {
    pub fleet: Fleet,
    /// Serializes mutating git operations so two concurrent saves can't interleave
    /// a working-tree write with another's commit. Writes are human-paced, so one
    /// fleet-wide lock is simpler than per-repo locks and correct.
    pub write_lock: tokio::sync::Mutex<()>,
}

type Shared = Arc<AppState>;

pub fn router(state: Shared, web_dir: &Path) -> Router {
    // In production the same binary serves the built frontend; any path that
    // isn't a real file falls back to index.html so the SPA's client routes load.
    let index = web_dir.join("index.html");
    let static_files = ServeDir::new(web_dir).fallback(ServeFile::new(index));

    let api = Router::new()
        .route("/repos", get(list_repos))
        .route("/tree", get(tree))
        .route("/file", get(file).put(save_file))
        .route("/search", get(search_handler))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    Router::new().nest("/api", api).fallback_service(static_files)
}

// ── /api/repos ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RepoEntry {
    name: String,
}

async fn list_repos(State(state): State<Shared>) -> Json<Vec<RepoEntry>> {
    let repos = state
        .fleet
        .repos()
        .iter()
        .map(|r| RepoEntry { name: r.name.clone() })
        .collect();
    Json(repos)
}

// ── /api/tree ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TreeQuery {
    repo: String,
}

#[derive(Serialize)]
struct TreeResponse {
    repo: String,
    files: Vec<String>,
}

async fn tree(State(state): State<Shared>, Query(q): Query<TreeQuery>) -> Result<Json<TreeResponse>, AppError> {
    let repo = state.fleet.repo(&q.repo).ok_or_else(|| AppError::not_found(format!("unknown repo: {}", q.repo)))?;
    let files = repo::tracked_files(&repo.dir).await.map_err(AppError::internal)?;
    Ok(Json(TreeResponse { repo: repo.name.clone(), files }))
}

// ── /api/file ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FileQuery {
    repo: String,
    path: String,
}

#[derive(Serialize)]
struct FileResponse {
    repo: String,
    path: String,
    language: String,
    bytes: u64,
    binary: bool,
    too_large: bool,
    content: Option<String>,
}

async fn file(State(state): State<Shared>, Query(q): Query<FileQuery>) -> Result<Json<FileResponse>, AppError> {
    let repo = state.fleet.repo(&q.repo).ok_or_else(|| AppError::not_found(format!("unknown repo: {}", q.repo)))?;
    let f = repo::read_file(&repo.dir, &q.path).await.map_err(AppError::bad_request)?;
    Ok(Json(FileResponse {
        repo: repo.name.clone(),
        path: f.path,
        language: f.language,
        bytes: f.bytes,
        binary: f.binary,
        too_large: f.too_large,
        content: f.content,
    }))
}

// ── PUT /api/file (edit) ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SaveRequest {
    repo: String,
    path: String,
    content: String,
    /// Commit message; defaults to `Update <path>` when absent or blank.
    #[serde(default)]
    message: Option<String>,
}

#[derive(Serialize)]
struct SaveResponse {
    /// False when the content was unchanged (no commit made).
    committed: bool,
    pushed: bool,
    commit: Option<String>,
    /// A non-fatal note (e.g. the push failed); the edit is still live locally.
    warning: Option<String>,
    /// The file as it now stands.
    file: FileResponse,
}

async fn save_file(
    State(state): State<Shared>,
    Json(body): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, AppError> {
    let repo = state
        .fleet
        .repo(&body.repo)
        .ok_or_else(|| AppError::not_found(format!("unknown repo: {}", body.repo)))?;

    // One writer at a time across the fleet (see AppState::write_lock).
    let _guard = state.write_lock.lock().await;
    let outcome = edit::write_file(&repo.dir, &body.path, &body.content, body.message.as_deref())
        .await
        .map_err(AppError::bad_request)?;

    let f = outcome.file;
    Ok(Json(SaveResponse {
        committed: outcome.committed,
        pushed: outcome.pushed,
        commit: outcome.commit,
        warning: outcome.warning,
        file: FileResponse {
            repo: repo.name.clone(),
            path: f.path,
            language: f.language,
            bytes: f.bytes,
            binary: f.binary,
            too_large: f.too_large,
            content: f.content,
        },
    }))
}

// ── /api/search ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    /// Restrict to a single repo by name; absent searches the whole fleet.
    repo: Option<String>,
    /// Treat `q` as a regular expression instead of a literal substring.
    #[serde(default)]
    regex: bool,
}

async fn search_handler(
    State(state): State<Shared>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<search::SearchResults>, AppError> {
    if q.q.trim().is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!("empty query")));
    }
    let repos: Vec<crate::fleet::Repo> = match &q.repo {
        Some(name) => {
            let r = state
                .fleet
                .repo(name)
                .ok_or_else(|| AppError::not_found(format!("unknown repo: {name}")))?;
            vec![r.clone()]
        }
        None => state.fleet.repos().to_vec(),
    };
    let results = search::run(&repos, &q.q, q.regex).await.map_err(AppError::internal)?;
    Ok(Json(results))
}

// ── error mapping ────────────────────────────────────────────────────────────

/// A handler error carrying an HTTP status and a message, rendered as JSON.
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn not_found(message: impl Into<String>) -> Self {
        AppError { status: StatusCode::NOT_FOUND, message: message.into() }
    }
    fn bad_request(err: anyhow::Error) -> Self {
        AppError { status: StatusCode::BAD_REQUEST, message: format!("{err:#}") }
    }
    fn internal(err: anyhow::Error) -> Self {
        AppError { status: StatusCode::INTERNAL_SERVER_ERROR, message: format!("{err:#}") }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorBody { error: self.message })).into_response()
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}
