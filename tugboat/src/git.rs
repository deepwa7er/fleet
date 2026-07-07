//! Thin git helpers shared by `fleet` (clone/pull/status) and `serve` (the
//! deploy-status endpoint). Everything is best-effort and read-only except
//! [`run`], which is used for the fetch/merge that `fleet pull` performs.

use std::path::{Path, PathBuf};
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

/// Whether `dir` is inside a git working tree. Unlike testing for a `.git`
/// directory, this is true for a directory *within* a checkout (a monorepo
/// member) and for linked worktrees (whose `.git` is a file).
pub fn is_work_tree(dir: &Path) -> bool {
    out(dir, &["rev-parse", "--is-inside-work-tree"])
        .ok()
        .flatten()
        .is_some_and(|s| s == "true")
}

/// The root of the working tree containing `dir` (`git rev-parse
/// --show-toplevel`). For a service directory inside the fleet monorepo this is
/// the repository root; for a standalone checkout it is the checkout itself.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let Some(path) = out(dir, &["rev-parse", "--show-toplevel"])? else {
        bail!("{} is not inside a git working tree", dir.display());
    };
    Ok(PathBuf::from(path))
}

/// Gather the [`RepoState`] for a checkout. Never errors: a non-repo or a git
/// hiccup just yields a sparsely-populated state.
pub fn state(dir: &Path) -> RepoState {
    if !is_work_tree(dir) {
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

/// Count commits in `from..to` that touch any of `paths` (repo-relative
/// pathspecs). Empty `paths` counts every commit, same as [`count_commits`].
/// `None` if either ref is unknown.
pub fn count_commits_touching(dir: &Path, from: &str, to: &str, paths: &[String]) -> Option<u32> {
    if paths.is_empty() {
        return count_commits(dir, from, to);
    }
    let range = format!("{from}..{to}");
    let mut args: Vec<&str> = vec!["rev-list", "--count", &range, "--"];
    args.extend(paths.iter().map(String::as_str));
    out(dir, &args).ok().flatten().and_then(|s| s.parse().ok())
}

/// One commit in a range, for the deploy changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Full commit sha.
    pub sha: String,
    /// Short sha for display.
    pub short: String,
    /// First line of the commit message.
    pub subject: String,
    /// Commit time, Unix epoch seconds.
    pub at: u64,
}

/// The commits in `from..to` (reachable from `to` but not `from`), newest first,
/// capped at `limit`. This is what a deploy shipped: `from` is the
/// previously-deployed sha and `to` the one this deploy shipped.
///
/// Best-effort: an empty range, or either ref being unknown, both yield an empty
/// vec — a caller that needs to tell "no new commits" from "unknown ref" should
/// check the refs with [`rev_parse`] first. Subjects are read with a `0x1f` field
/// separator (which can't appear in a one-line subject), so any character a commit
/// message may contain survives the parse.
pub fn log_range(dir: &Path, from: &str, to: &str, limit: usize) -> Vec<Commit> {
    let range = format!("{from}..{to}");
    let count = format!("-n{limit}");
    let Some(text) = out(dir, &["log", &count, "--format=%H%x1f%s%x1f%ct", &range])
        .ok()
        .flatten()
    else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\x1f');
            let sha = fields.next()?;
            let subject = fields.next()?;
            let at = fields.next()?.parse().ok()?;
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                short: short(sha).to_owned(),
                sha: sha.to_owned(),
                subject: subject.to_owned(),
                at,
            })
        })
        .collect()
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
    use super::{github_web_url, log_range, rev_parse};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Run git in `dir` with a deterministic identity and no signing, so the test
    /// never depends on (or is broken by) the developer's global git config.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// Commit a file with the given message and return the new HEAD sha.
    fn commit(dir: &Path, file: &str, message: &str) -> String {
        std::fs::write(dir.join(file), message).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", message]);
        rev_parse(dir, "HEAD").unwrap().unwrap()
    }

    #[test]
    fn log_range_lists_shipped_commits_newest_first() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("tugboat-log-range-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);

        let first = commit(&dir, "a", "first");
        let _second = commit(&dir, "b", "second");
        let third = commit(&dir, "c", "third");

        // first..third is the two commits after `first`, newest first.
        let commits = log_range(&dir, &first, &third, 10);
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["third", "second"]);
        assert_eq!(commits[0].sha, third);
        assert_eq!(commits[0].short, &third[..8]);

        // The limit caps the count, keeping the newest.
        let capped = log_range(&dir, &first, &third, 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].subject, "third");

        // An empty range (a re-deploy of the same sha) yields nothing.
        assert!(log_range(&dir, &third, &third, 10).is_empty());
        // An unknown ref is best-effort empty, not a panic.
        assert!(log_range(&dir, "0000000000000000000000000000000000000000", &third, 10).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Monorepo scoping: `count_commits_touching` counts only commits touching
    /// the given pathspecs, and falls back to the whole-repo count when no
    /// pathspecs are given.
    #[test]
    fn counts_commits_scoped_to_pathspecs() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("tugboat-count-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("svc")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        git(&dir, &["init", "-q"]);

        let base = commit(&dir, "svc/a", "svc work");
        let _other = commit(&dir, "other/b", "other work");
        let head = commit(&dir, "svc/c", "more svc work");

        // Whole-repo: both commits after `base` count.
        assert_eq!(super::count_commits(&dir, &base, &head), Some(2));
        // Scoped to svc/: the `other` commit is invisible.
        let scoped = super::count_commits_touching(&dir, &base, &head, &["svc".to_owned()]);
        assert_eq!(scoped, Some(1));
        // No pathspecs falls back to the whole-repo count.
        assert_eq!(super::count_commits_touching(&dir, &base, &head, &[]), Some(2));
        // An unknown ref is best-effort None, not a panic.
        let bogus = "0000000000000000000000000000000000000000";
        assert_eq!(super::count_commits_touching(&dir, bogus, &head, &["svc".to_owned()]), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Repo detection must hold from a directory *inside* a checkout — a fleet
    /// monorepo member has no `.git` of its own — and `toplevel` must name the
    /// containing repository root.
    #[test]
    fn detects_work_tree_and_toplevel_from_a_subdirectory() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("tugboat-toplevel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let svc = dir.join("svc");
        std::fs::create_dir_all(&svc).unwrap();
        git(&dir, &["init", "-q"]);
        std::fs::write(svc.join("f"), "x").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);

        assert!(super::is_work_tree(&dir));
        assert!(super::is_work_tree(&svc));
        // Compare canonicalized: git resolves symlinks (macOS /tmp → /private/tmp).
        let root = dir.canonicalize().unwrap();
        assert_eq!(super::toplevel(&svc).unwrap().canonicalize().unwrap(), root);
        assert_eq!(super::toplevel(&dir).unwrap().canonicalize().unwrap(), root);

        // A subdirectory reports its repository's state, not "not a repo".
        let state = super::state(&svc);
        assert!(state.is_repo);
        assert!(state.head_sha.is_some());

        // Outside any repo: cleanly negative.
        let stray = std::env::temp_dir().join(format!("tugboat-norepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&stray);
        std::fs::create_dir_all(&stray).unwrap();
        assert!(!super::is_work_tree(&stray));
        assert!(!super::state(&stray).is_repo);
        assert!(super::toplevel(&stray).is_err());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&stray);
    }

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
