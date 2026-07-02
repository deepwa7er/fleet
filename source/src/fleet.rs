//! The fleet's git working set, read from the same `fleet.toml` tugboat uses.
//!
//! We deserialize only the two fields this service needs — `root` and each
//! member's `path` — and ignore everything else (`[docs]`, deploy details, …).
//! That keeps a small, stable contract with `fleet.toml` rather than coupling to
//! tugboat's full model; if the two ever need one shared model, factor tugboat's
//! `fleet` module into a crate both depend on.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// One fleet repo: its display name and the absolute path to its working tree.
#[derive(Debug, Clone)]
pub struct Repo {
    /// Display name — the member's directory name (e.g. `lighthouse`).
    pub name: String,
    /// Absolute path to the repo's working tree on this host.
    pub dir: PathBuf,
    /// GitHub blob-URL prefix for this repo's files at HEAD — derived from the
    /// containing checkout's `origin`, so a monorepo project links into
    /// `…/fleet/blob/HEAD/<dir>` and an external repo into its own remote.
    /// `None` when the remote isn't GitHub. Consumers (spyglass) append
    /// `/<path>#L<line>`.
    pub blob_base: Option<String>,
}

/// The fleet's repos, resolved to absolute working-tree paths.
#[derive(Debug)]
pub struct Fleet {
    root: PathBuf,
    repos: Vec<Repo>,
}

/// Top-level monorepo directories that aren't fleet projects (shared library
/// code); everything else committed at the root is a browsable repo.
const NON_PROJECT_DIRS: &[&str] = &["crates", "web"];

impl Fleet {
    /// Assemble the browsable repo set: every *committed* top-level directory
    /// of the fleet monorepo at `root` (via `git ls-tree`, so build output and
    /// stray dirs can't appear), plus any external members from `fleet.toml`
    /// whose working tree exists on this host. Sorted by name for a stable UI.
    pub fn load(manifest: &Path) -> Result<Fleet> {
        let text = std::fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let raw: RawFleet =
            toml::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;

        let root = expand_tilde(raw.root.as_deref().unwrap_or("~/code/fleet"));
        if !root.is_dir() {
            bail!("fleet root {} is not a directory", root.display());
        }

        let mut repos: Vec<Repo> = Vec::new();
        if root.join(".git").exists() {
            let fleet_web = origin_web_url(&root);
            for name in toplevel_dirs(&root)? {
                if NON_PROJECT_DIRS.contains(&name.as_str()) {
                    continue;
                }
                let dir = root.join(&name);
                let blob_base = fleet_web.as_ref().map(|web| format!("{web}/blob/HEAD/{name}"));
                repos.push(Repo { name, dir, blob_base });
            }
        }

        for member in raw.members {
            let path = expand_tilde(&member.path);
            let dir = if path.is_absolute() { path } else { root.join(path) };
            // Skip members that aren't checked out here, and anything without a
            // real git repo — we only ever serve tracked files via git.
            if !dir.join(".git").exists() {
                continue;
            }
            let name = member.path.rsplit('/').next().unwrap_or(&member.path).to_string();
            if repos.iter().any(|r| r.name == name) {
                continue;
            }
            let blob_base = origin_web_url(&dir).map(|web| format!("{web}/blob/HEAD"));
            repos.push(Repo { name, dir, blob_base });
        }
        repos.sort_by(|a, b| a.name.cmp(&b.name));

        if repos.is_empty() {
            bail!("no checked-out repos found under {}", root.display());
        }
        Ok(Fleet { root, repos })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn repos(&self) -> &[Repo] {
        &self.repos
    }

    /// Look up a repo by its display name.
    pub fn repo(&self, name: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.name == name)
    }
}

/// The GitHub web URL of `dir`'s `origin` remote (`https://github.com/o/r`),
/// or `None` for a non-GitHub or missing remote. Handles the scp-style, https,
/// and ssh remote forms.
fn origin_web_url(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout);
    let remote = remote.trim();
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}"))
}

/// The committed top-level directories of the repo at `root`, sorted.
fn toplevel_dirs(root: &Path) -> Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "--name-only", "HEAD"])
        .output()
        .with_context(|| format!("running git ls-tree in {}", root.display()))?;
    if !output.status.success() {
        bail!("git ls-tree failed in {}", root.display());
    }
    let mut dirs: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|name| root.join(name).is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

#[derive(Debug, Deserialize)]
struct RawFleet {
    root: Option<String>,
    #[serde(default)]
    members: Vec<RawMember>,
}

#[derive(Debug, Deserialize)]
struct RawMember {
    path: String,
}

/// Expand a leading `~/` (or a bare `~`) to the user's home directory. Any other
/// path is returned unchanged.
pub fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if raw == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(raw)
}

/// The user's home directory, from `$HOME`. We avoid an extra dependency for
/// what is one environment variable on the platforms this fleet runs on.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
