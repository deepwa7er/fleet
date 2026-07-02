//! The fleet's uniform HTTP surface: the `{"error": …}` JSON contract, the
//! `/healthz` liveness endpoint, JSON 404s for unknown API paths, and SPA
//! static-file serving. Cross-service consumers (lighthouse, spyglass, harbor)
//! rely on these being the same everywhere.

use std::fmt::Display;
use std::path::Path;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

/// Common service errors, mapped onto HTTP status codes and the fleet's
/// `{"error": …}` body. Services whose domain needs more variants (state
/// machines, CAS conflicts) keep their own enum and implement `IntoResponse`
/// via [`error_response`] so the wire contract stays identical.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("record #{0} not found")]
    NotFound(i64),

    #[error("invalid input: {0}")]
    BadRequest(String),

    /// An internal invariant failure (e.g. an edited applied migration at
    /// startup). Not caused by the request.
    #[error("{0}")]
    Internal(String),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        if let Error::Db(ref e) = self {
            tracing::error!("database error: {e}");
        }
        error_response(self.status(), self)
    }
}

/// The fleet's error body: `status` + `{"error": message}`. Every service's
/// error path goes through here (directly or via [`Error`]) so consumers can
/// parse one shape.
pub fn error_response(status: StatusCode, message: impl Display) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

/// `GET /healthz` — the fleet-wide liveness contract: 200 and a body of `ok`.
/// Mount on every HTTP service so lighthouse and `deploy.toml` health checks
/// can rely on one endpoint.
pub async fn healthz() -> &'static str {
    "ok"
}

/// JSON 404 for unknown `/api/…` paths, so API consumers never receive the
/// SPA's index.html where they expected JSON.
pub async fn api_not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "unknown endpoint")
}

/// Static-file service for a built single-page app: serve `web_dir`, falling
/// back to its `index.html` so client-side routes deep-link correctly.
pub fn spa(web_dir: impl AsRef<Path>) -> ServeDir<ServeFile> {
    let web_dir = web_dir.as_ref();
    ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html")))
}

/// Initialize tracing with `RUST_LOG` if set, else `default_filter`
/// (e.g. `"drydock=info,tower_http=info"`).
pub fn init_tracing(default_filter: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();
}
