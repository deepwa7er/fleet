//! `tugboat fleet docs` — generate the fleet documentation site.
//!
//! Documenting the fleet is inherently a whole-fleet operation: it joins facts
//! that live in each member's own repo (its `deploy.toml`, its Cargo metadata)
//! with fleet-wide facts (membership and deploy order from `fleet.toml`, the
//! public routing table from breakwater) — so it belongs here, beside the other
//! `fleet` ops, rather than in any single service's per-repo deploy.
//!
//! The output is a static site directory:
//!   * `fleet.json` — the harvested model the React frontend renders.
//!   * `doc/<member>/…` — each Rust repo's `cargo doc` output, one self-contained
//!     bundle per repo so rustdoc's per-invocation search index keeps working.
//!
//! Every fact has exactly one source of truth, so the docs are by construction
//! current with what actually ships — there is no hand-maintained description of
//! the fleet to drift. The one human-authored field, a service's description,
//! comes from its crate's `Cargo.toml`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::deploy;
use crate::fleet::{DocsConfig, Fleet, Member};
use crate::manifest::{self, ArtifactKind};

/// What `tugboat fleet docs` was asked to produce.
pub struct Options {
    /// Local assembly target. `Some(dir)` writes the assembled site there and
    /// does not ship; `None` assembles into a temp dir and ships per the fleet's
    /// `[docs]` config.
    pub out: Option<PathBuf>,
    /// Skip building the frontend and reuse its existing `dist` instead.
    pub skip_build: bool,
    /// Skip the slow `cargo doc` pass and emit only `fleet.json` (+ frontend).
    pub skip_rustdoc: bool,
    /// Restrict the rustdoc pass to these member labels. The `fleet.json`
    /// overview always covers the whole fleet; this only limits which repos'
    /// (slow) `cargo doc` is rebuilt. `None` rebuilds every repo's.
    pub only: Option<Vec<String>>,
    /// Print the plan and exit without building or shipping anything.
    pub dry_run: bool,
}

// ── the model serialized to fleet.json ──────────────────────────────────────

/// The whole fleet, as the docs site consumes it.
#[derive(Debug, Serialize)]
pub struct FleetDoc {
    pub services: Vec<ServiceDoc>,
}

/// One service's reference entry, joined from its `deploy.toml`, its Cargo
/// metadata, and breakwater's routing table.
#[derive(Debug, Serialize)]
pub struct ServiceDoc {
    /// The deploy/unit name (from `deploy.toml`), else the repo directory name.
    pub name: String,
    /// One-line summary, from the crate's `Cargo.toml` `description` (may be empty).
    pub description: String,
    /// Git remote the repo is cloned from.
    pub repo: String,
    /// Whether `tugboat fleet deploy` ships this member.
    pub deployed: bool,
    /// systemd unit, when the service is tugboat-deployed (`{name}.service`).
    pub unit: Option<String>,
    /// Public URL, when breakwater routes the service (`https://<host>`).
    pub url: Option<String>,
    /// Loopback port the service listens on, from breakwater's upstream.
    pub port: Option<u16>,
    /// On-host health endpoint from `deploy.toml`, when it defines one.
    pub health: Option<String>,
    /// The build command from `deploy.toml`, when present.
    pub build_cmd: Option<String>,
    /// Whether the unit is enrolled in `lighthouse.target`.
    pub lighthouse_enrolled: bool,
    /// Relationships derived from ground truth (no hand-curated graph).
    pub relationships: Relationships,
    /// Rustdoc bundles for the repo's crates.
    pub crates: Vec<CrateDoc>,
}

/// How a service relates to the fleet's cross-cutting services — each derived
/// from a structured source, not a hand-maintained edge list.
#[derive(Debug, Serialize)]
pub struct Relationships {
    /// Enrolled in `lighthouse.target` (so lighthouse monitors it).
    pub monitored_by_lighthouse: bool,
    /// Shipped by `tugboat fleet deploy`.
    pub deployed_by_tugboat: bool,
    /// Has a breakwater route (so it's reachable by name over HTTPS).
    pub routed_by_breakwater: bool,
}

