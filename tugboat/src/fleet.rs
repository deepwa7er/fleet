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
    /// Optional configuration for `tugboat fleet docs` — where the docs frontend
    /// lives, how to build it, and where the assembled site is served.
    #[serde(default)]
    pub docs: Option<DocsConfig>,
}
fn default_root() -> String {
    "~/code".into()
}

/// The `[docs]` table: how `tugboat fleet docs` builds and ships the fleet
/// documentation site. The site is process-less static files (built from the
/// `repo` frontend plus the harvested model and per-repo rustdoc) served by
/// breakwater, so this records only the frontend build and the ship target.
#[derive(Debug, Deserialize)]
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
    /// Extra repos (paths relative to the fleet `root`) to include in the docs'
    /// line-of-code total beyond the deployable members — e.g. the deployer
    /// itself, `tugboat`. They count toward `total_loc` but aren't documented as
    /// services.
    #[serde(default)]
    pub extra_loc: Vec<String>,
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
    /// The fleet root as an absolute path (a leading `~/` expanded).
    pub fn root_dir(&self) -> PathBuf {
        expand_tilde(&self.root)
    }

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

/// Outcome of trying to record a just-deployed service as a fleet member.
#[derive(Debug, PartialEq)]
pub enum Registration {
    /// Already listed in `fleet.toml` — nothing to do.
    AlreadyMember,
    /// Appended a new `[[members]]` entry to the fleet manifest.
    Registered { fleet_path: PathBuf, member_path: String, repo: String },
    /// Not registered, with a human-readable reason. Best-effort: a registration
    /// hiccup must never undo a deploy that already succeeded, so anything that
    /// prevents expressing the service as a member yields this rather than an error.
    Skipped(String),
}

/// Ensure a service that `tugboat deploy` just shipped is recorded as a fleet
/// member, appending it to `fleet.toml` if absent. Located via the same
/// `resolve_manifest` rules as every other fleet op (explicit path, then
/// `TUGBOAT_FLEET`, then an upward search). Best-effort — see [`Registration`].
pub fn ensure_member(project_dir: &Path, service_name: &str) -> Result<Registration> {
    let fleet_path = match resolve_manifest(None) {
        Ok(p) => p,
        Err(_) => {
            return Ok(Registration::Skipped(
                "no fleet.toml found (set TUGBOAT_FLEET)".into(),
            ))
        }
    };
    let fleet = load(&fleet_path)?;

    // Where would this repo sit, relative to the fleet root?
    let (Ok(root), Ok(proj)) = (fleet.root_dir().canonicalize(), project_dir.canonicalize())
    else {
        return Ok(Registration::Skipped("could not resolve repo path".into()));
    };
    let Ok(rel) = proj.strip_prefix(&root) else {
        return Ok(Registration::Skipped(format!(
            "repo is outside the fleet root {}",
            root.display()
        )));
    };
    let member_path = rel.to_string_lossy().into_owned();

    // Already a member, by path or by deploy label? Then there's nothing to do.
    if fleet
        .members
        .iter()
        .any(|m| m.path == member_path || m.label() == service_name)
    {
        return Ok(Registration::AlreadyMember);
    }

    // The git remote is what `fleet clone` needs.
    let Some(repo) = git::out(project_dir, &["remote", "get-url", "origin"])?
        .filter(|s| !s.is_empty())
    else {
        return Ok(Registration::Skipped("repo has no `origin` remote".into()));
    };

    let text = fs::read_to_string(&fleet_path)
        .with_context(|| format!("reading {}", fleet_path.display()))?;
    let updated = append_member_toml(&text, &member_path, &repo)?;
    fs::write(&fleet_path, updated)
        .with_context(|| format!("writing {}", fleet_path.display()))?;

    Ok(Registration::Registered {
        fleet_path,
        member_path,
        repo,
    })
}

/// Append a `[[members]]` entry to `fleet.toml` text, preserving the file's
/// existing comments and layout (a format-preserving edit, not a re-serialize).
fn append_member_toml(text: &str, member_path: &str, repo: &str) -> Result<String> {
    use toml_edit::{value, DocumentMut, Table};
    let mut doc: DocumentMut = text.parse().context("parsing fleet.toml")?;
    let members = doc["members"]
        .as_array_of_tables_mut()
        .context("fleet.toml `members` is not an array of tables")?;
    let mut t = Table::new();
    t.decor_mut()
        .set_prefix("\n# Registered automatically by `tugboat deploy`.\n");
    t["path"] = value(member_path);
    t["repo"] = value(repo);
    members.push(t);
    Ok(doc.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_member_preserves_comments_and_adds_entry() {
        let src = r#"# fleet header comment
root = "~/code"

[[members]]
# lighthouse comment
path = "lighthouse"
repo = "git@github.com:deepwa7er/lighthouse.git"

[docs]
repo = "pilot"
build = "x"
dist = "d"
host = "h"
dest = "/opt/p"
"#;
        let out = append_member_toml(
            src,
            "drydock",
            "git@github.com:deepwa7er/drydock.git",
        )
        .unwrap();

        // Hand-written comments and the [docs] table survive the edit.
        assert!(out.contains("# fleet header comment"));
        assert!(out.contains("# lighthouse comment"));
        assert!(out.contains("[docs]"));
        assert!(out.contains("Registered automatically"));

        // Re-parses, with the new member appended and deploy defaulting to true.
        let fleet: Fleet = toml::from_str(&out).unwrap();
        assert_eq!(fleet.members.len(), 2);
        assert_eq!(fleet.members[0].path, "lighthouse");
        assert_eq!(fleet.members[1].path, "drydock");
        assert_eq!(fleet.members[1].repo, "git@github.com:deepwa7er/drydock.git");
        assert!(fleet.members[1].deploy);
    }

    #[test]
    fn ensure_member_registers_then_is_idempotent() {
        // Hermetic fleet root with one existing member plus a service repo to add.
        let tmp = std::env::temp_dir().join(format!("tugboat-fleettest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("code");
        let svc = root.join("newsvc");
        fs::create_dir_all(&svc).unwrap();

        // A git repo with an origin remote (the URL is never fetched).
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&svc)
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:deepwa7er/newsvc.git"]);

        let fleet_path = tmp.join("fleet.toml");
        fs::write(
            &fleet_path,
            format!(
                "root = \"{}\"\n\n[[members]]\npath = \"existing\"\nrepo = \"git@github.com:deepwa7er/existing.git\"\n",
                root.display()
            ),
        )
        .unwrap();
        std::env::set_var("TUGBOAT_FLEET", &fleet_path);

        // First deploy registers the service…
        let first = ensure_member(&svc, "newsvc").unwrap();
        assert!(matches!(first, Registration::Registered { .. }), "got {first:?}");
        let reloaded = load(&fleet_path).unwrap();
        assert!(reloaded.members.iter().any(|m| m.path == "newsvc"));
        assert_eq!(reloaded.members.len(), 2);

        // …a second deploy is a no-op (no duplicate entry).
        assert_eq!(ensure_member(&svc, "newsvc").unwrap(), Registration::AlreadyMember);
        assert_eq!(load(&fleet_path).unwrap().members.len(), 2);

        std::env::remove_var("TUGBOAT_FLEET");
        let _ = fs::remove_dir_all(&tmp);
    }
}
