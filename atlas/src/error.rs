//! atlas's error type, mapped onto the fleet's `{"error": …}` wire contract.
//!
//! fleet-common's shared `Error` keys `NotFound` by integer id; atlas also
//! addresses things by name (projects) and needs a "re-index already running"
//! conflict, so it keeps its own enum and routes every response through
//! `fleet_common::http::error_response` to keep the body shape identical.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} not found")]
    NotFound(String),

    #[error("invalid input: {0}")]
    BadRequest(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0:#}")]
    Internal(#[from] anyhow::Error),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<fleet_common::Error> for Error {
    fn from(e: fleet_common::Error) -> Self {
        match e {
            fleet_common::Error::NotFound(id) => Error::NotFound(format!("record #{id}")),
            fleet_common::Error::BadRequest(m) => Error::BadRequest(m),
            fleet_common::Error::Internal(m) => Error::Internal(anyhow::anyhow!(m)),
            fleet_common::Error::Db(e) => Error::Db(e),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match &self {
            Error::Db(e) => tracing::error!("database error: {e}"),
            Error::Internal(e) => tracing::error!("internal error: {e:#}"),
            _ => {}
        }
        fleet_common::http::error_response(self.status(), self)
    }
}