/// A single crate's rustdoc, linked from its service.
#[derive(Debug, Clone, Serialize)]
pub struct CrateDoc {
    pub name: String,
    /// Site-absolute path to the crate's rustdoc landing page, e.g.
    /// `/doc/harbor/harbor_server/`.
    pub doc_path: String,
}

// ── generation ──────────────────────────────────────────────────────────────

pub fn generate(fleet: &Fleet, opts: &Options) -> Result<()> {
    if opts.dry_run {
        return print_plan(fleet, opts);
    }
    match &opts.out {
        // Local assembly: write the site to the given directory, don't ship.
        Some(dir) => {
            assemble(fleet, opts, dir)?;
            println!(
                "\n✓ fleet docs assembled in {} (not shipped — --out given)",
                dir.display()
            );
            Ok(())
        }
        // Default: assemble into a temp dir and ship per the fleet's [docs].
        None => {
            let docs = fleet.docs.as_ref().context(
                "[docs] is not configured in fleet.toml; pass --out <dir> to assemble locally",
            )?;
            let workdir = WorkDir::new("tugboat-docs")?;
            assemble(fleet, opts, workdir.path())?;
            ship(docs, workdir.path())
        }
    }
}

/// Build the frontend (when configured), harvest the model, run rustdoc, and lay
/// the whole site down in `out`.
fn assemble(fleet: &Fleet, opts: &Options, out: &Path) -> Result<()> {
    let routes = load_routes(fleet);
    std::fs::create_dir_all(out)
        .with_context(|| format!("creating output dir {}", out.display()))?;
    let doc_root = out.join("doc");

    // The React frontend (the app shell) goes down first; the harvested model and
    // rustdoc overlay it (in particular, fleet.json overwrites the dev fixture).
    if let Some(docs) = &fleet.docs {
        build_frontend(fleet, docs, opts.skip_build, out)?;
    }

    println!("==> harvesting {} fleet members", fleet.members.len());
    let mut services = Vec::new();
    let mut rustdoc_built = Vec::new();
    let mut rustdoc_failed = Vec::new();

    for m in &fleet.members {
        let label = m.label().to_string();
        let cargo = harvest_cargo(&fleet.dir(m), &label)
            .with_context(|| format!("reading Cargo metadata for {label}"))?;
        let service = build_service_doc(fleet, m, &routes, &cargo.description, cargo.crates.clone())
            .with_context(|| format!("describing {label}"))?;

        let rustdoc_wanted = !opts.skip_rustdoc
            && opts.only.as_ref().is_none_or(|names| names.iter().any(|n| n == &label));
        if rustdoc_wanted {
            for root in &cargo.roots {
                println!("==> cargo doc: {label} ({})", root.manifest_path.display());
                match build_rustdoc(&root.manifest_path) {
                    Ok(()) if root.target_doc.is_dir() => {
                        copy_dir_all(&root.target_doc, &doc_root.join(&label))
                            .with_context(|| format!("copying rustdoc for {label}"))?;
                        rustdoc_built.push(label.clone());
                    }
                    Ok(()) => {
                        eprintln!("    warning: no rustdoc output at {}", root.target_doc.display());
                        rustdoc_failed.push(label.clone());
                    }
                    Err(err) => {
                        // One repo that doesn't `cargo doc` on this host must not
                        // sink the whole site — degrade visibly, don't drop silently.
                        eprintln!("    warning: cargo doc failed for {label}: {err:#}");
                        rustdoc_failed.push(label.clone());
                    }
                }
            }
        }

        services.push(service);
    }

    let model = FleetDoc { services };
    let json_path = out.join("fleet.json");
    let json = serde_json::to_string_pretty(&model).context("serializing fleet.json")?;
    std::fs::write(&json_path, json)
        .with_context(|| format!("writing {}", json_path.display()))?;

    println!("  services:   {}", model.services.len());
    println!("  fleet.json: {}", json_path.display());
    if opts.skip_rustdoc {
        println!("  rustdoc:    skipped (--skip-rustdoc)");
    } else {
        println!("  rustdoc:    built {}", fmt_list(&rustdoc_built));
        if !rustdoc_failed.is_empty() {
            println!("  rustdoc:    skipped/failed {}", fmt_list(&rustdoc_failed));
        }
    }
    Ok(())
}

