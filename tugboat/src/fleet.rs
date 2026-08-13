//! The fleet: a set of related services operated together. `fleet.toml` lists
//! member repos so tugboat can clone, pull, deploy, and report on the whole
//! suite with one command — instead of `cd`-ing into each repo in turn.
//!
//! A member only records where its repo lives (`path`, relative to `root`) and
//! its git remote (`repo`, for `clone`); the deploy details stay in that repo's
//! own `deploy.toml`, so there is a single source of truth per service.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::{deploy, git, manifest};

/// The fleet manifest, `fleet.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Fleet {
    /// Base directory the member `path`s are relative to. A leading `~/`
    /// expands to the home directory. Defaults to the directory `fleet.toml`
    /// itself lives in — the monorepo root — which keeps the manifest
    /// relocatable (a CI checkout and a dev box resolve the same file with no
    /// hardcoded path).
    #[serde(default)]
    root: Option<String>,
    /// Where the loaded manifest lives; the `root` default. Set by [`load`].
    #[serde(skip)]
    manifest_dir: PathBuf,
    #[serde(default)]
    pub members: Vec<Member>,
    /// Optional configuration for `tugboat fleet docs` — where the docs frontend
    /// lives, how to build it, and where the assembled site is served.
    #[serde(default)]
    pub docs: Option<DocsConfig>,
    /// Backup-set additions for state that no in-repo `deploy.toml` can
    /// declare (see [`BackupExtras`]); merged by `tugboat fleet gen`.
    #[serde(default)]
    pub backup: BackupExtras,
}

/// State the fleet backup must cover but which no in-monorepo `deploy.toml`
/// declares: tugboat's own ledger (it isn't a deployed service, so it has no
/// manifest) and external members' databases (their manifests live outside
/// this repo, where `fleet gen --check` in CI can't read them).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BackupExtras {
    /// `<dir>/<file>` SQLite databases under `/var/lib`, snapshotted online.
    #[serde(default)]
    pub sqlite: Vec<String>,
    /// `/var/lib` subdirectories backed up in place.
    #[serde(default)]
    pub dirs: Vec<String>,
}
/// The `[docs]` table: how `tugboat fleet docs` builds and ships the fleet
/// documentation site. The site is process-less static files (built from the
/// `repo` frontend plus the harvested model and per-repo rustdoc) served by
/// breakwater, so this records only the frontend build and the ship target.
#[derive(Debug, Clone, Deserialize)]
pub struct DocsConfig {
    /// Member path (relative to the fleet `root`) of the frontend repo, e.g.
    /// `pilot`.
    pub repo: String,
    /// Command run in the repo dir to produce the static frontend, e.g.
    /// `cd web && bun install && bun run build`.
    pub build: String,
    /// Built frontend directory, relative to the repo dir, e.g. `web/dist`.
    pub dist: String,
    /// SSH host (alias) the assembled site ships to.
    pub host: String,
    /// Absolute path on the host that breakwater serves the site from.
    pub dest: String,
    /// Public URL, polled after a ship to confirm the site is live. Optional.
    #[serde(default)]
    pub url: Option<String>,
    /// Claude guidance documents to surface in pilot's Guidance section, beyond
    /// the per-member `CLAUDE.md` (which is auto-discovered). Each source is a
    /// file or a directory of `.md` files; see [`GuidanceSource`].
    #[serde(default)]
    pub guidance: Vec<GuidanceSource>,
}

/// A source of Claude guidance documents for the docs site's Guidance section.
/// `path` may be absolute, `~/…`, or relative to the fleet `root`. A file is one
/// document; a directory contributes each `*.md` inside it plus each
/// `*/SKILL.md` (so a skills directory and a flat memory directory both work).
#[derive(Debug, Clone, Deserialize)]
pub struct GuidanceSource {
    /// Grouping shown in the UI (e.g. `Doctrine`, `Skills`, `Memory`).
    pub category: String,
    /// File or directory path (absolute, `~/…`, or relative to the fleet root).
    pub path: String,
    /// Title for a single-file source. Ignored for a directory (each file's name
    /// is used). Defaults to the file stem when omitted.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    /// Repo directory (also the `clone` target): absolute, `~/…`, or relative
    /// to the fleet `root` — the same forms `[[docs.guidance]]` paths accept.
    /// External repos (outside the monorepo) use `~/…` paths.
    pub path: String,
    /// Git remote, used by `fleet clone`.
    pub repo: String,
}

