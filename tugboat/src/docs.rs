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
use crate::git;
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
    /// The docs site's own public URL (from `[docs].url`), so a consumer of
    /// `fleet.json` — e.g. lighthouse building "open the docs" links — knows
    /// where the site lives without hardcoding it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    /// Total hand-written source lines across the fleet (sum of each service's
    /// `loc`). A fleet-scale stat; harbor displays it on the new-tab page.
    pub total_loc: u64,
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
    /// Hand-written source lines in this repo — tracked code files only (via
    /// `git ls-files`, so build output and vendored deps are excluded).
    pub loc: u64,
    /// Primary implementation language, inferred from the repo's manifest files
    /// (`Cargo.toml` → Rust, `go.mod` → Go, else `package.json` → TypeScript).
    /// `None` when none apply (e.g. a process-less or config-only repo).
    pub language: Option<String>,
    /// Relationships derived from ground truth (no hand-curated graph).
    pub relationships: Relationships,
    /// Rustdoc bundles for the repo's crates.
    pub crates: Vec<CrateDoc>,
    /// Whether a rendered README was shipped at `/readme/<name>.html` for the
    /// service's detail page.
    pub has_readme: bool,
    /// Direct dependencies of the service's own crates (no transitive deps).
    pub dependencies: Dependencies,
}

/// A service's direct dependencies, split into workspace-internal edges and
/// external crates. Empty for non-Rust services. Harvested from `cargo metadata`
/// (declared deps only — no transitive graph), so it stays accurate by
/// construction.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Dependencies {
    /// Edges between the service's own crates (`from` depends on `to`), for a
    /// workspace like lagoon's; empty for a single-crate service.
    pub internal: Vec<InternalDep>,
    /// External crate names the service depends on directly, sorted and deduped
    /// (dev-dependencies excluded).
    pub external: Vec<String>,
}

/// One workspace-internal dependency edge: crate `from` depends on crate `to`.
#[derive(Debug, Clone, Serialize)]
pub struct InternalDep {
    pub from: String,
    pub to: String,
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
    let mut docs_built = Vec::new();
    let mut docs_failed = Vec::new();

    for m in &fleet.members {
        let label = m.label().to_string();
        let member_docs = harvest_docs(&fleet.dir(m), &label)
            .with_context(|| format!("harvesting docs for {label}"))?;
        let language = detect_language(&fleet.dir(m), &member_docs);

        // Render the repo's README to an HTML fragment for the service's detail
        // page, shipped at /readme/<name>.html (fetched lazily by pilot).
        let readme_html = read_readme(&fleet.dir(m)).map(|md| render_markdown_body(&md));
        let service = build_service_doc(fleet, m, &routes, &member_docs, language, readme_html.is_some())
            .with_context(|| format!("describing {label}"))?;
        if let Some(html) = &readme_html {
            let readme_dir = out.join("readme");
            std::fs::create_dir_all(&readme_dir)
                .with_context(|| format!("creating {}", readme_dir.display()))?;
            let path = readme_dir.join(format!("{}.html", service.name));
            std::fs::write(&path, html)
                .with_context(|| format!("writing {}", path.display()))?;
        }

        let docs_wanted = !opts.skip_rustdoc
            && opts.only.as_ref().is_none_or(|names| names.iter().any(|n| n == &label));
        if docs_wanted {
            // Rust members are documented with `cargo doc`, one bundle per root.
            for root in &member_docs.roots {
                println!("==> cargo doc: {label} ({})", root.manifest_path.display());
                match build_rustdoc(&root.manifest_path) {
                    Ok(()) if root.target_doc.is_dir() => {
                        copy_dir_all(&root.target_doc, &doc_root.join(&label))
                            .with_context(|| format!("copying rustdoc for {label}"))?;
                        docs_built.push(label.clone());
                    }
                    Ok(()) => {
                        eprintln!("    warning: no rustdoc output at {}", root.target_doc.display());
                        docs_failed.push(label.clone());
                    }
                    Err(err) => {
                        // One repo that doesn't `cargo doc` on this host must not
                        // sink the whole site — degrade visibly, don't drop silently.
                        eprintln!("    warning: cargo doc failed for {label}: {err:#}");
                        docs_failed.push(label.clone());
                    }
                }
            }
            // A Go member is documented with `go doc` → one HTML page.
            if let Some(module_dir) = &member_docs.go_module {
                println!("==> go doc: {label} ({})", module_dir.display());
                match build_go_docs(module_dir, &doc_root.join(&label)) {
                    Ok(()) => docs_built.push(label.clone()),
                    Err(err) => {
                        eprintln!("    warning: go doc failed for {label}: {err:#}");
                        docs_failed.push(label.clone());
                    }
                }
            }
        }

        services.push(service);
    }

