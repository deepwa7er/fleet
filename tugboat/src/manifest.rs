//! The deploy manifest: `deploy.toml`, plus an optional untracked
//! `deploy.local.toml` overlay for host- and tailnet-specific values that
//! shouldn't live in git.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A service's deploy description. Committed to the service repo as `deploy.toml`.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    /// Service / systemd unit base name (the unit is `{name}.service`).
    pub name: String,
    /// One-line service description for the docs (`tugboat fleet docs`). When set
    /// it is authoritative — it overrides any crate-level Cargo description, which
    /// is the right call for a workspace (no single crate speaks for the service)
    /// or a non-Rust service (no Cargo description at all). Unused by deploys.
    #[serde(default)]
    pub description: Option<String>,
    /// SSH host (an alias from `~/.ssh/config`). May instead come from the
    /// overlay, `--host`, or `TUGBOAT_HOST`.
    #[serde(default)]
    pub host: Option<String>,
    /// Loopback port the service listens on. THE port authority: `tugboat
    /// fleet gen` derives breakwater's route from it and `fleet docs` publishes
    /// it in fleet.json. Omit for services breakwater doesn't proxy (breakwater
    /// itself, tidepool's own-node HTTPS).
    #[serde(default)]
    pub port: Option<u16>,
    /// The service's durable state under `/var/lib/<name>` on the host,
    /// declared so the fleet backup set is generated (`tugboat fleet gen`)
    /// instead of hand-maintained — the omission that once left two databases
    /// unprotected.
    #[serde(default)]
    pub state: Option<StateDecl>,
    pub build: Build,
    pub artifacts: Vec<Artifact>,
    /// On-host health gate after restart. Omit to default to `systemctl
    /// is-active {name}` (right for services with no loopback listener).
    #[serde(default)]
    pub health: Option<Health>,
    /// Optional end-to-end check run from this machine after a successful
    /// deploy. Informational only — never triggers a rollback.
    #[serde(default)]
    pub verify: Option<Verify>,
    #[serde(default)]
    pub lighthouse: Lighthouse,
}

/// What of `/var/lib/<name>` the fleet backup preserves.
#[derive(Debug, Deserialize, Clone)]
pub struct StateDecl {
    /// A SQLite database file inside the state dir, snapshotted with SQLite's
    /// online-backup API (a raw copy of a live WAL database can tear).
    #[serde(default)]
    pub db: Option<String>,
    /// Back up the state dir's plain files in place (configs, JSONL, caches).
    #[serde(default)]
    pub files: bool,
}

