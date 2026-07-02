//! The `source` service's code-search response (`GET /api/search`), grouped
//! repo → file → line. Produced by source, consumed by spyglass.

use serde::{Deserialize, Serialize};

/// A whole search result set.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    /// True if the server's match cap stopped the scan early.
    #[serde(default)]
    pub truncated: bool,
    pub total: usize,
    pub repos: Vec<RepoMatches>,
}

/// Matches within a single repo.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoMatches {
    pub repo: String,
    /// GitHub blob-URL prefix for this repo's files at HEAD (append
    /// `/<path>#L<line>`); `None` when the remote isn't GitHub. Derived by
    /// source from the repo's actual origin, so consumers deep-link without
    /// knowing the fleet's repo layout.
    #[serde(default)]
    pub blob_base: Option<String>,
    pub files: Vec<FileMatches>,
}

/// Matches within a single file.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileMatches {
    /// Repo-relative path.
    pub path: String,
    pub matches: Vec<LineMatch>,
}

/// One matching line.
#[derive(Debug, Serialize, Deserialize)]
pub struct LineMatch {
    pub line_number: u64,
    /// The line's text (trailing newline trimmed).
    pub text: String,
    /// `[start, end)` byte offsets of each match within `text`, for highlighting.
    pub ranges: Vec<[usize; 2]>,
}
