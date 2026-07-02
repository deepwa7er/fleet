//! HTTP layer: serves the embedded single-page UI and the federated search API.
//! All assets are baked into the binary, so the one file is the whole deploy.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use crate::search::{Engine, SearchResponse};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");

/// Build the router with the shared search engine as state.
pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/api/search", get(search))
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

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// `GET /api/search?q=…` — federated results grouped by source.
async fn search(
    State(engine): State<Arc<Engine>>,
    Query(query): Query<SearchQuery>,
) -> Json<SearchResponse> {
    Json(engine.search(&query.q).await)
}
