use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Domain errors, mapped onto HTTP status codes for the server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("record #{0} not found")]
    NotFound(i64),

    #[error("invalid input: {0}")]
    BadRequest(String),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = self.status();
        if let Error::Db(ref e) = self {
            tracing::error!("database error: {e}");
        }
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}
