//! The deploy manifest: `deploy.toml`, plus an optional untracked
//! `deploy.local.toml` overlay for host- and tailnet-specific values that
//! shouldn't live in git.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A service's deploy description. Committed to the service repo as `deploy.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Shell command run locally, in the manifest's directory. `{workdir}`
    /// expands to a fresh temp directory for build output; `{workspace}` to the
    /// repository checkout root being built (the cargo workspace root, where
    /// `target/` lives — the manifest's own directory for a standalone repo).
    pub cmd: String,
    /// Capabilities the otherwise-opaque build command requires from tugboat.
    /// Declaring these explicitly keeps the deploy engine from reverse-engineering
    /// build semantics by searching shell text.
    #[serde(default)]
    pub requirements: Vec<BuildRequirement>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildRequirement {
    /// A C compiler, linker driver, and archiver for Rust's static Linux target.
    #[serde(rename = "x86_64-linux-musl")]
    X86_64LinuxMusl,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Lighthouse {
    /// Enroll the unit in `lighthouse.target` so lighthouse discovers it.
    #[serde(default)]
    pub enroll: bool,
}

/// The untracked `deploy.local.toml` overlay. Every field is optional and, when
/// present, replaces the corresponding value from `deploy.toml`.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
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

/// Machine-local deploy settings, kept separate from the committed manifest so
/// a default-branch deployment can apply them to the manifest from that branch
/// without importing build or installation fields from the working tree.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeOverrides {
    local: LocalOverride,
    host: Option<String>,
}

impl Manifest {
    /// The resolved SSH host. Guaranteed present after runtime loading.
    pub fn host(&self) -> &str {
        self.host.as_deref().expect("host validated during loading")
    }
}

/// Read and deserialize a `deploy.toml` without overlay, host resolution, or
/// validation — the shared first step of runtime loading and [`parse`].
fn read_raw(path: &Path) -> Result<Manifest> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_raw(&text, &path.display().to_string())
}

fn parse_raw(text: &str, source: &str) -> Result<Manifest> {
    toml::from_str(text).with_context(|| format!("parsing {source}"))
}

/// Parse a `deploy.toml` for inspection (e.g. doc generation): structural
/// validation only. It applies no `deploy.local.toml` overlay and does not
/// require a deploy host — a host is a *deploy*-time concern, not a property of
/// the service being described.
pub fn parse(path: &Path) -> Result<Manifest> {
    let manifest = read_raw(path)?;
    validate_structure(&manifest)?;
    Ok(manifest)
}

/// Read only the machine-local settings associated with a manifest path.
pub(crate) fn runtime_overrides(
    path: &Path,
    host_override: Option<&str>,
) -> Result<RuntimeOverrides> {
    let local_path = path.with_file_name("deploy.local.toml");
    let local = if local_path.exists() {
        let local_text = fs::read_to_string(&local_path)
            .with_context(|| format!("reading {}", local_path.display()))?;
        toml::from_str(&local_text).with_context(|| format!("parsing {}", local_path.display()))?
    } else {
        LocalOverride::default()
    };

    // Host precedence: --host > TUGBOAT_HOST > overlay/manifest.
    let host = if let Some(host) = host_override {
        Some(host.to_owned())
    } else if let Ok(host) = std::env::var("TUGBOAT_HOST") {
        if !host.is_empty() {
            Some(host)
        } else {
            None
        }
    } else {
        None
    };
    Ok(RuntimeOverrides { local, host })
}

/// Load and validate a committed manifest with previously captured local
/// runtime settings.
pub(crate) fn load_with_overrides(path: &Path, overrides: &RuntimeOverrides) -> Result<Manifest> {
    let manifest = read_raw(path)?;
    finish_load(manifest, overrides)
}

pub(crate) fn load_text_with_overrides(
    text: &str,
    source: &str,
    overrides: &RuntimeOverrides,
) -> Result<Manifest> {
    finish_load(parse_raw(text, source)?, overrides)
}

impl RuntimeOverrides {
    pub(crate) fn host(&self) -> Option<&str> {
        self.host.as_deref().or(self.local.host.as_deref())
    }
}