    // The fleet total counts the deployable members plus any extra repos the
    // [docs] config names (e.g. the deployer itself), which aren't services.
    let members_loc: u64 = services.iter().map(|s| s.loc).sum();
    let extra_loc: u64 = match &fleet.docs {
        Some(docs) => docs
            .extra_loc
            .iter()
            .map(|rel| count_loc(&fleet.root_dir().join(rel)))
            .sum(),
        None => 0,
    };
    let model = FleetDoc {
        docs_url: fleet.docs.as_ref().and_then(|d| d.url.clone()),
        total_loc: members_loc + extra_loc,
        services,
    };
    let json_path = out.join("fleet.json");
    let json = serde_json::to_string_pretty(&model).context("serializing fleet.json")?;
    std::fs::write(&json_path, json)
        .with_context(|| format!("writing {}", json_path.display()))?;

    let guidance = harvest_guidance(fleet, out)?;

    println!("  services:   {}", model.services.len());
    println!("  fleet.json: {}", json_path.display());
    println!("  guidance:   {guidance} document(s)");
    if opts.skip_rustdoc {
        println!("  api docs:   skipped (--skip-rustdoc)");
    } else {
        println!("  api docs:   built {}", fmt_list(&docs_built));
        if !docs_failed.is_empty() {
            println!("  api docs:   skipped/failed {}", fmt_list(&docs_failed));
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

// ── change detection (for auto-refresh) ─────────────────────────────────────

/// A fingerprint of every fleet member's committed state — each member's git
/// HEAD sha. It changes on any commit, amend, rebase, or pull in any member, and
/// is the signal that the published docs are stale. (Members with no checkout
/// contribute `none`, so a missing repo is stable, not noisy.)
pub fn fleet_fingerprint(fleet: &Fleet) -> String {
    let mut entries: Vec<String> = fleet
        .members
        .iter()
        .map(|m| {
            let head = git::state(&fleet.dir(m)).head_sha.unwrap_or_else(|| "none".into());
            format!("{}:{head}", m.label())
        })
        .collect();
    // Extra repos counted in `total_loc` (e.g. the deployer) also change the docs
    // output, so a commit to them must mark the docs stale too.
    if let Some(docs) = &fleet.docs {
        for rel in &docs.extra_loc {
            let head =
                git::state(&fleet.root_dir().join(rel)).head_sha.unwrap_or_else(|| "none".into());
            entries.push(format!("{rel}:{head}"));
        }
    }
    entries.join("\n")
}

/// Where the last successfully-shipped fingerprint is cached on the dev box.
fn fingerprint_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("tugboat").join("docs-fingerprint")
}

/// The fingerprint of the last docs build that shipped successfully, if recorded.
pub fn read_stored_fingerprint() -> Option<String> {
    std::fs::read_to_string(fingerprint_path())
        .ok()
        .map(|s| s.trim().to_string())
}

/// Record the fingerprint of a docs build that just shipped successfully, so the
/// next change check can tell whether anything moved since.
pub fn write_fingerprint(fingerprint: &str) -> Result<()> {
    let path = fingerprint_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, fingerprint).with_context(|| format!("writing {}", path.display()))
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
    docs: &MemberDocs,
    language: Option<String>,
    has_readme: bool,
) -> Result<ServiceDoc> {
    let label = member.label().to_string();
    let manifest_path = fleet.dir(member).join("deploy.toml");
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
        .unwrap_or(&docs.description);

    // Match a route by the service name, then the repo label — breakwater's
    // route host's first DNS label is the service it fronts.
    let route = routes.get(&name).or_else(|| routes.get(&label));
    let url = route.map(|r| r.url.clone());
    let port = route.and_then(|r| r.port);

    // A member is a deployable service iff it has a deploy.toml.
    let deployed = manifest.is_some();
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
        loc: count_loc(&fleet.dir(member)),
        language,
        crates: docs.crates.clone(),
        has_readme,
        dependencies: docs.dependencies.clone(),
    })
}

