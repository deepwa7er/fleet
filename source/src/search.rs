//! Full-text search across the fleet, powered by ripgrep.
//!
//! We shell out to `rg --json` rather than reimplement search: it's fast,
//! respects each repo's `.gitignore` (so artifacts and secrets stay out), and
//! its JSON event stream is stable and easy to map back to (repo, file, line).
//! Results are capped and grouped by repo → file for the UI.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

// The response shapes live in fleet-api — the shared producer/consumer
// contract (spyglass deserializes exactly these types).
pub use fleet_api::search::{FileMatches, LineMatch, RepoMatches, SearchResults};

use crate::fleet::Repo;

/// Hard cap on match lines returned in one search, so a common term can't
/// produce an unbounded payload. When hit, the response is flagged `truncated`.
const MAX_MATCHES: usize = 500;

/// Run a search over `repos`. `query` is a literal substring unless `regex` is
/// set, in which case it's a ripgrep regular expression. Search is smart-case
/// (case-insensitive unless the query has an uppercase letter).
pub async fn run(repos: &[Repo], query: &str, regex: bool) -> Result<SearchResults> {
    // ripgrep is invoked with the query and every repo dir as positional args —
    // no shell is involved (we use `Command` directly), so the query can't be
    // interpreted as anything but a pattern.
    let mut cmd = Command::new("rg");
    cmd.arg("--json").arg("--smart-case").arg("--max-columns").arg("500");
    if !regex {
        cmd.arg("--fixed-strings");
    }
    cmd.arg("--regexp").arg(query).arg("--");
    for repo in repos {
        cmd.arg(&repo.dir);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("spawning ripgrep")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();

    // Accumulate matches grouped by repo, preserving ripgrep's file/line order.
    let mut by_repo: Vec<(String, Vec<FileMatches>)> = Vec::new();
    let mut repo_index: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;
    let mut truncated = false;

    while let Some(line) = lines.next_line().await.context("reading ripgrep output")? {
        if line.is_empty() {
            continue;
        }
        let event: RgEvent = match serde_json::from_str(&line) {
            Ok(ev) => ev,
            // ripgrep's stream is well-formed; skip anything unexpected rather
            // than fail the whole search.
            Err(_) => continue,
        };
        if event.kind != "match" {
            continue;
        }
        let Some(data) = event.data else { continue };
        let Some(abs_path) = data.path.and_then(|p| p.text) else { continue };
        let Some((repo_name, rel)) = locate(repos, &abs_path) else { continue };
        let Some(line_text) = data.lines.and_then(|l| l.text) else { continue };
        let line_number = data.line_number.unwrap_or(0);
        let ranges = data
            .submatches
            .into_iter()
            .map(|m| [m.start, m.end])
            .collect::<Vec<_>>();

        let text = line_text.trim_end_matches(['\n', '\r']).to_string();
        let line_match = LineMatch { line_number, text, ranges };

        // Find or create this repo's bucket, then this file's bucket within it.
        let ri = *repo_index.entry(repo_name.clone()).or_insert_with(|| {
            by_repo.push((repo_name.clone(), Vec::new()));
            by_repo.len() - 1
        });
        let files = &mut by_repo[ri].1;
        match files.last_mut() {
            Some(f) if f.path == rel => f.matches.push(line_match),
            _ => files.push(FileMatches { path: rel, matches: vec![line_match] }),
        }

        total += 1;
        if total >= MAX_MATCHES {
            truncated = true;
            break;
        }
    }

    // Stop ripgrep promptly if we broke out early, then reap it.
    let _ = child.start_kill();
    let status = child.wait().await.context("waiting on ripgrep")?;
    // ripgrep exits 1 when there are simply no matches — that's success here.
    // A real failure (bad regex, etc.) is code 2; surface its stderr.
    if !truncated && total == 0
        && let Some(code) = status.code()
            && code >= 2 {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = stderr.read_to_string(&mut err).await;
                }
                anyhow::bail!("ripgrep error: {}", err.trim());
            }

    let repos_out = by_repo
        .into_iter()
        .map(|(repo, files)| {
            let blob_base = repos
                .iter()
                .find(|r| r.name == repo)
                .and_then(|r| r.blob_base.clone());
            RepoMatches { repo, blob_base, files }
        })
        .collect();
    Ok(SearchResults { query: query.to_string(), truncated, total, repos: repos_out })
}

/// Map an absolute path from ripgrep back to `(repo name, repo-relative path)` by
/// matching it against the known repo directories (longest match wins, so nested
/// layouts resolve to the deepest repo).
fn locate(repos: &[Repo], abs: &str) -> Option<(String, String)> {
    let abs_path = Path::new(abs);
    let mut best: Option<(&Repo, String)> = None;
    for repo in repos {
        if let Ok(rel) = abs_path.strip_prefix(&repo.dir) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            let better = match &best {
                Some((r, _)) => repo.dir.as_os_str().len() > r.dir.as_os_str().len(),
                None => true,
            };
            if better {
                best = Some((repo, rel));
            }
        }
    }
    best.map(|(repo, rel)| (repo.name.clone(), rel))
}

// ── ripgrep --json event shapes (only the fields we use) ─────────────────────

#[derive(Debug, Deserialize)]
struct RgEvent {
    #[serde(rename = "type")]
    kind: String,
    data: Option<RgData>,
}

#[derive(Debug, Deserialize)]
struct RgData {
    path: Option<RgText>,
    lines: Option<RgText>,
    line_number: Option<u64>,
    #[serde(default)]
    submatches: Vec<RgSubmatch>,
}

#[derive(Debug, Deserialize)]
struct RgText {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RgSubmatch {
    start: usize,
    end: usize,
}