/// Build the docs frontend and copy its built `dist` into `out`. With
/// `skip_build`, reuse whatever `dist` already exists (a missing one just means
/// no app shell — the data and rustdoc still assemble).
fn build_frontend(fleet: &Fleet, docs: &DocsConfig, skip_build: bool, out: &Path) -> Result<()> {
    let member = fleet
        .find(&docs.repo)
        .with_context(|| format!("[docs] repo `{}` is not a fleet member", docs.repo))?;
    let repo_dir = fleet.dir(member);
    let dist = repo_dir.join(&docs.dist);

    if skip_build {
        if dist.is_dir() {
            println!("==> frontend: reusing {} (--skip-build)", dist.display());
            copy_dir_all(&dist, out)?;
        } else {
            eprintln!(
                "    warning: --skip-build, but no frontend at {}; the site will have no app shell",
                dist.display()
            );
        }
        return Ok(());
    }

    println!("==> frontend build: `{}` (in {})", docs.build, repo_dir.display());
    run_in(&repo_dir, &docs.build)?;
    if !dist.is_dir() {
        bail!("frontend build produced no {}", dist.display());
    }
    copy_dir_all(&dist, out)?;
    Ok(())
}

/// Ship an assembled site to the host and swap it into place atomically. There
/// is no service to restart — breakwater serves the directory — so this is a
/// directory rsync plus a swap, not a unit deploy.
fn ship(docs: &DocsConfig, assembled: &Path) -> Result<()> {
    let log = deploy::StdoutSink;
    let DocsConfig { host, dest, url, .. } = docs;
    let staged = format!("{host}:{dest}.tug-new");
    let parent = Path::new(dest)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());

    println!("\n==> SHIP: {} → {host}:{dest}", assembled.display());
    // Ensure the destination's parent exists so rsync can create the staged dir,
    // then stage the new site beside the live one (same filesystem → atomic swap).
    deploy::ssh_script(host, &ensure_parent_script(&parent), &log)?;
    deploy::rsync(assembled, &staged, ArtifactKind::Dir, &log)?;
    deploy::ssh_script(host, &swap_script(dest), &log)?;

    if let Some(url) = url {
        println!("==> VERIFY: {url}");
        match verify_url(url) {
            Ok(()) => println!("    live at {url}"),
            Err(err) => println!(
                "    warning: {url} not reachable from here ({err}); the files are in place on the host"
            ),
        }
    }
    println!("\n✓ fleet docs shipped to {host}:{dest}");
    Ok(())
}

/// Remote script: create the destination's parent directory (root or via sudo).
fn ensure_parent_script(parent: &str) -> String {
    format!(
        "set -euo pipefail\n\
         sudo=\"\"; [ \"$(id -u)\" -eq 0 ] || sudo=\"sudo\"\n\
         $sudo mkdir -p {}",
        deploy::shq(parent),
    )
}

/// Remote script: swap the freshly-staged `dest.tug-new` into `dest`, keeping the
/// previous tree as `dest.tug-bak` only until the swap succeeds. A directory
/// rename on one filesystem is atomic, so a reader never sees a half-written site.
fn swap_script(dest: &str) -> String {
    format!(
        "set -euo pipefail\n\
         sudo=\"\"; [ \"$(id -u)\" -eq 0 ] || sudo=\"sudo\"\n\
         d={d}\n\
         $sudo rm -rf \"$d.tug-bak\"\n\
         if [ -e \"$d\" ]; then $sudo mv \"$d\" \"$d.tug-bak\"; fi\n\
         $sudo mv \"$d.tug-new\" \"$d\"\n\
         $sudo rm -rf \"$d.tug-bak\"\n\
         echo \"    installed $d\"",
        d = deploy::shq(dest),
    )
}

