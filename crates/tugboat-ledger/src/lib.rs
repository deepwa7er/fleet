//! The tugboat deploy ledger: what shipped, when, and how it went — recorded
//! on the host so dashboards can answer "is this service running the latest
//! code?" without asking the deployer.
//!
//! On-host layout, under tugboat's state dir (`/var/lib/tugboat`):
//!
//! - `<name>.jsonl` — one [`LedgerEntry`] per deploy, appended (O_APPEND, one
//!   short line, so concurrent or interrupted writes can't tear an entry).
//! - `<name>/<id>.log` — the deploy's full transcript, named by its
//!   [`deploy id`](deploy_id).
//!
//! This crate is the contract: tugboat writes through it and lighthouse reads
//! through it, so a schema change is a compile error in the same commit — not
//! a dashboard quietly showing empty history.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Current ledger schema version. v2 added `id`, which names the deploy's
/// transcript file. Readers should ignore entries with a *newer* version (see
/// [`LedgerEntry::is_supported`]) rather than misread them.
pub const LEDGER_VERSION: u32 = 2;

/// One deploy's record — a line of `<name>.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Schema version this entry was written with. Absent (0) on pre-v1 lines.
    #[serde(default)]
    pub v: u32,
    /// Deploy id (`{at}-{short}{nonce}`), naming the transcript
    /// `<name>/<id>.log`.
    /// Absent on pre-v2 entries, which have no saved transcript.
    #[serde(default)]
    pub id: Option<String>,
    /// Full sha of the deployed commit.
    pub sha: String,
    /// Short sha, for display.
    pub short: String,
    /// Whether the built tree had uncommitted changes (always false for
    /// default-branch deploys, which build a clean checkout).
    #[serde(default)]
    pub dirty: bool,
    /// Branch the deploy shipped, when known.
    pub branch: Option<String>,
    /// `deployed` (came up healthy) or `rolled_back` (it didn't).
    pub result: String,
    /// Deploy start, Unix epoch seconds.
    pub at: u64,
}

impl LedgerEntry {
    /// Whether a reader built against this crate understands the entry — its
    /// version is at or below [`LEDGER_VERSION`]. Entries from a newer writer
    /// may carry semantics this reader would silently misrepresent.
    pub fn is_supported(&self) -> bool {
        self.v <= LEDGER_VERSION
    }
}

/// The deploy id naming a transcript: `{start-unix-seconds}-{short-sha}{nonce}`,
/// or `{at}-nogit{nonce}` outside a checkout. The caller supplies a lowercase
/// alphanumeric nonce so attempts of the same commit in the same second cannot
/// overwrite each other's transcript.
pub fn deploy_id(at: u64, short: Option<&str>, nonce: &str) -> String {
    format!("{at}-{}{nonce}", short.unwrap_or("nogit"))
}

/// A deploy id is all digits, a dash, then lowercase alphanumerics (a short
/// sha or `nogit`, followed by the attempt nonce). Readers validate before
/// touching the filesystem: rejecting malformed ids also makes path traversal
/// impossible (no `/`, no `.`).
pub fn valid_deploy_id(id: &str) -> bool {
    let Some((at, rest)) = id.split_once('-') else {
        return false;
    };
    !at.is_empty()
        && at.bytes().all(|b| b.is_ascii_digit())
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

/// A service's ledger file under the tugboat state dir.
pub fn ledger_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join(format!("{name}.jsonl"))
}

/// A deploy's transcript file under the tugboat state dir. The id must already
/// be [`valid_deploy_id`]-checked by the caller.
pub fn transcript_path(state_dir: &Path, name: &str, id: &str) -> PathBuf {
    state_dir.join(name).join(format!("{id}.log"))
}

/// Parse a ledger file's text, oldest entry first. Unparseable lines are
/// skipped (the ledger is append-only across schema versions); entries a newer
/// writer produced are *kept* — filter with [`LedgerEntry::is_supported`] if
/// misreading them would matter.
pub fn parse_ledger(text: &str) -> Vec<LedgerEntry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_id_round_trips_validation() {
        assert!(valid_deploy_id(&deploy_id(
            1_718_900_000,
            Some("1a2b3c4d"),
            "0123456789abcdef"
        )));
        assert!(valid_deploy_id(&deploy_id(42, None, "fedcba9876543210")));
        assert_ne!(
            deploy_id(42, Some("deadbeef"), "0000000000000000"),
            deploy_id(42, Some("deadbeef"), "0000000000000001")
        );
        assert!(!valid_deploy_id("../../etc/passwd"));
        assert!(!valid_deploy_id("nodash"));
        assert!(!valid_deploy_id("123-UPPER"));
        assert!(!valid_deploy_id("-short"));
        assert!(!valid_deploy_id("123-"));
    }

    /// The v2 wire shape both sides rely on, plus tolerance for pre-v2 lines
    /// and retention of future-version entries.
    #[test]
    fn parses_current_legacy_and_future_entries() {
        let text = concat!(
            r#"{"v":2,"id":"1718900000-1a2b3c4d","sha":"aaaa","short":"1a2b3c4d","dirty":false,"branch":"main","result":"deployed","at":1718900000}"#,
            "\n",
            // pre-v1: no v, no id, no dirty
            r#"{"sha":"bbbb","short":"bbbbbbbb","branch":null,"result":"rolled_back","at":1718000000}"#,
            "\n",
            r#"{"v":9,"sha":"cccc","short":"cccccccc","branch":"main","result":"deployed","at":1719000000}"#,
            "\nnot json\n",
        );
        let entries = parse_ledger(text);
        assert_eq!(entries.len(), 3, "bad lines skipped, versions kept");
        assert_eq!(entries[0].id.as_deref(), Some("1718900000-1a2b3c4d"));
        assert!(entries[0].is_supported());
        assert_eq!(entries[1].v, 0);
        assert!(entries[1].is_supported(), "pre-v1 entries are understood");
        assert!(!entries[2].is_supported(), "future-version entries are flagged");
    }

    #[test]
    fn paths_follow_the_on_host_layout() {
        let dir = Path::new("/var/lib/tugboat");
        assert_eq!(ledger_path(dir, "tide"), Path::new("/var/lib/tugboat/tide.jsonl"));
        assert_eq!(
            transcript_path(dir, "tide", "42-abc"),
            Path::new("/var/lib/tugboat/tide/42-abc.log")
        );
    }
}
