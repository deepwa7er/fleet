//! Thin git helpers shared by `fleet` (clone/pull/status) and `serve` (the
//! deploy-status endpoint). Everything is best-effort and read-only except
//! [`run`], which is used for the fetch/merge that `fleet pull` performs.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// A snapshot of a checkout's state, as far as git can report it.
#[derive(Debug, Default, Clone)]
pub struct RepoState {
    /// Whether `dir` is a git checkout at all.
    pub is_repo: bool,
    /// Current branch (`None` in detached HEAD).
    pub branch: Option<String>,
    /// Full `HEAD` commit sha.
    pub head_sha: Option<String>,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
    /// Number of changed paths (`git status --porcelain` lines).
    pub dirty_files: u32,
    /// The upstream tracking ref, if any (`origin/main`, …).
    pub upstream: Option<String>,
    /// Commits on HEAD not on upstream.
    pub upstream_ahead: u32,
    /// Commits on upstream not on HEAD.
    pub upstream_behind: u32,
}

/// Gather the [`RepoState`] for a checkout. Never errors: a non-repo or a git
/// hiccup just yields a sparsely-populated state.
pub fn state(dir: &Path) -> RepoState {
    if !dir.join(".git").is_dir() {
        return RepoState::default();
    }
    let mut st = RepoState {
        is_repo: true,
        branch: out(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().flatten(),
        head_sha: out(dir, &["rev-parse", "HEAD"]).ok().flatten(),
        ..RepoState::default()
    };

    let status = out(dir, &["status", "--porcelain"]).ok().flatten().unwrap_or_default();
    st.dirty_files = status.lines().filter(|l| !l.is_empty()).count() as u32;
    st.dirty = st.dirty_files > 0;

    st.upstream = upstream(dir).ok().flatten();
    if let Some(upstream) = &st.upstream {
        // `<upstream>...HEAD` left-right counts: left = behind, right = ahead.
        let range = format!("{upstream}...HEAD");
        if let Some(counts) = out(dir, &["rev-list", "--left-right", "--count", &range]).ok().flatten() {
            let mut it = counts.split_whitespace();
            st.upstream_behind = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            st.upstream_ahead = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    st
}

/// Short form of a full sha for display (first 8 chars).
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Whether `ancestor` is an ancestor of `descendant` (so the descendant is the
/// later commit). Used to tell "local is ahead of what's deployed" (stale) from
/// "local has diverged from it".
pub fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Count commits in the range `from..to` (commits reachable from `to` but not
/// `from`). `None` if either ref is unknown.
pub fn count_commits(dir: &Path, from: &str, to: &str) -> Option<u32> {
    out(dir, &["rev-list", "--count", &format!("{from}..{to}")])
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
}

/// Run a git command in `dir`, inheriting stdio, returning whether it succeeded.
pub fn run(dir: &Path, args: &[&str]) -> Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .with_context(|| format!("spawning git {}", args.join(" ")))?
        .success())
}

/// Run a git command in `dir`, returning trimmed stdout, or `None` if it failed.
pub fn out(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

/// Whether the working tree is clean (no uncommitted changes).
pub fn is_clean(dir: &Path) -> Result<bool> {
    Ok(out(dir, &["status", "--porcelain"])?
        .map(|s| s.is_empty())
        .unwrap_or(false))
}

/// The upstream tracking ref of the current branch, falling back to
/// `origin/<branch>` when no upstream is configured but it exists.
pub fn upstream(dir: &Path) -> Result<Option<String>> {
    if let Some(u) = out(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])? {
        if !u.is_empty() {
            return Ok(Some(u));
        }
    }
    let branch = match out(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])? {
        Some(b) if !b.is_empty() => b,
        _ => return Ok(None),
    };
    let candidate = format!("origin/{branch}");
    if out(dir, &["rev-parse", "--verify", "--quiet", &candidate])?.is_some() {
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}
