//! Editing a tracked file: write the working tree, commit just that file, and
//! push. The commit (not the push) is what matters for the docs loop — a fleet
//! repo's post-commit hook reships pilot's docs from the local working tree — so
//! a push failure (offline, remote behind) is reported as a warning, not an error.
//!
//! Writes are tracked-files-only (the same gate reads use) and path-scoped:
//! `git commit -- <path>` commits only the edited file's working-tree state, so a
//! repo with other in-progress changes is never swept into the commit.

use std::path::Path;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::repo::{self, FileContents};

/// The result of an edit, for the client to reflect.
pub struct EditOutcome {
    /// False when the new content was identical to HEAD (nothing to commit).
    pub committed: bool,
    /// Whether the commit was pushed to the remote.
    pub pushed: bool,
    /// Short SHA of the new commit, when one was made.
    pub commit: Option<String>,
    /// A non-fatal problem (e.g. the push failed) the user should see.
    pub warning: Option<String>,
    /// The file as it now stands, re-read after the write.
    pub file: FileContents,
}

/// Write `content` to a tracked file, then commit (path-scoped) and push.
pub async fn write_file(
    repo_dir: &Path,
    rel: &str,
    content: &str,
    message: Option<&str>,
) -> Result<EditOutcome> {
    // Same gate as reads: tracked, in-tree paths only.
    let safe = repo::ensure_tracked_path(repo_dir, rel).await?;
    let full = repo_dir.join(&safe);

    // Never clobber a directory or a symlink-to-elsewhere; we only edit the
    // regular files the browser shows.
    let meta = tokio::fs::symlink_metadata(&full)
        .await
        .with_context(|| format!("stat {}", full.display()))?;
    if !meta.is_file() {
        bail!("not a regular file: {safe}");
    }

    tokio::fs::write(&full, content)
        .await
        .with_context(|| format!("writing {}", full.display()))?;

    // If the write produced no change vs HEAD, there's nothing to commit. `git
    // diff --quiet` exits 0 when the path is unchanged, 1 when it differs.
    let diff = run_git(repo_dir, &["diff", "--quiet", "--", &safe]).await?;
    if diff.status.success() {
        let file = repo::read_file(repo_dir, &safe).await?;
        return Ok(EditOutcome { committed: false, pushed: false, commit: None, warning: None, file });
    }

    let message = message.map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
    let message = message.unwrap_or_else(|| format!("Update {safe}"));

    // Path-scoped commit: only `safe`'s working-tree state is committed.
    let commit = run_git(repo_dir, &["commit", "-m", &message, "--", &safe]).await?;
    if !commit.status.success() {
        bail!("git commit failed: {}", String::from_utf8_lossy(&commit.stderr).trim());
    }

    let sha = run_git(repo_dir, &["rev-parse", "--short", "HEAD"]).await?;
    let commit_sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();

    // Best-effort push: the commit already triggered the docs refresh hook, so a
    // push failure leaves the edit live locally — surface it, don't fail.
    let mut pushed = false;
    let mut warning = None;
    let push = run_git(repo_dir, &["push"]).await?;
    if push.status.success() {
        pushed = true;
    } else {
        warning = Some(format!(
            "committed {commit_sha} locally, but push failed: {}",
            String::from_utf8_lossy(&push.stderr).trim()
        ));
    }

    let file = repo::read_file(repo_dir, &safe).await?;
    Ok(EditOutcome { committed: true, pushed, commit: Some(commit_sha), warning, file })
}

async fn run_git(repo_dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()
        .await
        .with_context(|| format!("running git {} in {}", args.join(" "), repo_dir.display()))
}