impl Member {
    /// Short label for output (the repo directory's final component).
    pub fn label(&self) -> &str {
        Path::new(&self.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.path)
    }
}

/// A deployable service discovered on disk: a directory under the fleet root
/// that contains a `deploy.toml`. Deployability is a fact of the filesystem —
/// the presence of a deploy manifest — not a roster entry, so a newly-built
/// service is deployable (and visible to the daemon and lighthouse) the moment
/// it has one, with no `fleet.toml` edit.
#[derive(Debug, Clone)]
pub struct Deployable {
    /// Service name — the repo directory's final component (matches the deploy
    /// label and the systemd unit stem).
    pub name: String,
    /// The repo directory.
    pub dir: PathBuf,
    /// The deploy manifest within it (`<dir>/deploy.toml`).
    pub manifest_path: PathBuf,
}

/// Discover deployable services by scanning the fleet `root` for immediate
/// subdirectories that contain a `deploy.toml`. Sorted by name for stable output.
/// A directory that is itself a git repository (contains `.git`) is skipped:
/// it is a nested checkout (e.g. `fleet/notes` before it is committed as a
/// subtree) whose deploy.toml is not visible to CI and whose route remains
/// hand-written in `breakwater.toml` until the import lands.
pub fn discover_deployables(root: &Path) -> Result<Vec<Deployable>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        // A missing root is simply an empty fleet (e.g. a fresh machine).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => {
            return Err(e).with_context(|| format!("scanning fleet root {}", root.display()))
        }
    };
    for entry in entries {
        let dir = entry
            .with_context(|| format!("reading {}", root.display()))?
            .path();
        if !dir.is_dir() {
            continue;
        }
        // Nested git checkout — not yet part of the monorepo history.
        if dir.join(".git").exists() {
            continue;
        }
        let manifest_path = dir.join("deploy.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        found.push(Deployable {
            name,
            dir,
            manifest_path,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Every deployable service the fleet knows: the directories under the fleet
/// root with a `deploy.toml` ([`discover_deployables`]), plus any external
/// member whose checkout carries one (e.g. lagoon — deployable, but not part
/// of the monorepo). A root directory wins a name collision. Sorted by name.
pub fn deployables(fleet: &Fleet) -> Result<Vec<Deployable>> {
    let mut found = discover_deployables(&fleet.root_dir())?;
    for member in &fleet.members {
        let dir = fleet.dir(member);
        let manifest_path = dir.join("deploy.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let name = member.label().to_string();
        if found.iter().any(|d| d.name == name) {
            continue;
        }
        found.push(Deployable { name, dir, manifest_path });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

impl Fleet {
    /// The fleet root as an absolute path: the explicit `root` (`~/` expanded)
    /// when set, else the directory the manifest was loaded from.
    pub fn root_dir(&self) -> PathBuf {
        match &self.root {
            Some(root) => expand_tilde(root),
            None => self.manifest_dir.clone(),
        }
    }

    /// Absolute path to a member's checkout. A `~/…` or absolute member path
    /// stands alone; a relative one is anchored at the fleet root.
    pub fn dir(&self, member: &Member) -> PathBuf {
        let path = expand_tilde(&member.path);
        if path.is_absolute() {
            path
        } else {
            self.root_dir().join(path)
        }
    }

    /// Find a member by its label (the repo directory's final component).
    pub fn find(&self, label: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.label() == label)
    }
}

/// Expand a leading `~/` (or a bare `~`) to the home directory; otherwise return
/// the path unchanged. Keeps `fleet.toml` portable across machines.
pub(crate) fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

/// Resolve the fleet manifest path: explicit `--manifest`, else `TUGBOAT_FLEET`,
/// else the nearest `fleet.toml` found searching upward from the current dir.
pub fn resolve_manifest(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("TUGBOAT_FLEET") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let cwd = std::env::current_dir().context("getting current directory")?;
    for dir in cwd.ancestors() {
        let candidate = dir.join("fleet.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "no fleet.toml found (searched up from {}); pass --manifest or set TUGBOAT_FLEET",
        cwd.display()
    );
}

/// Load and validate `fleet.toml`.
pub fn load(path: &Path) -> Result<Fleet> {
    let text =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut fleet: Fleet =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    // The manifest's own directory is the default fleet root.
    fleet.manifest_dir = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?
        .parent()
        .context("fleet.toml has no parent directory")?
        .to_path_buf();

    if fleet.members.is_empty() {
        bail!("fleet: at least one [[members]] entry is required");
    }
    let mut seen = std::collections::HashSet::new();
    for m in &fleet.members {
        if m.path.trim().is_empty() {
            bail!("fleet: every member needs a non-empty `path`");
        }
        if m.repo.trim().is_empty() {
            bail!("fleet: member `{}` needs a `repo` remote", m.path);
        }
        if !seen.insert(&m.path) {
            bail!("fleet: duplicate member path `{}`", m.path);
        }
    }
    if let Some(docs) = &fleet.docs {
        for (field, value) in [
            ("repo", &docs.repo),
            ("build", &docs.build),
            ("dist", &docs.dist),
            ("host", &docs.host),
            ("dest", &docs.dest),
        ] {
            if value.trim().is_empty() {
                bail!("fleet: [docs] `{field}` must not be empty");
            }
        }
        if !docs.dest.starts_with('/') {
            bail!("fleet: [docs] `dest` must be an absolute path (got `{}`)", docs.dest);
        }
        if fleet.members.iter().all(|m| m.label() != docs.repo) {
            bail!("fleet: [docs] `repo` = `{}` is not a fleet member", docs.repo);
        }
    }
    Ok(fleet)
}

/// `fleet list` — show the configured members and whether each is a deployable
/// service (has a `deploy.toml`) or clone-only.
pub fn list(fleet: &Fleet) {
    println!("Fleet ({} members, root {}):\n", fleet.members.len(), fleet.root_dir().display());
    for m in &fleet.members {
        let tag = if fleet.dir(m).join("deploy.toml").is_file() {
            "deploy"
        } else {
            "clone-only"
        };
        println!("  {:<14} [{:>10}]  {}", m.label(), tag, m.repo);
    }
}

/// `fleet clone` — clone any members not yet present. Idempotent.
pub fn clone(fleet: &Fleet) -> Result<()> {
    let mut cloned = 0;
    for m in &fleet.members {
        let dir = fleet.dir(m);
        if dir.exists() {
            println!("  exists:   {}", m.label());
            continue;
        }
        println!("==> CLONE {} → {}", m.repo, dir.display());
        let status = Command::new("git")
            .args(["clone", &m.repo])
            .arg(&dir)
            .status()
            .context("spawning git clone")?;
        if !status.success() {
            bail!("git clone failed for {}", m.label());
        }
        cloned += 1;
    }
    println!("\n✓ fleet clone: {cloned} cloned, {} already present", fleet.members.len() - cloned);
    Ok(())
}

/// `fleet pull` — fast-forward-only pull of every clean member checkout. Mirrors
/// the safe semantics of the SessionStart sync hook: never merges, never
/// discards local work.
pub fn pull(fleet: &Fleet) -> Result<()> {
    for m in &fleet.members {
        let dir = fleet.dir(m);
        let label = m.label();
        if !dir.join(".git").is_dir() {
            println!("  missing:  {label} (run `fleet clone`)");
            continue;
        }
        if !git::is_clean(&dir)? {
            println!("  dirty:    {label} (skipped)");
            continue;
        }
        if !git::run(&dir, &["fetch", "-q", "origin"])? {
            println!("  fetch ✗:  {label}");
            continue;
        }
        // Fast-forward to the upstream of the current branch, if one exists.
        match git::upstream(&dir)? {
            Some(target) if git::run(&dir, &["merge", "--ff-only", "-q", &target])? => {
                println!("  pulled:   {label}")
            }
            Some(_) => println!("  not-ff:   {label} (local commits or divergence; skipped)"),
            None => println!("  no upstream: {label} (skipped)"),
        }
    }
    println!("\n✓ fleet pull complete");
    Ok(())
}

/// `fleet status` — a one-line git summary per member.
pub fn status(fleet: &Fleet) -> Result<()> {
    for m in &fleet.members {
        let label = m.label();
        let st = git::state(&fleet.dir(m));
        if !st.is_repo {
            println!("  {label:<14} missing");
            continue;
        }
        let branch = st.branch.as_deref().unwrap_or("?");
        let dirty = if st.dirty { "dirty" } else { "clean" };
        // Compare against the same target `pull` uses (upstream, else origin/<branch>).
        let track = match st.upstream {
            None => "no remote branch".to_string(),
            Some(_) => match (st.upstream_ahead, st.upstream_behind) {
                (0, 0) => "up-to-date".into(),
                (a, 0) => format!("ahead {a}"),
                (0, b) => format!("behind {b}"),
                (a, b) => format!("ahead {a}, behind {b}"),
            },
        };
        println!("  {label:<14} {branch:<10} {dirty:<6} {track}");
    }
    Ok(())
}

/// `fleet deploy` — deploy every discovered deployable service (any repo under
/// the fleet root with a `deploy.toml`), reusing the per-service deploy engine.
/// Fail-fast unless `continue_on_error`. An optional comma-separated `--only`
/// set restricts to named services.
pub fn deploy(
    fleet: &Fleet,
    only: Option<&str>,
    dry_run: bool,
    continue_on_error: bool,
) -> Result<()> {
    let root = fleet.root_dir();
    let mut deployables = self::deployables(fleet)?;

    if let Some(only) = only {
        let wanted: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        for name in &wanted {
            if !deployables.iter().any(|d| d.name == *name) {
                bail!("--only: no deployable service named `{name}` (needs a deploy.toml under {})", root.display());
            }
        }
        deployables.retain(|d| wanted.contains(&d.name.as_str()));
    }

    if deployables.is_empty() {
        println!("No deployable services found under {}", root.display());
        return Ok(());
    }

    let mut deployed = Vec::new();
    let mut failed = Vec::new();

    for d in &deployables {
        println!("\n════ {} ════", d.name);
        let result = (|| -> Result<()> {
            let manifest = manifest::load(&d.manifest_path, None)?;
            deploy::run(&manifest, &d.dir, deploy::Source::DefaultBranch, dry_run, &deploy::StdoutSink)
        })();

        match result {
            Ok(()) => deployed.push(d.name.clone()),
            Err(err) if continue_on_error => {
                eprintln!("!! {} failed: {err:#}", d.name);
                failed.push(d.name.clone());
            }
            Err(err) => {
                return Err(err.context(format!("fleet deploy aborted at {}", d.name)));
            }
        }
    }

    println!("\n──── fleet deploy summary ────");
    println!("  deployed: {}", fmt_list(&deployed));
    if !failed.is_empty() {
        println!("  failed:   {}", fmt_list(&failed));
        bail!("{} service(s) failed to deploy", failed.len());
    }
    Ok(())
}

fn fmt_list(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".into()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// External members with a deploy.toml stay deployable (lagoon) — the
    /// daemon and `fleet deploy` must see root directories AND such members.
    #[test]
    fn deployables_merges_members_with_manifests() {
        let tmp = std::env::temp_dir().join(format!("tugboat-deployables-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("fleet");
        let ext = tmp.join("external");
        fs::create_dir_all(root.join("svc")).unwrap();
        fs::write(root.join("svc/deploy.toml"), "name = \"svc\"\n").unwrap();
        fs::create_dir_all(&ext).unwrap();
        fs::write(ext.join("deploy.toml"), "name = \"external\"\n").unwrap();

        let manifest = format!(
            "root = \"{}\"\n\n[[members]]\npath = \"{}\"\nrepo = \"git@example.com:x.git\"\n",
            root.display(),
            ext.display()
        );
        let fleet: Fleet = toml::from_str(&manifest).unwrap();
        let names: Vec<String> =
            super::deployables(&fleet).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["external", "svc"], "root dirs and manifest-bearing members, sorted");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discovers_only_dirs_with_a_deploy_toml() {
        let tmp = std::env::temp_dir().join(format!("tugboat-discover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);

        // Two deployable services, one clone-only repo, one loose file.
        for name in ["bravo", "alpha"] {
            let d = tmp.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("deploy.toml"), "name = \"x\"\n").unwrap();
        }
        fs::create_dir_all(tmp.join("charlie")).unwrap(); // no deploy.toml
        fs::write(tmp.join("loose.txt"), "x").unwrap();

        let found = discover_deployables(&tmp).unwrap();
        let names: Vec<_> = found.iter().map(|d| d.name.as_str()).collect();
        // Only the deploy.toml dirs, sorted.
        assert_eq!(names, vec!["alpha", "bravo"]);
        assert!(found[0].manifest_path.ends_with("alpha/deploy.toml"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_on_missing_root_is_empty() {
        let p = std::env::temp_dir().join("tugboat-does-not-exist-zzz");
        assert!(discover_deployables(&p).unwrap().is_empty());
    }
}