/// Poll a URL until it answers (or give up). Informational, like a deploy verify.
fn verify_url(url: &str) -> Result<()> {
    const RETRIES: u32 = 6;
    for attempt in 1..=RETRIES {
        let status = Command::new("curl")
            .args(["-fs", "-o", "/dev/null", "--max-time", "12", url])
            .status()
            .context("spawning curl")?;
        if status.success() {
            return Ok(());
        }
        if attempt < RETRIES {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    bail!("not reachable after {RETRIES} attempts");
}

/// Run a shell command in `dir`, streaming its output to this process's stdio.
fn run_in(dir: &Path, cmd: &str) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .with_context(|| format!("spawning: {cmd}"))?;
    if !status.success() {
        bail!("command exited with {status}: {cmd}");
    }
    Ok(())
}

/// Print what `fleet docs` would do, without building or shipping.
fn print_plan(fleet: &Fleet, opts: &Options) -> Result<()> {
    println!("DRY RUN — fleet docs plan\n");
    match &fleet.docs {
        Some(docs) => {
            println!("  frontend: `{}` (in {})", docs.build, docs.repo);
            println!("  dist:     {}", docs.dist);
        }
        None => println!("  frontend: (none — no [docs] configured)"),
    }
    let rustdoc = if opts.skip_rustdoc {
        "skipped (--skip-rustdoc)".to_string()
    } else {
        match &opts.only {
            Some(only) => format!("cargo doc for {}", only.join(", ")),
            None => "cargo doc for every Rust member".to_string(),
        }
    };
    println!("  rustdoc:  {rustdoc}");
    match (&opts.out, &fleet.docs) {
        (Some(dir), _) => println!("  output:   {} (local; not shipped)", dir.display()),
        (None, Some(docs)) => {
            println!("  ship:     → {}:{}", docs.host, docs.dest);
            if let Some(url) = &docs.url {
                println!("  verify:   {url}");
            }
        }
        (None, None) => println!("  ship:     (cannot — no [docs] configured; use --out)"),
    }
    Ok(())
}

/// A temp directory removed when this guard drops.
struct WorkDir(PathBuf);
impl WorkDir {
    fn new(prefix: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        // Start from a clean slate so a previous run's files can't leak in.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating work dir {}", dir.display()))?;
        Ok(Self(dir))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Assemble one service's [`ServiceDoc`] from its member entry, its `deploy.toml`
/// (when present), the routing table, and its harvested Cargo facts.
fn build_service_doc(
    fleet: &Fleet,
    member: &Member,
    routes: &HashMap<String, RouteInfo>,
    description: &str,
    crates: Vec<CrateDoc>,
) -> Result<ServiceDoc> {
    let label = member.label().to_string();
    let manifest_path = fleet.manifest_path(member);
    let manifest = if manifest_path.is_file() {
        Some(manifest::parse(&manifest_path)?)
    } else {
        None
    };

    let name = manifest
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| label.clone());

    // An explicit deploy.toml description is authoritative; otherwise fall back
    // to the crate-level Cargo description harvested for this repo.
    let description = manifest
        .as_ref()
        .and_then(|m| m.description.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(description);

    // Match a route by the service name, then the repo label — breakwater's
    // route host's first DNS label is the service it fronts.
    let route = routes.get(&name).or_else(|| routes.get(&label));
    let url = route.map(|r| r.url.clone());
    let port = route.and_then(|r| r.port);

    let deployed = member.deploy && manifest.is_some();
    let unit = manifest.as_ref().map(|_| format!("{name}.service"));
    let health = manifest
        .as_ref()
        .and_then(|m| m.health.as_ref())
        .and_then(|h| h.url.clone());
    let build_cmd = manifest.as_ref().map(|m| m.build.cmd.clone());
    let enrolled = manifest.as_ref().map(|m| m.lighthouse.enroll).unwrap_or(false);

    Ok(ServiceDoc {
        name,
        description: description.to_string(),
        repo: member.repo.clone(),
        deployed,
        unit,
        relationships: Relationships {
            monitored_by_lighthouse: enrolled,
            deployed_by_tugboat: deployed,
            routed_by_breakwater: url.is_some(),
        },
        url,
        port,
        health,
        build_cmd,
        lighthouse_enrolled: enrolled,
        crates,
    })
}

// ── breakwater routes ───────────────────────────────────────────────────────

/// A service's public face, as breakwater exposes it.
struct RouteInfo {
    url: String,
    port: Option<u16>,
}

/// Only the routing fields breakwater's config carries that we care about; the
/// rest of `breakwater.toml` (TLS, ACME, ports) is ignored.
#[derive(Deserialize)]
struct BreakwaterConfig {
    #[serde(default)]
    routes: Vec<BreakwaterRoute>,
}
#[derive(Deserialize)]
struct BreakwaterRoute {
    host: String,
    /// Present for proxy routes; absent for static-directory routes.
    #[serde(default)]
    upstream: Option<String>,
}

/// Read breakwater's routing table, keyed by each route host's first DNS label
/// (the service name). Best-effort: if breakwater isn't a member or its config
/// can't be read, services simply carry no URL.
fn load_routes(fleet: &Fleet) -> HashMap<String, RouteInfo> {
    let mut table = HashMap::new();
    let Some(breakwater) = fleet.find("breakwater") else {
        return table;
    };
    let path = fleet.dir(breakwater).join("breakwater.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return table;
    };
    let config: BreakwaterConfig = match toml::from_str(&text) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("    warning: could not parse {}: {err}", path.display());
            return table;
        }
    };
    for route in config.routes {
        let Some(label) = route.host.split('.').next() else {
            continue;
        };
        let port = route
            .upstream
            .as_deref()
            .and_then(|u| u.rsplit(':').next())
            .and_then(|p| p.parse().ok());
        table.insert(
            label.to_string(),
            RouteInfo { url: format!("https://{}", route.host), port },
        );
    }
    table
}

// ── Cargo metadata + rustdoc ────────────────────────────────────────────────

/// Cargo facts harvested for one member.
struct CargoInfo {
    /// Best service description found among the member's crates (may be empty).
    description: String,
    /// One entry per documentable crate, with its site-absolute rustdoc path.
    crates: Vec<CrateDoc>,
    /// Cargo roots to run `cargo doc` against (a member usually has one).
    roots: Vec<CargoRoot>,
}

/// A Cargo package/workspace root within a member, and where its built docs land.
struct CargoRoot {
    manifest_path: PathBuf,
    /// `<target-directory>/doc`, the tree `cargo doc` writes (reported by Cargo).
    target_doc: PathBuf,
}

/// Harvest a member's Cargo facts via `cargo metadata` (fast — no compilation).
/// A non-Rust member (no `Cargo.toml` anywhere) yields empty facts.
fn harvest_cargo(member_dir: &Path, label: &str) -> Result<CargoInfo> {
    let manifests = discover_cargo_roots(member_dir);
    let mut roots = Vec::new();
    let mut packages = Vec::new();
    let mut crates = Vec::new();

    for manifest_path in manifests {
        let metadata = cargo_metadata(&manifest_path)
            .with_context(|| format!("cargo metadata for {}", manifest_path.display()))?;
        roots.push(CargoRoot {
            manifest_path,
            target_doc: PathBuf::from(&metadata.target_directory).join("doc"),
        });
        for package in metadata.packages {
            if let Some(doc_dir) = primary_doc_dir(&package) {
                crates.push(CrateDoc {
                    name: package.name.clone(),
                    doc_path: format!("/doc/{label}/{doc_dir}/"),
                });
            }
            packages.push(package);
        }
    }

    Ok(CargoInfo {
        description: choose_description(&packages, label),
        crates,
        roots,
    })
}

/// Find the Cargo root(s) inside a member: the repo-root `Cargo.toml` if present
/// (which is the workspace/package root Cargo resolves from), otherwise any
/// `Cargo.toml` one directory down (e.g. harbor's crate lives in `server/`).
fn discover_cargo_roots(member_dir: &Path) -> Vec<PathBuf> {
    let root = member_dir.join("Cargo.toml");
    if root.is_file() {
        return vec![root];
    }
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(member_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("Cargo.toml");
            if candidate.is_file() {
                found.push(candidate);
            }
        }
    }
    found.sort();
    found
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetaPackage>,
    target_directory: String,
}
#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    #[serde(default)]
    description: Option<String>,
    targets: Vec<MetaTarget>,
}
#[derive(Deserialize)]
struct MetaTarget {
    name: String,
    kind: Vec<String>,
}

