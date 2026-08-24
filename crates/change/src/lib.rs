//! The source-control domain shared by `dw` and Skiff (DW-002/DW-003).
//!
//! The append-only log is authored state, not a cache. Rounds remain jj
//! commits, while this crate owns the durable relationships and the exact
//! structured diff that review annotations address.

mod deploy;
mod diff;
mod jj;
mod landing;
mod log;
mod model;
mod record;
mod service;

pub use deploy::{DeployTrigger, TugboatClient, TugboatConfig};
pub use diff::{Diff, DiffFile, DiffHunk, DiffKind, DiffLine, parse_diff};
pub use jj::{Commit, Jj, ResolvedCommit, is_full_change_id};
pub use landing::{FizzyConfig, LandingConfig, LandingService, TailReport};
pub use log::{RoundInput, Store};
pub use model::{
    Annotation, AnnotationSide, Author, CardComment, Change, ChangeRef, ChangeState, Deploy,
    DeployOutcome, DeployService, Landed, Landing, RecordExport, Request, Round,
};
pub use record::{PublicChange, Record, RecordConfig, build_public_change};
pub use service::{ChangeService, default_change_dir, default_repos_dir};

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid repository name: {0:?}")]
    InvalidRepo(String),
    #[error("invalid card number: {0}")]
    InvalidCard(u64),
    #[error("change {repo}/{card} already exists")]
    Exists { repo: String, card: u64 },
    #[error("no change {repo}/{card}")]
    NotFound { repo: String, card: u64 },
    #[error("change {repo}/{card} is {state}; {operation} are frozen")]
    Frozen {
        repo: String,
        card: u64,
        state: ChangeState,
        operation: &'static str,
    },
    #[error("change id {change_id} is already round {round}")]
    DuplicateRound { change_id: String, round: u32 },
    #[error("change {repo}/{card} has no round {round}")]
    NoRound { repo: String, card: u64, round: u32 },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Transition(String),
    #[error("{0}")]
    Jj(String),
    #[error("{0}")]
    External(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Error {
    let context = context.into();
    move |source| Error::Io { context, source }
}

pub fn validate_repo(repo: &str) -> Result<()> {
    let mut chars = repo.chars();
    let valid_first = chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric());
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if !valid_first || !valid_rest {
        return Err(Error::InvalidRepo(repo.to_owned()));
    }
    Ok(())
}

pub fn validate_card(card: u64) -> Result<()> {
    if card == 0 {
        return Err(Error::InvalidCard(card));
    }
    Ok(())
}

/// Resolve a named jj repository below one configured root.
pub fn repository_path(repos_dir: &Path, repo: &str) -> Result<PathBuf> {
    validate_repo(repo)?;
    let root = std::fs::canonicalize(repos_dir).map_err(io(format!(
        "resolving repository root {}",
        repos_dir.display()
    )))?;
    let unresolved = root.join(repo);
    let path = match std::fs::canonicalize(&unresolved) {
        Ok(path) if path.starts_with(&root) => path,
        Ok(_) => {
            return Err(Error::Invalid(format!(
                "repository {repo} resolves outside {}",
                root.display()
            )));
        }
        Err(_) => {
            return Err(Error::Invalid(format!(
                "no jj repository named {repo} under {}",
                root.display()
            )));
        }
    };
    if !path.join(".jj").exists() {
        return Err(Error::Invalid(format!(
            "no jj repository named {repo} under {}",
            root.display()
        )));
    }
    Ok(path)
}
