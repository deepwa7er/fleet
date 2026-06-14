//! HTTP layer: serves the embedded single-page UI and a JSON generate API.
//! All assets are baked into the binary, so the one file is the whole deploy.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};

use crate::generator::{Engine, Mode, Request as GenRequest};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");

/// Build the router with the shared generator engine as state.
pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/api/generate", get(generate))
        .route("/healthz", get(healthz))
        .with_state(engine)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS).into_response()
}

async fn app_js() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], APP_JS).into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

/// Query parameters for `/api/generate`. Every field is optional and falls back
/// to the generator's defaults, so a bare `/api/generate` works.
#[derive(Debug, Deserialize)]
struct GenerateQuery {
    mode: Option<String>,
    count: Option<usize>,
    min: Option<usize>,
    max: Option<usize>,
    order: Option<usize>,
    syllables: Option<usize>,
    prefer: Option<String>,
    strength: Option<f64>,
    #[serde(default)]
    only: bool,
    seed: Option<u64>,
}

#[derive(Serialize)]
struct GenerateResponse {
    /// Size of the embedded real-word corpus (shown in the UI docbar).
    corpus: usize,
    params: EchoedParams,
    words: Vec<WordRow>,
}

#[derive(Serialize)]
struct EchoedParams {
    mode: &'static str,
    count: usize,
    min: usize,
    max: usize,
    order: usize,
    syllables: usize,
    prefer: String,
    strength: f64,
    only: bool,
    seed: Option<u64>,
}

#[derive(Serialize)]
struct WordRow {
    index: usize,
    word: String,
    length: usize,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

async fn generate(State(engine): State<Arc<Engine>>, Query(q): Query<GenerateQuery>) -> Response {
    let defaults = GenRequest::default();

    let mode = match q.mode.as_deref() {
        None => defaults.mode,
        Some(s) => match s.parse::<Mode>() {
            Ok(m) => m,
            Err(e) => return error(e.to_string()),
        },
    };

    let req = GenRequest {
        mode,
        count: q.count.unwrap_or(defaults.count),
        min_len: q.min.unwrap_or(defaults.min_len),
        max_len: q.max.unwrap_or(defaults.max_len),
        order: q.order.unwrap_or(defaults.order),
        syllables: q.syllables.unwrap_or(defaults.syllables),
        prefer: q.prefer.unwrap_or_default(),
        strength: q.strength.unwrap_or(defaults.strength),
        only: q.only,
        seed: q.seed,
    };

    match engine.run(&req) {
        Ok(words) => {
            let rows = words
                .into_iter()
                .enumerate()
                .map(|(i, w)| WordRow { index: i + 1, length: w.len(), word: w })
                .collect();
            let body = GenerateResponse {
                corpus: engine.corpus_len(),
                params: EchoedParams {
                    mode: req.mode.as_str(),
                    count: req.count,
                    min: req.min_len,
                    max: req.max_len,
                    order: req.order,
                    syllables: req.syllables,
                    prefer: req.prefer,
                    strength: req.strength,
                    only: req.only,
                    seed: req.seed,
                },
                words: rows,
            };
            axum::Json(body).into_response()
        }
        Err(e) => error(e.to_string()),
    }
}

fn error(message: String) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(ErrorBody { error: message })).into_response()
}
