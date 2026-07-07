//! What can affect a monorepo member, derived from the build graphs
//! themselves rather than a hand-maintained list: the cargo workspace's
//! path-dependency graph (a change to `crates/fleet-common` affects exactly
//! its dependents) and each member web app's `file:` package links (the
//! `@fleet/ui` wiring). `serve` uses this to scope a member's "undeployed
//! commits" count to commits that can actually reach its build.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

/// The cargo workspace's path-dependency graph: each workspace package's
/// directory mapped to the directories of its direct path dependencies.
/// Registry dependencies carry no path and are excluded — they can't change
/// via a repo commit except through the workspace manifests, which the caller
/// accounts for separately.
pub struct WorkspaceGraph {
    edges: HashMap<PathBuf, Vec<PathBuf>>,
}

/// The subset of `cargo metadata` we consume.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    /// Absolute directory of a path dependency; `None` for registry deps.
    path: Option<PathBuf>,
}

/// Load the workspace graph by running `cargo metadata --no-deps` at `root`
/// (any directory inside the workspace works — cargo finds the root).
pub fn load_workspace_graph(root: &Path) -> Result<WorkspaceGraph> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("running cargo metadata")?;
    ensure!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    parse_metadata(&out.stdout)
}

fn parse_metadata(json: &[u8]) -> Result<WorkspaceGraph> {
    let meta: Metadata = serde_json::from_slice(json).context("parsing cargo metadata")?;
    let mut edges = HashMap::new();
    for pkg in meta.packages {
        let dir = pkg
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(pkg.manifest_path);
        let deps = pkg.dependencies.into_iter().filter_map(|d| d.path).collect();
        edges.insert(dir, deps);
    }
    Ok(WorkspaceGraph { edges })
}

impl WorkspaceGraph {
    /// Whether any workspace package lives under `member_dir` — i.e. the
    /// member builds Rust, so the workspace manifests affect it.
    pub fn has_packages_under(&self, member_dir: &Path) -> bool {
        self.edges.keys().any(|d| d.starts_with(member_dir))
    }

    /// Directories of the transitive path dependencies of every package under
    /// `member_dir`, excluding directories under `member_dir` itself (the
    /// member's own tree is already in scope).
    pub fn dep_dirs(&self, member_dir: &Path) -> Vec<PathBuf> {
        let mut queue: VecDeque<&PathBuf> = self
            .edges
            .keys()
            .filter(|d| d.starts_with(member_dir))
            .collect();
        let mut seen: HashSet<&PathBuf> = queue.iter().copied().collect();
        let mut out = Vec::new();
        while let Some(dir) = queue.pop_front() {
            for dep in self.edges.get(dir).map(Vec::as_slice).unwrap_or_default() {
                if seen.insert(dep) {
                    queue.push_back(dep);
                    if !dep.starts_with(member_dir) {
                        out.push(dep.clone());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// The subset of `package.json` we consume.
#[derive(Deserialize)]
struct PackageJson {
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: HashMap<String, String>,
}

/// Directories of the `file:` dependencies of the member's web app — the
/// packages a web build actually links (e.g. `@fleet/ui`). Reads
/// `<member>/web/package.json`; a member without one contributes nothing.
/// Specifier paths resolve relative to that file. Best-effort: unreadable or
/// unparsable manifests yield an empty list (the member's own dir still
/// scopes the count).
pub fn web_dep_dirs(member_dir: &Path) -> Vec<PathBuf> {
    let web = member_dir.join("web");
    let Ok(raw) = std::fs::read(web.join("package.json")) else {
        return Vec::new();
    };
    let Ok(pkg) = serde_json::from_slice::<PackageJson>(&raw) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = pkg
        .dependencies
        .values()
        .chain(pkg.dev_dependencies.values())
        .filter_map(|spec| spec.strip_prefix("file:"))
        .filter_map(|rel| web.join(rel).canonicalize().ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The graph walk must be transitive (member -> a -> b pulls in b) and
    /// must not report directories under the member itself.
    #[test]
    fn dep_dirs_walks_transitively_and_skips_own_tree() {
        let edges = HashMap::from([
            (
                PathBuf::from("/repo/svc"),
                vec![PathBuf::from("/repo/crates/a"), PathBuf::from("/repo/svc/sub")],
            ),
            (PathBuf::from("/repo/svc/sub"), vec![]),
            (
                PathBuf::from("/repo/crates/a"),
                vec![PathBuf::from("/repo/crates/b")],
            ),
            (PathBuf::from("/repo/crates/b"), vec![]),
            (
                PathBuf::from("/repo/unrelated"),
                vec![PathBuf::from("/repo/crates/c")],
            ),
        ]);
        let graph = WorkspaceGraph { edges };

        assert!(graph.has_packages_under(Path::new("/repo/svc")));
        assert!(!graph.has_packages_under(Path::new("/repo/nope")));

        let deps = graph.dep_dirs(Path::new("/repo/svc"));
        assert_eq!(
            deps,
            vec![PathBuf::from("/repo/crates/a"), PathBuf::from("/repo/crates/b")]
        );
    }

    /// `cargo metadata` parsing keeps only path dependencies, keyed by the
    /// package's directory.
    #[test]
    fn parses_metadata_path_deps() {
        let json = br#"{
            "packages": [
                {
                    "manifest_path": "/repo/svc/Cargo.toml",
                    "dependencies": [
                        {"name": "fleet-common", "path": "/repo/crates/fleet-common"},
                        {"name": "serde"}
                    ]
                }
            ]
        }"#;
        let graph = parse_metadata(json).unwrap();
        assert_eq!(
            graph.dep_dirs(Path::new("/repo/svc")),
            vec![PathBuf::from("/repo/crates/fleet-common")]
        );
    }
}