fn cargo_metadata(manifest_path: &Path) -> Result<CargoMetadata> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--manifest-path"])
        .arg(manifest_path)
        .stderr(Stdio::inherit())
        .output()
        .context("spawning cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata exited with {}", output.status);
    }
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata output")
}

/// The rustdoc output folder for a package: its library target's name if it has
/// one, else its first binary's — both normalized as rustdoc normalizes them
/// (`-` → `_`). `None` for a package with nothing to document.
fn primary_doc_dir(package: &MetaPackage) -> Option<String> {
    let is = |target: &&MetaTarget, kind: &str| target.kind.iter().any(|k| k == kind);
    let lib = package
        .targets
        .iter()
        .find(|t| is(t, "lib") || is(t, "rlib") || is(t, "proc-macro"));
    let bin = package.targets.iter().find(|t| is(t, "bin"));
    lib.or(bin).map(|t| t.name.replace('-', "_"))
}

/// Pick a service description from its crates: prefer the package named after
/// the repo, then any package whose name starts with it, then any package that
/// has a description at all.
fn choose_description(packages: &[MetaPackage], label: &str) -> String {
    fn desc(package: &MetaPackage) -> Option<&str> {
        package.description.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
    let exact = packages.iter().find(|p| p.name == label).and_then(desc);
    let prefix = packages.iter().find(|p| p.name.starts_with(label)).and_then(desc);
    let any = packages.iter().find_map(desc);
    exact.or(prefix).or(any).unwrap_or("").to_string()
}

fn build_rustdoc(manifest_path: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .args(["doc", "--no-deps", "--manifest-path"])
        .arg(manifest_path)
        .status()
        .context("spawning cargo doc")?;
    if !status.success() {
        bail!("cargo doc exited with {status}");
    }
    Ok(())
}

/// Recursively copy `src`'s contents into `dst` (created if missing).
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {} → {}", from.display(), to.display()))?;
        }
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

    fn target(name: &str, kind: &str) -> MetaTarget {
        MetaTarget { name: name.into(), kind: vec![kind.into()] }
    }
    fn package(name: &str, description: Option<&str>, targets: Vec<MetaTarget>) -> MetaPackage {
        MetaPackage { name: name.into(), description: description.map(Into::into), targets }
    }

    #[test]
    fn doc_dir_prefers_lib_and_normalizes_dashes() {
        let pkg = package("harbor-server", None, vec![target("harbor-server", "bin")]);
        assert_eq!(primary_doc_dir(&pkg).as_deref(), Some("harbor_server"));

        let lib_and_bin = package(
            "thing",
            None,
            vec![target("thing", "bin"), target("thing", "lib")],
        );
        assert_eq!(primary_doc_dir(&lib_and_bin).as_deref(), Some("thing"));

        let nothing = package("buildscript-only", None, vec![target("build-script-build", "custom-build")]);
        assert_eq!(primary_doc_dir(&nothing), None);
    }

    #[test]
    fn description_prefers_exact_name_then_prefix_then_any() {
        let pkgs = vec![
            package("lagoon-core", Some("the core"), vec![]),
            package("lagoon", Some("the service"), vec![]),
            package("unrelated", Some("other"), vec![]),
        ];
        assert_eq!(choose_description(&pkgs, "lagoon"), "the service");

        // No exact match — fall back to a name that starts with the label.
        let prefix_only = vec![package("lagoon-server", Some("server crate"), vec![])];
        assert_eq!(choose_description(&prefix_only, "lagoon"), "server crate");

        // Neither — fall back to any described package.
        let any = vec![package("zzz", Some("something"), vec![])];
        assert_eq!(choose_description(&any, "lagoon"), "something");

        // Nothing described — empty string.
        let none = vec![package("zzz", None, vec![])];
        assert_eq!(choose_description(&none, "lagoon"), "");
    }
}