fn finish_load(mut manifest: Manifest, overrides: &RuntimeOverrides) -> Result<Manifest> {
    if let Some(host) = &overrides.local.host {
        manifest.host = Some(host.clone());
    }
    if let Some(health) = &overrides.local.health {
        manifest.health = Some(health.clone());
    }
    if let Some(verify) = &overrides.local.verify {
        manifest.verify = Some(verify.clone());
    }
    if let Some(lighthouse) = &overrides.local.lighthouse {
        manifest.lighthouse = lighthouse.clone();
    }
    if let Some(host) = &overrides.host {
        manifest.host = Some(host.clone());
    }
    validate_structure(&manifest)?;
    validate_host(&manifest)?;
    Ok(manifest)
}

/// Validate everything intrinsic to the service description — independent of any
/// particular deploy. Shared by [`parse`] and runtime loading.
fn validate_structure(manifest: &Manifest) -> Result<()> {
    if !valid_service_name(&manifest.name) {
        bail!(
            "manifest: `name` must begin with an ASCII letter or digit and contain only ASCII letters, digits, `-`, or `_`"
        );
    }
    if manifest.build.cmd.trim().is_empty() {
        bail!("manifest: `build.cmd` is required");
    }
    let mut requirements = HashSet::new();
    for requirement in &manifest.build.requirements {
        if !requirements.insert(*requirement) {
            bail!("manifest: duplicate build requirement `{requirement}`");
        }
    }
    if manifest.artifacts.is_empty() {
        bail!("manifest: at least one `[[artifacts]]` is required");
    }
    let mut destinations = HashSet::new();
    for artifact in &manifest.artifacts {
        if artifact.src.trim().is_empty() {
            bail!("manifest: artifact `src` must not be empty");
        }
        if !valid_absolute_path(&artifact.dest) {
            bail!(
                "manifest: artifact dest must be a normalized absolute path without `.` or `..`: {}",
                artifact.dest
            );
        }
        if !destinations.insert(artifact.dest.as_str()) {
            bail!(
                "manifest: duplicate artifact destination: {}",
                artifact.dest
            );
        }
        if artifact.mode.len() != 4
            || !artifact.mode.starts_with('0')
            || !artifact
                .mode
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            bail!(
                "manifest: artifact mode must be four octal digits: {}",
                artifact.mode
            );
        }
    }
    if let Some(health) = &manifest.health {
        validate_attempts("health", health.retries, health.interval_ms)?;
        if let Some(url) = &health.url {
            validate_http_url("health.url", url)?;
        }
    }
    if let Some(verify) = &manifest.verify {
        validate_attempts("verify", verify.retries, verify.interval_ms)?;
        validate_http_url("verify.url", &verify.url)?;
    }
    Ok(())
}

/// Validate that a deploy host has been resolved — required at deploy time, but
/// not by [`parse`] (inspection).
fn validate_host(manifest: &Manifest) -> Result<()> {
    let Some(host) = manifest.host.as_deref().filter(|host| !host.is_empty()) else {
        bail!("manifest: a host is required (set `host`, `--host`, or TUGBOAT_HOST)");
    };
    if !valid_ssh_host(host) {
        bail!(
            "manifest: host must begin with an ASCII letter or digit and contain only ASCII letters, digits, `.`, `-`, or `_`"
        );
    }
    Ok(())
}

fn valid_service_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_ssh_host(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_absolute_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        && value != "/"
        && !value.ends_with('/')
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_'))
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn validate_attempts(section: &str, retries: u32, interval_ms: u64) -> Result<()> {
    if retries == 0 {
        bail!("manifest: `{section}.retries` must be greater than zero");
    }
    if interval_ms == 0 {
        bail!("manifest: `{section}.interval_ms` must be greater than zero");
    }
    Ok(())
}

fn validate_http_url(field: &str, url: &str) -> Result<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("manifest: `{field}` must be an http:// or https:// URL");
    }
    Ok(())
}