#[derive(Debug, Deserialize)]
pub struct Build {
    /// Shell command run locally, in the manifest's directory. `{workdir}`
    /// expands to a fresh temp directory for build output; `{workspace}` to the
    /// repository checkout root being built (the cargo workspace root, where
    /// `target/` lives — the manifest's own directory for a standalone repo).
    pub cmd: String,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// A single file (e.g. a binary), shipped with rsync.
    #[default]
    File,
    /// A directory tree (e.g. built web assets), shipped with rsync --delete.
    Dir,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Artifact {
    /// Local path produced by the build, relative to the manifest's directory.
    /// `{workdir}` and `{workspace}` are expanded (a cargo binary in the fleet
    /// workspace lives under `{workspace}/target/…`).
    pub src: String,
    /// Absolute remote path. Installed via an atomic rename, with the previous
    /// file/dir backed up and restored if the deploy fails its health check.
    pub dest: String,
    #[serde(default)]
    pub kind: ArtifactKind,
    /// File mode (ignored for `dir` artifacts, which keep their source perms).
    #[serde(default = "default_mode")]
    pub mode: String,
}
fn default_mode() -> String {
    "0755".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Health {
    /// A URL curled on the host's loopback. Omit to use `systemctl is-active`.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_health_retries")]
    pub retries: u32,
    #[serde(default = "default_health_interval")]
    pub interval_ms: u64,
}
fn default_health_retries() -> u32 {
    10
}
fn default_health_interval() -> u64 {
    500
}

#[derive(Debug, Deserialize, Clone)]
pub struct Verify {
    pub url: String,
    #[serde(default = "default_verify_retries")]
    pub retries: u32,
    #[serde(default = "default_verify_interval")]
    pub interval_ms: u64,
}
fn default_verify_retries() -> u32 {
    6
}
fn default_verify_interval() -> u64 {
    2000
}

#[derive(Debug, Deserialize, Default)]
pub struct Lighthouse {
    /// Enroll the unit in `lighthouse.target` so lighthouse discovers it.
    #[serde(default)]
    pub enroll: bool,
}

/// The untracked `deploy.local.toml` overlay. Every field is optional and, when
/// present, replaces the corresponding value from `deploy.toml`.
#[derive(Debug, Deserialize, Default)]
struct LocalOverride {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    health: Option<Health>,
    #[serde(default)]
    verify: Option<Verify>,
    #[serde(default)]
    lighthouse: Option<Lighthouse>,
}

impl Manifest {
    /// The resolved SSH host. Guaranteed present after [`load`] validates it.
    pub fn host(&self) -> &str {
        self.host.as_deref().expect("host validated in load()")
    }
}

/// Read and deserialize a `deploy.toml` without overlay, host resolution, or
/// validation — the shared first step of [`load`] and [`parse`].
fn read_raw(path: &Path) -> Result<Manifest> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Parse a `deploy.toml` for inspection (e.g. doc generation): structural
/// validation only. Unlike [`load`], it applies no `deploy.local.toml` overlay
/// and does not require a deploy host — a host is a *deploy*-time concern, not a
/// property of the service being described.
pub fn parse(path: &Path) -> Result<Manifest> {
    let manifest = read_raw(path)?;
    validate_structure(&manifest)?;
    Ok(manifest)
}

/// Load `deploy.toml`, apply the optional `deploy.local.toml` overlay and the
/// `--host` override, then validate (structure and a resolved host).
pub fn load(path: &Path, host_override: Option<&str>) -> Result<Manifest> {
    let mut manifest = read_raw(path)?;

    let local_path = path.with_file_name("deploy.local.toml");
    if local_path.exists() {
        let local_text = fs::read_to_string(&local_path)
            .with_context(|| format!("reading {}", local_path.display()))?;
        let local: LocalOverride = toml::from_str(&local_text)
            .with_context(|| format!("parsing {}", local_path.display()))?;
        if local.host.is_some() {
            manifest.host = local.host;
        }
        if local.health.is_some() {
            manifest.health = local.health;
        }
        if local.verify.is_some() {
            manifest.verify = local.verify;
        }
        if let Some(lighthouse) = local.lighthouse {
            manifest.lighthouse = lighthouse;
        }
    }

    // Host precedence: --host > TUGBOAT_HOST > overlay/manifest.
    if let Some(host) = host_override {
        manifest.host = Some(host.to_string());
    } else if let Ok(host) = std::env::var("TUGBOAT_HOST") {
        if !host.is_empty() {
            manifest.host = Some(host);
        }
    }

    validate_structure(&manifest)?;
    validate_host(&manifest)?;
    Ok(manifest)
}

/// Validate everything intrinsic to the service description — independent of any
/// particular deploy. Shared by [`parse`] and [`load`].
fn validate_structure(manifest: &Manifest) -> Result<()> {
    if manifest.name.trim().is_empty() {
        bail!("manifest: `name` is required");
    }
    if manifest.build.cmd.trim().is_empty() {
        bail!("manifest: `build.cmd` is required");
    }
    if manifest.artifacts.is_empty() {
        bail!("manifest: at least one `[[artifacts]]` is required");
    }
    for artifact in &manifest.artifacts {
        if !artifact.dest.starts_with('/') {
            bail!("manifest: artifact dest must be an absolute path: {}", artifact.dest);
        }
    }
    Ok(())
}

/// Validate that a deploy host has been resolved — required by [`load`] (a
/// deploy), but not by [`parse`] (inspection).
fn validate_host(manifest: &Manifest) -> Result<()> {
    if manifest.host.as_deref().unwrap_or("").is_empty() {
        bail!("manifest: a host is required (set `host`, `--host`, or TUGBOAT_HOST)");
    }
    Ok(())
}
