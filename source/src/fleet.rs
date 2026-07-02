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
}

/// The fleet's repos, resolved to absolute working-tree paths.
#[derive(Debug)]
pub struct Fleet {
    root: PathBuf,
    repos: Vec<Repo>,
}

impl Fleet {
    /// Read `fleet.toml`, resolve member paths against `root`, and keep only the
    /// repos whose working tree actually exists on this host (a member may be
    /// listed but not yet cloned). Repos are sorted by name for a stable UI.
    pub fn load(manifest: &Path) -> Result<Fleet> {
        let text = std::fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let raw: RawFleet =
            toml::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;

        let root = expand_tilde(raw.root.as_deref().unwrap_or("~/code"));
        if !root.is_dir() {
            bail!("fleet root {} is not a directory", root.display());
        }

        let mut repos: Vec<Repo> = Vec::new();
        for member in raw.members {
            let dir = root.join(&member.path);
            // Skip members that aren't checked out here, and anything without a
            // real git repo — we only ever serve tracked files via git.
            if !dir.join(".git").exists() {
                continue;
            }
            let name = member.path.rsplit('/').next().unwrap_or(&member.path).to_string();
            if repos.iter().any(|r| r.name == name) {
                continue;
            }
            repos.push(Repo { name, dir });
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
