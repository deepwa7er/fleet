use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Ensure `dir` is a checkout of `remote` at `origin/<branch>`.
///
/// Clones if `dir` is not yet a git working tree, otherwise fetches and
/// hard-resets to the tracked branch. This is a one-way mirror: harbor never
/// writes to the repo, so a hard reset is always safe and keeps the working
/// copy byte-for-byte identical to the remote.
pub fn sync(dir: &Path, remote: &str, branch: &str) -> Result<()> {
    if is_git_repo(dir) {
        run(dir, &["fetch", "--quiet", "origin", branch])
            .context("git fetch")?;
        run(dir, &["reset", "--hard", "--quiet", &format!("origin/{branch}")])
            .context("git reset --hard")?;
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let dir_str = dir.to_str().context("source_dir is not valid UTF-8")?;
        run_in(
            dir.parent().unwrap_or_else(|| Path::new(".")),
            &["clone", "--quiet", "--branch", branch, remote, dir_str],
        )
        .context("git clone")?;
    }
    Ok(())
}

/// The short HEAD commit of the working copy, if it is a git repo.
pub fn head_commit(dir: &Path) -> Option<String> {
    if !is_git_repo(dir) {
        return None;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let commit = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn run(dir: &Path, args: &[&str]) -> Result<()> {
    run_in(dir, &prepend_c(dir, args))
}

/// Run `git` with the given args from `cwd`.
fn run_in(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("spawning git {args:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {args:?} failed: {}", stderr.trim());
    }
    Ok(())
}

/// Build a `git -C <dir> <args...>` argument vector.
fn prepend_c<'a>(dir: &'a Path, args: &[&'a str]) -> Vec<&'a str> {
    let mut v = Vec::with_capacity(args.len() + 2);
    v.push("-C");
    v.push(dir.to_str().unwrap_or("."));
    v.extend_from_slice(args);
    v
}