/// Infer a repo's primary language from what the doc harvest found (which locates
/// Cargo manifests even in subdirs, e.g. harbor's `server/`): Rust if it has any
/// Cargo root, Go if it's a Go module, else TypeScript when a `package.json` sits
/// at the root or under `web/` (a frontend-only/static repo like pilot). `None`
/// when none apply.
fn detect_language(dir: &Path, docs: &MemberDocs) -> Option<String> {
    if !docs.roots.is_empty() {
        Some("Rust".to_string())
    } else if docs.go_module.is_some() {
        Some("Go".to_string())
    } else if dir.join("package.json").is_file() || dir.join("web/package.json").is_file() {
        Some("TypeScript".to_string())
    } else {
        None
    }
}

/// Source file extensions counted as hand-written code (excludes config, data,
/// lock files, and prose like Markdown).
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "swift", "py", "rb", "sh", "fish", "css",
    "scss", "sass", "html", "sql",
];

/// Count hand-written source lines in a repo: tracked code files only (via
/// `git ls-files`, so build artifacts and vendored deps — being git-ignored —
/// don't count). Best-effort: a non-repo or a read hiccup yields 0.
fn count_loc(dir: &Path) -> u64 {
    let Ok(output) = Command::new("git").arg("-C").arg(dir).arg("ls-files").output() else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    let files = String::from_utf8_lossy(&output.stdout);
    let mut total = 0u64;
    for rel in files.lines() {
        let ext = Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !CODE_EXTENSIONS.contains(&ext) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(dir.join(rel)) {
            total += content.lines().count() as u64;
        }
    }
    total
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

/// Doc facts harvested for one member: how to document it and what to link.
struct MemberDocs {
    /// Best service description from Cargo (empty for non-Rust; deploy.toml fills).
    description: String,
    /// Doc links to surface on the site (Rust: one per crate; Go: one).
    crates: Vec<CrateDoc>,
    /// Cargo roots to run `cargo doc` against (empty unless this is a Rust member).
    roots: Vec<CargoRoot>,
    /// Module directory to run `go doc` against, when this is a Go member.
    go_module: Option<PathBuf>,
    /// Direct dependencies of the member's crates (empty for non-Rust members).
    dependencies: Dependencies,
}

/// A Cargo package/workspace root within a member, and where its built docs land.
struct CargoRoot {
    manifest_path: PathBuf,
    /// `<target-directory>/doc`, the tree `cargo doc` writes (reported by Cargo).
    target_doc: PathBuf,
}

/// Harvest a member's doc facts. A Rust member is documented with `cargo doc`
/// (discovered via `cargo metadata` — fast, no compilation); a Go member (a
/// `go.mod` and no `Cargo.toml`) with `go doc`; anything else gets no API docs.
fn harvest_docs(member_dir: &Path, label: &str) -> Result<MemberDocs> {
    let manifests = discover_cargo_roots(member_dir);
    if !manifests.is_empty() {
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
        let dependencies = harvest_dependencies(&packages);
        return Ok(MemberDocs {
            description: choose_description(&packages, label),
            crates,
            roots,
            go_module: None,
            dependencies,
        });
    }

    // A Go module (go.mod, no Cargo.toml) is documented with `go doc`, mounted as
    // a single page at /doc/<label>/. Its description comes from deploy.toml.
    if member_dir.join("go.mod").is_file() {
        let module = go_module_name(member_dir).unwrap_or_else(|| label.to_owned());
        return Ok(MemberDocs {
            description: String::new(),
            crates: vec![CrateDoc { name: module, doc_path: format!("/doc/{label}/") }],
            roots: Vec::new(),
            go_module: Some(member_dir.to_path_buf()),
            dependencies: Dependencies::default(),
        });
    }

    Ok(MemberDocs {
        description: String::new(),
        crates: Vec::new(),
        roots: Vec::new(),
        go_module: None,
        dependencies: Dependencies::default(),
    })
}

/// Split a workspace's declared dependencies into internal edges (a member crate
/// depending on another member crate) and external crates (everything else),
/// from `cargo metadata`'s per-package `dependencies` — declared deps only, so no
/// transitive noise. Dev-dependencies are excluded (they're test/build scaffolding,
/// not the service's architecture).
fn harvest_dependencies(packages: &[MetaPackage]) -> Dependencies {
    let members: std::collections::HashSet<&str> =
        packages.iter().map(|p| p.name.as_str()).collect();
    let mut internal = Vec::new();
    let mut external = std::collections::BTreeSet::new();
    for package in packages {
        for dep in &package.dependencies {
            if dep.kind.as_deref() == Some("dev") {
                continue;
            }
            if members.contains(dep.name.as_str()) {
                internal.push(InternalDep {
                    from: package.name.clone(),
                    to: dep.name.clone(),
                });
            } else {
                external.insert(dep.name.clone());
            }
        }
    }
    internal.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    internal.dedup_by(|a, b| a.from == b.from && a.to == b.to);
    Dependencies {
        internal,
        external: external.into_iter().collect(),
    }
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
    /// Declared dependencies (present even with `--no-deps`; no transitive graph).
    #[serde(default)]
    dependencies: Vec<MetaDep>,
}

#[derive(Deserialize)]
struct MetaDep {
    name: String,
    /// `null` for a normal dependency, else `"dev"` or `"build"`.
    #[serde(default)]
    kind: Option<String>,
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

// ── Go docs ─────────────────────────────────────────────────────────────────
//
// rustdoc has no Go equivalent that emits a static site, so we render `go doc`
// (which ships with the Go toolchain — no extra tooling) into one self-contained
// HTML page per repo. It's text-based rather than navigable like rustdoc, but
// honest and dependency-free, and the monospace styling fits the fleet's look.

/// Build a Go module's docs into `dest/index.html`. Prefers **gomarkdoc**
/// (structured Markdown — index, signatures, prose, source links — rendered to
/// HTML), and falls back to plain `go doc` text if gomarkdoc isn't installed.
fn build_go_docs(module_dir: &Path, dest: &Path) -> Result<()> {
    let module = go_module_name(module_dir).unwrap_or_else(|| "package".to_owned());
    let html = match go_markdown(module_dir) {
        Ok(markdown) if !markdown.trim().is_empty() => render_markdown_page(&module, &markdown),
        result => {
            if let Err(err) = &result {
                eprintln!("    note: gomarkdoc unavailable ({err}); falling back to `go doc` text");
            }
            go_doc_html(&module, &go_doc_text_sections(module_dir)?)
        }
    };
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    std::fs::write(dest.join("index.html"), html)
        .with_context(|| format!("writing {}", dest.join("index.html").display()))
}

/// Markdown for the whole module from `gomarkdoc ./...` (combined, on stdout).
/// Errors if gomarkdoc isn't installed or fails — the caller then falls back.
fn go_markdown(module_dir: &Path) -> Result<String> {
    let output = Command::new("gomarkdoc")
        .arg("./...")
        .current_dir(module_dir)
        .output()
        .context("spawning gomarkdoc (install: `go install github.com/princjef/gomarkdoc/cmd/gomarkdoc@latest`)")?;
    if !output.status.success() {
        bail!("gomarkdoc failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Render gomarkdoc's Markdown to a styled HTML page. gomarkdoc links to types
/// and funcs via explicit `<a name="…">` anchors (which pass through as raw HTML)
/// and to sections (Index, Variables, …) via heading slugs — so we give every
/// heading a GitHub-style slug `id` to make the section links resolve too.
fn render_markdown_page(module: &str, markdown: &str) -> String {
    markdown_html(module, &render_markdown_body(markdown))
}

/// Render Markdown to an HTML **fragment** (no page wrapper): heading slugs for
/// anchor links, syntax-highlighted fenced code, and ` ```mermaid ` blocks emitted
/// as `<pre class="mermaid">` for a client-side Mermaid renderer to draw. Shared by
/// the Go-doc page and pilot's README detail pages.
fn render_markdown_body(markdown: &str) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    // Loaded once per page (cheap relative to the build); used to highlight code.
    let syntaxes = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();
    let theme = &theme_set.themes["base16-ocean.dark"];

    let events: Vec<Event> = Parser::new_ext(markdown, options).collect();
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            // Give each heading a slug id so gomarkdoc's section links resolve.
            Event::Start(Tag::Heading { level, id: None, classes, attrs }) => {
                let mut text = String::new();
                let mut j = i + 1;
                while j < events.len() && !matches!(events[j], Event::End(TagEnd::Heading(_))) {
                    if let Event::Text(t) | Event::Code(t) = &events[j] {
                        text.push_str(t);
                    }
                    j += 1;
                }
                out.push(Event::Start(Tag::Heading {
                    level: *level,
                    id: Some(slugify(&text).into()),
                    classes: classes.clone(),
                    attrs: attrs.clone(),
                }));
                i += 1;
            }
            // Syntax-highlight fenced code blocks (gomarkdoc emits ```go), replacing
            // the block's events with one highlighted HTML fragment.
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let mut code = String::new();
                let mut j = i + 1;
                while j < events.len() && !matches!(events[j], Event::End(TagEnd::CodeBlock)) {
                    if let Event::Text(t) = &events[j] {
                        code.push_str(t);
                    }
                    j += 1;
                }
                // A ```mermaid block becomes a container the client-side Mermaid
                // renderer draws; everything else is syntax-highlighted.
                let rendered = if lang.eq_ignore_ascii_case("mermaid") {
                    format!("<pre class=\"mermaid\">{}</pre>", html_escape(&code))
                } else {
                    highlight_code(&syntaxes, theme, &lang, &code)
                };
                out.push(Event::Html(rendered.into()));
                i = j + 1; // skip the block's Text and End events
            }
            _ => {
                out.push(events[i].clone());
                i += 1;
            }
        }
    }

    let mut body = String::new();
    html::push_html(&mut body, out.into_iter());
    body
}

/// Read a repo's README (any common casing), or `None` when it has none.
fn read_readme(dir: &Path) -> Option<String> {
    for name in ["README.md", "readme.md", "README.markdown", "Readme.md"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

// ── Claude guidance documents ─────────────────────────────────────────────────
//
// pilot's Guidance section: the instruction/context docs that steer Claude across
// the fleet — each fleet member's CLAUDE.md (auto), plus the configured sources
// ([docs] `guidance` in fleet.toml: the global ~/.claude/CLAUDE.md, the drydock
// worker prompt, the skills dir, the memory dir). Each is rendered to an HTML
// fragment at /guidance/<id>.html; the index is guidance.json.

/// One guidance document in the index served at `guidance.json`.
#[derive(Debug, Serialize)]
struct GuidanceDoc {
    /// URL-safe id; the page is `/guidance/<id>` and the fragment `<id>.html`.
    id: String,
    /// Human title.
    label: String,
    /// Grouping for the UI (e.g. `Doctrine`, `Skills`, `Memory`).
    category: String,
}

#[derive(Debug, Serialize)]
struct GuidanceIndex {
    docs: Vec<GuidanceDoc>,
}

/// Harvest the guidance documents, render each to `/guidance/<id>.html`, and write
/// the index to `guidance.json`. Returns how many were emitted (0 → nothing
/// written). Missing/unreadable sources warn and are skipped, never fatal.
fn harvest_guidance(fleet: &Fleet, out: &Path) -> Result<usize> {
    // (category, label, file).
    let mut sources: Vec<(String, String, PathBuf)> = Vec::new();

    // Auto: each fleet member's CLAUDE.md.
    for m in &fleet.members {
        let path = fleet.dir(m).join("CLAUDE.md");
        if path.is_file() {
            sources.push(("Project CLAUDE.md".to_string(), m.label().to_string(), path));
        }
    }

    // Configured sources (files and directories).
    if let Some(docs) = &fleet.docs {
        for src in &docs.guidance {
            let base = resolve_guidance_path(fleet, &src.path);
            if base.is_file() {
                let label = src.label.clone().unwrap_or_else(|| file_stem(&base));
                sources.push((src.category.clone(), label, base));
            } else if base.is_dir() {
                for (label, file) in collect_dir_markdown(&base) {
                    sources.push((src.category.clone(), label, file));
                }
            } else {
                eprintln!("    warning: guidance path not found: {}", base.display());
            }
        }
    }

    if sources.is_empty() {
        return Ok(0);
    }

    let guidance_root = out.join("guidance");
    std::fs::create_dir_all(&guidance_root)
        .with_context(|| format!("creating {}", guidance_root.display()))?;

    let mut index = Vec::new();
    let mut used_ids = std::collections::HashSet::new();
    for (category, label, path) in sources {
        let markdown = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("    warning: reading guidance {}: {err}", path.display());
                continue;
            }
        };
        let base_id = {
            let s = slugify(&format!("{category}-{label}"));
            if s.is_empty() { "doc".to_string() } else { s }
        };
        let mut id = base_id.clone();
        let mut n = 2;
        while !used_ids.insert(id.clone()) {
            id = format!("{base_id}-{n}");
            n += 1;
        }
        let html = render_markdown_body(&markdown);
        let file = guidance_root.join(format!("{id}.html"));
        std::fs::write(&file, html).with_context(|| format!("writing {}", file.display()))?;
        index.push(GuidanceDoc { id, label, category });
    }

    let count = index.len();
    let json = serde_json::to_string_pretty(&GuidanceIndex { docs: index })
        .context("serializing guidance.json")?;
    std::fs::write(out.join("guidance.json"), json).context("writing guidance.json")?;
    Ok(count)
}

/// Resolve a guidance `path`: absolute or `~/…` is expanded as-is; anything else
/// is relative to the fleet root.
fn resolve_guidance_path(fleet: &Fleet, path: &str) -> PathBuf {
    if path.starts_with('/') || path.starts_with("~/") || path == "~" {
        crate::fleet::expand_tilde(path)
    } else {
        fleet.root_dir().join(path)
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("doc").to_string()
}

/// The markdown documents directly under `dir`: each immediate `*.md` file (label
/// = stem) plus each immediate subdir containing `SKILL.md` (label = dir name).
/// A top-level `<name>.md` is dropped when a sibling skill dir `<name>/` exists,
/// so a skills directory and a flat memory directory both harvest cleanly.
fn collect_dir_markdown(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();

    let mut skill_dirs = std::collections::HashSet::new();
    for path in &entries {
        if path.is_dir() && path.join("SKILL.md").is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                skill_dirs.insert(name.to_string());
                out.push((name.to_string(), path.join("SKILL.md")));
            }
        }
    }
    for path in &entries {
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let stem = file_stem(path);
            if !skill_dirs.contains(&stem) {
                out.push((stem, path.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Highlight one code block to an HTML `<pre>`, with per-token color spans from
/// the theme. syntect's inline `<pre>` background is stripped so the page CSS
/// owns the container (border, padding, background); on any failure the code is
/// emitted plain, never lost.
fn highlight_code(
    syntaxes: &syntect::parsing::SyntaxSet,
    theme: &syntect::highlighting::Theme,
    lang: &str,
    code: &str,
) -> String {
    let syntax = syntaxes
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    match syntect::html::highlighted_html_for_string(code, syntaxes, syntax, theme) {
        // Drop the leading `<pre style="background-color:…">` so our CSS controls
        // the container; keep the inner color spans and the closing `</pre>`.
        Ok(html) => match html.find('>') {
            Some(end) => format!("<pre>{}", &html[end + 1..]),
            None => html,
        },
        Err(_) => format!("<pre>{}</pre>", html_escape(code)),
    }
}

/// A GitHub-style heading slug: lowercase, non-alphanumerics dropped, spaces and
/// separators collapsed to single hyphens (e.g. "Variables" → "variables").
fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
            prev_dash = false;
        } else if (ch == ' ' || ch == '-' || ch == '_') && !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// Fallback: one `<section>` per documentable package, from `go doc -all`. A
/// `main` package (a binary) exports nothing, so `go doc` is empty — skip it.
fn go_doc_text_sections(module_dir: &Path) -> Result<String> {
    let packages = go_list(module_dir)?;
    let mut sections = String::new();
    for package in &packages {
        let text = go_doc(module_dir, package)?;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        sections.push_str(&format!(
            "<section><h2>{}</h2><pre>{}</pre></section>\n",
            html_escape(package),
            html_escape(text),
        ));
    }
    if sections.is_empty() {
        bail!("no documentable Go packages in {}", module_dir.display());
    }
    Ok(sections)
}

/// The import paths of every package in the module (`go list ./...`).
fn go_list(module_dir: &Path) -> Result<Vec<String>> {
    let output = Command::new("go")
        .args(["list", "./..."])
        .current_dir(module_dir)
        .output()
        .context("spawning go list")?;
    if !output.status.success() {
        bail!("go list failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The full doc text for one package (`go doc -all <pkg>`).
fn go_doc(module_dir: &Path, package: &str) -> Result<String> {
    let output = Command::new("go")
        .args(["doc", "-all", package])
        .current_dir(module_dir)
        .output()
        .context("spawning go doc")?;
    if !output.status.success() {
        bail!("go doc failed for {package}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The module path from a `go.mod`'s `module` line.
fn go_module_name(module_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(module_dir.join("go.mod")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("module "))
        .map(|s| s.trim().to_owned())
}

/// Escape text for safe inclusion in HTML.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Wrap the rendered package sections in a self-contained dark, monospace page
/// (the fleet's DG-001 palette, inlined since this isn't part of the React app).
fn go_doc_html(module: &str, sections: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{module} — Go docs</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0b0c0d; color:#e6e4dd;
    font-family:"Berkeley Mono","JetBrains Mono","IBM Plex Mono",ui-monospace,Menlo,Consolas,monospace;
    font-size:13px; line-height:1.5; }}
  .wrap {{ max-width:980px; margin:0 auto; padding:2rem 1.5rem; }}
  header {{ border-bottom:1px solid #2a2b2d; padding-bottom:1rem; margin-bottom:1.5rem; }}
  h1 {{ font-size:1.4rem; margin:0; color:#f08c00; }}
  .sub {{ color:#6a675f; font-size:12px; margin-top:.3rem; }}
  section {{ border:1px solid #2a2b2d; background:#121315; margin-bottom:1rem; }}
  h2 {{ font-size:13px; margin:0; padding:.5rem .75rem; background:#16181a;
    border-bottom:1px solid #2a2b2d; color:#9a978d; font-weight:600; }}
  pre {{ margin:0; padding:.75rem; overflow-x:auto; white-space:pre-wrap; }}
</style></head>
<body><div class="wrap">
<header><h1>{module}</h1><div class="sub">Go package documentation · generated by `go doc`</div></header>
{sections}</div></body></html>
"#
    )
}

/// Wrap gomarkdoc-rendered HTML in a styled page (the fleet's DG-001 palette,
/// inlined). Styles the full structure gomarkdoc produces — headings, the index
/// list, fenced code (signatures), tables, and links — so it reads like real
/// docs rather than a text dump.
fn markdown_html(module: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{module} — Go docs</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0b0c0d; color:#e6e4dd;
    font-family:"Berkeley Mono","JetBrains Mono","IBM Plex Mono",ui-monospace,Menlo,Consolas,monospace;
    font-size:13px; line-height:1.6; }}
  .wrap {{ max-width:920px; margin:0 auto; padding:2rem 1.5rem; }}
  a {{ color:#f08c00; text-decoration:none; }}
  a:hover {{ text-decoration:underline; }}
  h1 {{ font-size:1.5rem; color:#f08c00; margin:2rem 0 .5rem;
    padding-top:1rem; border-top:1px solid #2a2b2d; }}
  h1:first-of-type {{ border-top:0; padding-top:0; margin-top:0; }}
  h2 {{ font-size:1.1rem; color:#e6e4dd; margin:1.75rem 0 .5rem; }}
  h3, h4 {{ font-size:1rem; color:#9a978d; margin:1.25rem 0 .4rem; }}
  p {{ color:#c9c6bd; }}
  ul {{ padding-left:1.25rem; }}
  li {{ margin:.15rem 0; }}
  code {{ background:#16181a; border:1px solid #2a2b2d; border-radius:3px;
    padding:.05rem .3rem; font-size:.92em; }}
  pre {{ background:#121315; border:1px solid #2a2b2d; border-radius:4px;
    padding:.75rem 1rem; overflow-x:auto; }}
  pre code {{ background:none; border:0; padding:0; }}
  table {{ border-collapse:collapse; margin:.5rem 0; }}
  th, td {{ border:1px solid #2a2b2d; padding:.3rem .6rem; text-align:left; }}
  hr {{ border:0; border-top:1px solid #2a2b2d; margin:1.5rem 0; }}
</style></head>
<body><div class="wrap">{body}</div></body></html>
"#
    )
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
        MetaPackage {
            name: name.into(),
            description: description.map(Into::into),
            targets,
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn dependencies_split_internal_vs_external_and_skip_dev() {
        let dep = |name: &str, kind: Option<&str>| MetaDep {
            name: name.into(),
            kind: kind.map(Into::into),
        };
        let packages = vec![
            MetaPackage {
                name: "svc-server".into(),
                description: None,
                targets: vec![],
                dependencies: vec![
                    dep("svc-core", None),       // internal edge
                    dep("axum", None),           // external
                    dep("tempfile", Some("dev")), // dev — excluded
                ],
            },
            MetaPackage {
                name: "svc-core".into(),
                description: None,
                targets: vec![],
                dependencies: vec![dep("serde", None)],
            },
        ];
        let deps = harvest_dependencies(&packages);
        assert_eq!(deps.internal.len(), 1);
        assert_eq!(deps.internal[0].from, "svc-server");
        assert_eq!(deps.internal[0].to, "svc-core");
        // External: sorted, deduped, dev-dependency excluded.
        assert_eq!(deps.external, vec!["axum", "serde"]);
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

    #[test]
    fn slugify_matches_gomarkdoc_section_anchors() {
        // gomarkdoc's section links (#variables, #index) target heading slugs.
        assert_eq!(slugify("Variables"), "variables");
        assert_eq!(slugify("Index"), "index");
        // Multi-word / punctuated headings collapse separators to single hyphens.
        assert_eq!(slugify("type Clip"), "type-clip");
        assert_eq!(slugify("func (d *DB) GetClip"), "func-d-db-getclip");
        assert_eq!(slugify(""), "");
    }
}
