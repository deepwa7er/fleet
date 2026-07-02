use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::core::model::State;

/// Domain errors. These map onto HTTP status codes for the server and onto
/// process exit codes / stderr for the CLI.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ticket #{0} not found")]
    NotFound(i64),

    /// A state-machine transition that the current state does not permit.
    #[error("ticket #{id} is {actual}, cannot {action} (requires {required})")]
    InvalidTransition {
        id: i64,
        action: &'static str,
        required: State,
        actual: State,
    },

    /// Lost a compare-and-swap race (e.g. two workers claiming the same ticket).
    #[error("ticket #{0} was already claimed")]
    Conflict(i64),

    #[error("invalid input: {0}")]
    BadRequest(String),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),

    /// A startup-time integrity failure surfaced by fleet-common (e.g. an
    /// edited applied migration). Never produced by a request handler.
    #[error("{0}")]
    Startup(String),
}

/// fleet-common's store errors fold into the domain error so `Store::open`
/// composes; the Db case keeps its type, everything else is a startup failure.
impl From<fleet_common::Error> for Error {
    fn from(e: fleet_common::Error) -> Self {
        match e {
            fleet_common::Error::Db(e) => Error::Db(e),
            other => Error::Startup(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidTransition { .. } => StatusCode::CONFLICT,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Startup(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        if let Error::Db(ref e) = self {
            tracing::error!("database error: {e}");
        }
        // The fleet's uniform {"error": …} body.
        fleet_common::http::error_response(self.status(), self)
    }
}
