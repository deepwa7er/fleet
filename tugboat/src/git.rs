//! Thin git helpers shared by `fleet` (clone/pull/status) and `serve` (the
//! deploy-status endpoint). Everything is best-effort and read-only except
//! [`run`], which is used for the fetch/merge that `fleet pull` performs.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

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

/// Convert a git remote URL into its GitHub web base
/// (`https://github.com/owner/repo`), or `None` for non-GitHub remotes. Handles
/// the scp-style (`git@github.com:owner/repo.git`), https, and ssh forms — so a
/// reader can build commit/compare links without hardcoding the host or owner.
pub fn github_web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("git://github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    // Require exactly an `owner/repo` shape; reject empty or deeper paths.
    let mut parts = path.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}"))
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

/// Resolve a committish (branch, tag, sha, …) to its full sha, or `None` when it
/// doesn't resolve in this repo.
pub fn rev_parse(dir: &Path, committish: &str) -> Result<Option<String>> {
    out(dir, &["rev-parse", "--verify", "--quiet", committish])
}

/// Origin's default branch (e.g. `main`) — what a deploy ships and what the
/// dashboard reports, regardless of which branch the working tree is parked on.
///
/// Reads the local `refs/remotes/origin/HEAD` symref (set at clone time), so it
/// needs no network in the common case. If that ref is missing it asks the remote
/// once — `git remote set-head origin --auto`, which also persists the ref so the
/// next call is local again — then falls back to a conventional `main`/`master`
/// that exists as a remote-tracking ref.
pub fn default_branch(dir: &Path) -> Result<String> {
    if let Some(branch) = origin_head_branch(dir)? {
        return Ok(branch);
    }
    let _ = run(dir, &["remote", "set-head", "origin", "--auto"]);
    if let Some(branch) = origin_head_branch(dir)? {
        return Ok(branch);
    }
    for candidate in ["main", "master"] {
        if rev_parse(dir, &format!("origin/{candidate}"))?.is_some() {
            return Ok(candidate.to_owned());
        }
    }
    bail!("could not determine origin's default branch for {}", dir.display())
}

/// The branch that `refs/remotes/origin/HEAD` points at, from local refs only
/// (no network). `None` when the symref isn't set.
fn origin_head_branch(dir: &Path) -> Result<Option<String>> {
    let Some(full) = out(dir, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])? else {
        return Ok(None);
    };
    Ok(full
        .strip_prefix("refs/remotes/origin/")
        .map(str::to_owned)
        .filter(|b| !b.is_empty()))
}

/// Fetch `origin`, updating remote-tracking refs and objects. It never touches
/// the working tree or the current branch, so it is safe to run while other work
/// (e.g. the drydock worker) has a checkout of the same repo open.
pub fn fetch(dir: &Path) -> Result<()> {
    if !run(dir, &["fetch", "--quiet", "origin"])? {
        bail!("git fetch origin failed in {}", dir.display());
    }
    Ok(())
}

/// Create a detached worktree of the repo at `dir`, checked out at `committish`.
/// The worktree shares the repo's object store, so creating it is cheap. Pair it
/// with [`remove_worktree`].
pub fn add_worktree(dir: &Path, worktree: &Path, committish: &str) -> Result<()> {
    let path = worktree.to_string_lossy();
    if !run(dir, &["worktree", "add", "--quiet", "--detach", &path, committish])? {
        bail!("git worktree add {path} @ {committish} failed");
    }
    Ok(())
}

/// Remove a worktree created by [`add_worktree`], pruning git's admin metadata.
/// Best-effort and idempotent: used both to clean up on drop and to clear a stale
/// worktree an interrupted deploy may have left before creating a fresh one. Runs
/// silently (via [`out`], which captures output) so the common "nothing to clean"
/// case doesn't print a scary `fatal: … is not a working tree`.
pub fn remove_worktree(dir: &Path, worktree: &Path) {
    let path = worktree.to_string_lossy();
    let _ = out(dir, &["worktree", "remove", "--force", &path]);
    // If the directory survived (it was never a worktree, or the remove failed),
    // delete it and prune the dangling admin entry so a re-add can reuse the path.
    if worktree.exists() {
        let _ = std::fs::remove_dir_all(worktree);
    }
    let _ = out(dir, &["worktree", "prune"]);
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

#[cfg(test)]
mod tests {
    use super::github_web_url;

    #[test]
    fn normalizes_github_remote_forms() {
        let want = Some("https://github.com/deepwa7er/lagoon".to_string());
        // The fleet uses scp-style; also accept https/ssh and a missing .git.
        assert_eq!(github_web_url("git@github.com:deepwa7er/lagoon.git"), want);
        assert_eq!(github_web_url("git@github.com:deepwa7er/lagoon"), want);
        assert_eq!(github_web_url("https://github.com/deepwa7er/lagoon.git"), want);
        assert_eq!(github_web_url("https://github.com/deepwa7er/lagoon"), want);
        assert_eq!(github_web_url("ssh://git@github.com/deepwa7er/lagoon.git"), want);
        assert_eq!(github_web_url("  git@github.com:deepwa7er/lagoon.git  "), want);
    }

    #[test]
    fn rejects_non_github_or_malformed() {
        assert_eq!(github_web_url("git@gitlab.com:x/y.git"), None);
        assert_eq!(github_web_url("https://example.com/a/b"), None);
        assert_eq!(github_web_url("git@github.com:deepwa7er"), None); // no repo
        assert_eq!(github_web_url("git@github.com:a/b/c.git"), None); // too deep
        assert_eq!(github_web_url(""), None);
    }
}
