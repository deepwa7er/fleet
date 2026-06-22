//! `tugboat agent` — deploy a per-user daemon to dev machines.
//!
//! Distinct from the VPS deploy engine (`deploy.rs`), which targets a
//! root-owned systemd *system* service on the `deepwa7er` host. An "agent" is a
//! small daemon that runs on the dev machines themselves — a launchd login agent
//! on macOS, a `systemd --user` unit on Linux — like `tidepool-clipd`. Its
//! pure-Go binary cross-compiles for each target from wherever tugboat runs; the
//! binary is shipped (a local install, or rsync over SSH) with an atomic swap,
//! then the agent/unit is restarted. No health-check/rollback/ledger — these are
//! trivially restartable user daemons, not the VPS's load-bearing services.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::deploy::{self, StdoutSink};
use crate::manifest::ArtifactKind;

/// The agent manifest (`agent.toml`).
#[derive(Debug, Deserialize)]
pub struct AgentManifest {
    /// Agent name (also the built binary's filename).
    pub name: String,
    /// Build command, run in the manifest's directory once per target.
    /// `{goos}`/`{goarch}` select the platform; `{out}` is the output path.
    pub build: String,
    pub targets: Vec<Target>,
}

/// One machine to install the agent on.
#[derive(Debug, Deserialize)]
pub struct Target {
    /// Short label (also the `--only` selector).
    pub name: String,
    /// Build + install on this machine. Mutually exclusive with `ssh`.
    #[serde(default)]
    pub local: bool,
    /// SSH target (`user@host`) to install on. Mutually exclusive with `local`.
    #[serde(default)]
    pub ssh: Option<String>,
    pub goos: String,
    pub goarch: String,
    /// Absolute install path (a leading `~/` is expanded on the target).
    pub dest: String,
    /// Restart as this launchd agent label. Mutually exclusive with `systemd_user`.
    #[serde(default)]
    pub launchd: Option<String>,
    /// Restart as this `systemd --user` unit. Mutually exclusive with `launchd`.
    #[serde(default)]
    pub systemd_user: Option<String>,
}

impl Target {
    /// The shell command that restarts the agent on its machine.
    fn restart_cmd(&self) -> String {
        match (&self.launchd, &self.systemd_user) {
            (Some(label), None) => {
                format!("launchctl kickstart -k gui/$(id -u)/{}", shq(label))
            }
            (None, Some(unit)) => format!("systemctl --user restart {}", shq(unit)),
            _ => unreachable!("validated: exactly one restart kind"),
        }
    }

    fn location(&self) -> String {
        match &self.ssh {
            Some(host) => host.clone(),
            None => "local".to_string(),
        }
    }
}

/// `tugboat agent deploy` — build and install the agent on each target.
pub fn deploy(manifest_path: &Path, only: Option<&str>, dry_run: bool) -> Result<()> {
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("agent manifest not found: {}", manifest_path.display()))?;
    let dir = manifest_path.parent().context("manifest has no parent directory")?;
    let manifest = load(&manifest_path)?;

    let targets = select(&manifest, only)?;
    for target in targets {
        println!("\n════ {} → {} ════", manifest.name, target.name);
        deploy_target(dir, &manifest, target, dry_run)?;
    }
    if !dry_run {
        println!("\n✓ {} deployed to: {}", manifest.name, fmt_names(&select(&manifest, only)?));
    }
    Ok(())
}

fn deploy_target(dir: &Path, manifest: &AgentManifest, target: &Target, dry_run: bool) -> Result<()> {
    let workdir = WorkDir::new(&format!("tugboat-agent-{}-{}", manifest.name, target.name))?;
    let out = workdir.path().join(&manifest.name);
    let build_cmd = manifest
        .build
        .replace("{goos}", &target.goos)
        .replace("{goarch}", &target.goarch)
        .replace("{out}", &out.to_string_lossy());

    if dry_run {
        println!("  build:   {build_cmd}");
        println!("  install: {} → {}", target.location(), target.dest);
        println!("  restart: {}", target.restart_cmd());
        return Ok(());
    }

    println!("==> BUILD ({}/{}): {build_cmd}", target.goos, target.goarch);
    run_in(dir, &build_cmd)?;
    if !out.is_file() {
        bail!("build produced no binary at {}", out.display());
    }

    match &target.ssh {
        Some(host) => install_remote(host, &out, &target.dest, &target.restart_cmd()),
        None => install_local(&out, &target.dest, &target.restart_cmd()),
    }
}

