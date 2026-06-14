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
    /// SSH host (an alias from `~/.ssh/config`). May instead come from the
    /// overlay, `--host`, or `TUGBOAT_HOST`.
    #[serde(default)]
    pub host: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct Build {
    /// Shell command run locally, in the manifest's directory. `{workdir}`
    /// expands to a fresh temp directory for build output.
    pub cmd: String,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// A single file (e.g. a binary), shipped with scp.
    #[default]
    File,
    /// A directory tree (e.g. built web assets), shipped with rsync --delete.
    Dir,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Artifact {
    /// Local path produced by the build. `{workdir}` is expanded.
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

/// Load `deploy.toml`, apply the optional `deploy.local.toml` overlay and the
/// `--host` override, then validate.
pub fn load(path: &Path, host_override: Option<&str>) -> Result<Manifest> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

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

    validate(&manifest)?;
    Ok(manifest)
}

fn validate(manifest: &Manifest) -> Result<()> {
    if manifest.name.trim().is_empty() {
        bail!("manifest: `name` is required");
    }
    if manifest.host.as_deref().unwrap_or("").is_empty() {
        bail!("manifest: a host is required (set `host`, `--host`, or TUGBOAT_HOST)");
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