impl std::fmt::Display for BuildRequirement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::X86_64LinuxMusl => formatter.write_str("x86_64-linux-musl"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(input: &str) -> Result<Manifest> {
        let parsed = toml::from_str(input).context("parsing test manifest")?;
        validate_structure(&parsed)?;
        Ok(parsed)
    }

    const VALID: &str = r#"
name = "example-service"
host = "deepwa7er"

[build]
cmd = "cargo build"
requirements = ["x86_64-linux-musl"]

[[artifacts]]
src = "target/release/example"
dest = "/usr/local/bin/example"
"#;

    #[test]
    fn accepts_a_fully_validated_manifest() {
        let parsed = manifest(VALID).unwrap();
        assert_eq!(
            parsed.build.requirements,
            [BuildRequirement::X86_64LinuxMusl]
        );
    }

    #[test]
    fn rejects_values_that_are_unsafe_for_execution() {
        for (old, replacement) in [
            ("example-service", "$(touch /tmp/nope)"),
            ("/usr/local/bin/example", "/usr/local/../tmp/example"),
            ("/usr/local/bin/example", "/usr/local//bin/example"),
            ("/usr/local/bin/example", "/usr/local/bin/example/"),
            ("/usr/local/bin/example", "/usr/local/bin/bad name"),
            ("target/release/example", "  "),
            (
                "cmd = \"cargo build\"",
                "cmd = \"cargo build\"\nunknown = true",
            ),
        ] {
            let input = VALID.replacen(old, replacement, 1);
            assert!(
                manifest(&input).is_err(),
                "accepted invalid replacement `{replacement}`"
            );
        }
    }

    #[test]
    fn deploy_host_is_validated_after_overrides_are_applied() {
        let mut parsed = manifest(VALID).unwrap();
        assert!(validate_host(&parsed).is_ok());
        parsed.host = Some("-oProxyCommand=bad".to_owned());
        assert!(validate_host(&parsed).is_err());
    }

    #[test]
    fn runtime_overrides_do_not_import_working_tree_build_fields() {
        let dir = tempfile::tempdir().unwrap();
        let working = dir.path().join("working");
        let source = dir.path().join("source");
        std::fs::create_dir_all(&working).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            working.join("deploy.toml"),
            VALID.replace("cargo build", "local build"),
        )
        .unwrap();
        std::fs::write(
            working.join("deploy.local.toml"),
            "[health]\nurl = \"http://127.0.0.1:9000/health\"\n",
        )
        .unwrap();
        std::fs::write(
            source.join("deploy.toml"),
            VALID
                .replace("cargo build", "origin build")
                .replace("target/release/example", "origin/example"),
        )
        .unwrap();

        let overrides =
            runtime_overrides(&working.join("deploy.toml"), Some("runtime-host")).unwrap();
        let loaded = load_with_overrides(&source.join("deploy.toml"), &overrides).unwrap();

        assert_eq!(loaded.build.cmd, "origin build");
        assert_eq!(loaded.artifacts[0].src, "origin/example");
        assert_eq!(loaded.host(), "runtime-host");
        assert_eq!(
            loaded
                .health
                .as_ref()
                .and_then(|health| health.url.as_deref()),
            Some("http://127.0.0.1:9000/health")
        );
    }

    #[test]
    fn rejects_duplicate_destinations_and_requirements() {
        let duplicate_requirement = VALID.replace(
            "[\"x86_64-linux-musl\"]",
            "[\"x86_64-linux-musl\", \"x86_64-linux-musl\"]",
        );
        assert!(manifest(&duplicate_requirement).is_err());

        let duplicate_destination =
            format!("{VALID}\n[[artifacts]]\nsrc = \"other\"\ndest = \"/usr/local/bin/example\"\n");
        assert!(manifest(&duplicate_destination).is_err());
    }

    #[test]
    fn every_committed_fleet_manifest_satisfies_the_validated_schema() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut checked = 0;
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path().join("deploy.toml");
            if path.is_file() {
                parse(&path).unwrap_or_else(|error| {
                    panic!("{} failed validation: {error:#}", path.display())
                });
                checked += 1;
            }
        }
        assert!(checked > 0, "test did not discover any fleet manifests");
    }
}