/// Install on this machine: atomic swap (write beside the dest, then rename, so a
/// running binary is replaced safely), then restart.
fn install_local(built: &Path, dest: &str, restart_cmd: &str) -> Result<()> {
    let dest = expand_tilde(dest);
    let staged = with_suffix(&dest, ".tug-new");
    println!("==> INSTALL (local): {}", dest.display());

    std::fs::copy(built, &staged)
        .with_context(|| format!("copying to {}", staged.display()))?;
    set_executable(&staged)?;
    // A freshly-built local binary carries no quarantine, but a downloaded one
    // would; strip it best-effort so Gatekeeper doesn't block the launchd agent.
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&staged).status();
    }
    std::fs::rename(&staged, &dest)
        .with_context(|| format!("installing {}", dest.display()))?;

    println!("==> RESTART: {restart_cmd}");
    run_local(restart_cmd)
}

/// Install on a remote machine over SSH: rsync the binary beside the dest, then a
/// remote atomic swap + restart.
fn install_remote(host: &str, built: &Path, dest: &str, restart_cmd: &str) -> Result<()> {
    let log = StdoutSink;
    let staged = format!("{dest}.tug-new");
    println!("==> SHIP: {} → {host}:{dest}", built.display());
    deploy::rsync(built, &format!("{host}:{staged}"), ArtifactKind::File, &log)?;

    println!("==> INSTALL + RESTART on {host}");
    // `dest` may contain a leading `~`, which the remote shell expands only when
    // unquoted; agent paths have no spaces, so this is safe.
    let script = format!(
        "set -euo pipefail\nchmod 755 {staged}\nmv {staged} {dest}\n{restart_cmd}\necho \"    installed {dest}\""
    );
    deploy::ssh_script(host, &script, &log)
}

// ── manifest loading + validation ───────────────────────────────────────────

fn load(path: &Path) -> Result<AgentManifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: AgentManifest =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

    if manifest.name.trim().is_empty() {
        bail!("agent: `name` is required");
    }
    if manifest.build.trim().is_empty() {
        bail!("agent: `build` is required");
    }
    if manifest.targets.is_empty() {
        bail!("agent: at least one `[[targets]]` is required");
    }
    let mut seen = std::collections::HashSet::new();
    for t in &manifest.targets {
        if !seen.insert(t.name.as_str()) {
            bail!("agent: duplicate target name `{}`", t.name);
        }
        match (t.local, &t.ssh) {
            (true, Some(_)) => bail!("agent target `{}`: set `local` or `ssh`, not both", t.name),
            (false, None) => bail!("agent target `{}`: set `local = true` or `ssh = \"user@host\"`", t.name),
            _ => {}
        }
        match (&t.launchd, &t.systemd_user) {
            (Some(_), Some(_)) => bail!("agent target `{}`: set `launchd` or `systemd_user`, not both", t.name),
            (None, None) => bail!("agent target `{}`: set `launchd` or `systemd_user`", t.name),
            _ => {}
        }
        if !t.dest.starts_with('/') && !t.dest.starts_with("~/") {
            bail!("agent target `{}`: `dest` must be absolute or start with `~/`", t.name);
        }
    }
    Ok(manifest)
}

/// Filter targets by an optional comma-separated `--only` set (matched by name).
fn select<'a>(manifest: &'a AgentManifest, only: Option<&str>) -> Result<Vec<&'a Target>> {
    let Some(only) = only else {
        return Ok(manifest.targets.iter().collect());
    };
    let wanted: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let mut chosen = Vec::new();
    for name in &wanted {
        match manifest.targets.iter().find(|t| t.name == *name) {
            Some(t) => chosen.push(t),
            None => bail!("--only: no agent target named `{name}`"),
        }
    }
    Ok(chosen)
}

// ── small helpers ───────────────────────────────────────────────────────────

/// Run a shell command in `dir`, streaming output to this process's stdio.
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

/// Run a shell command in the current directory.
fn run_local(cmd: &str) -> Result<()> {
    let status = Command::new("sh").arg("-c").arg(cmd).status().with_context(|| format!("spawning: {cmd}"))?;
    if !status.success() {
        bail!("command exited with {status}: {cmd}");
    }
    Ok(())
}

/// Expand a leading `~/` to the home directory.
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Append `suffix` to a path's filename (`/a/b` + `.new` → `/a/b.new`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod +x {}", path.display()))
}
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Quote a value for safe interpolation as a single shell word.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn fmt_names(targets: &[&Target]) -> String {
    targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// A temp directory removed when this guard drops.
struct WorkDir(PathBuf);
impl WorkDir {
    fn new(prefix: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating work dir {}", dir.display()))?;
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
