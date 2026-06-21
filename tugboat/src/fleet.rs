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
#[derive(Debug, Deserialize)]
pub struct Fleet {
    /// Base directory the member `path`s are relative to. A leading `~/`
    /// expands to the home directory. Defaults to `~/code`.
    #[serde(default = "default_root")]
    root: String,
    #[serde(default)]
    pub members: Vec<Member>,
}
fn default_root() -> String {
    "~/code".into()
}

#[derive(Debug, Deserialize)]
pub struct Member {
    /// Repo directory, relative to the fleet `root` (also the `clone` target).
    pub path: String,
    /// Git remote, used by `fleet clone`.
    pub repo: String,
    /// Whether `fleet deploy` deploys this member. Members that ship some other
    /// way (e.g. a multi-unit backend) set this false but still clone/pull.
    #[serde(default = "default_true")]
    pub deploy: bool,
    /// Deploy manifest within the repo. Defaults to `deploy.toml`.
    #[serde(default = "default_manifest")]
    pub manifest: String,
}
fn default_true() -> bool {
    true
}
fn default_manifest() -> String {
    "deploy.toml".into()
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

impl Fleet {
    /// Absolute path to a member's checkout.
    pub fn dir(&self, member: &Member) -> PathBuf {
        expand_tilde(&self.root).join(&member.path)
    }

    /// Absolute path to a member's deploy manifest within its checkout.
    pub fn manifest_path(&self, member: &Member) -> PathBuf {
        self.dir(member).join(&member.manifest)
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
    let fleet: Fleet =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

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
    Ok(fleet)
}

/// Filter members to an optional comma-separated `--only` set (matched against
/// the member label). Errors if a requested name matches no member.
fn select<'a>(fleet: &'a Fleet, only: Option<&str>) -> Result<Vec<&'a Member>> {
    let Some(only) = only else {
        return Ok(fleet.members.iter().collect());
    };
    let wanted: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let mut chosen = Vec::new();
    for name in &wanted {
        match fleet.members.iter().find(|m| m.label() == *name) {
            Some(m) => chosen.push(m),
            None => bail!("--only: no fleet member named `{name}`"),
        }
    }
    Ok(chosen)
}

/// `fleet list` — show the configured members.
pub fn list(fleet: &Fleet) {
    println!("Fleet ({} members, root {}):\n", fleet.members.len(), fleet.root);
    for m in &fleet.members {
        let tag = if m.deploy { "deploy" } else { "no-deploy" };
        println!("  {:<14} [{:>9}]  {}", m.label(), tag, m.repo);
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

/// `fleet deploy` — deploy each deployable member in listed order, reusing the
/// per-service deploy engine. Fail-fast unless `continue_on_error`.
pub fn deploy(
    fleet: &Fleet,
    only: Option<&str>,
    skip_build: bool,
    dry_run: bool,
    continue_on_error: bool,
) -> Result<()> {
    let members = select(fleet, only)?;
    let mut deployed = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for m in members {
        let label = m.label();
        if !m.deploy {
            println!("\n— skipping {label} (deploy = false)");
            skipped.push(label.to_string());
            continue;
        }
        let manifest_path = fleet.dir(m).join(&m.manifest);
        if !manifest_path.is_file() {
            // deploy = true but no manifest is a configuration error, surfaced
            // loudly rather than silently skipped.
            let msg = format!("{label}: manifest not found at {}", manifest_path.display());
            if continue_on_error {
                eprintln!("\n!! {msg}");
                failed.push(label.to_string());
                continue;
            }
            bail!(msg);
        }

        println!("\n════ {label} ════");
        let result = (|| -> Result<()> {
            let project_dir = manifest_path.parent().context("manifest has no parent")?;
            let m = manifest::load(&manifest_path, None)?;
            deploy::run(&m, project_dir, skip_build, dry_run, &deploy::StdoutSink)
        })();

        match result {
            Ok(()) => deployed.push(label.to_string()),
            Err(err) if continue_on_error => {
                eprintln!("!! {label} failed: {err:#}");
                failed.push(label.to_string());
            }
            Err(err) => {
                return Err(err.context(format!("fleet deploy aborted at {label}")));
            }
        }
    }

    println!("\n──── fleet deploy summary ────");
    println!("  deployed: {}", fmt_list(&deployed));
    if !skipped.is_empty() {
        println!("  skipped:  {}", fmt_list(&skipped));
    }
    if !failed.is_empty() {
        println!("  failed:   {}", fmt_list(&failed));
        bail!("{} member(s) failed to deploy", failed.len());
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
