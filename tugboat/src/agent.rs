//! `tugboat agent` — install a per-user binary on the dev machines themselves.
//!
//! Distinct from the VPS deploy engine (`deploy.rs`), which targets a
//! root-owned systemd *system* service on the `deepwa7er` host. An "agent" target
//! is a binary that lives on a dev machine, installed locally or over SSH with an
//! atomic swap. It comes in two shapes:
//!
//! - a **daemon** — restarted after install via a launchd login agent (macOS) or
//!   a `systemd --user` unit (Linux), like `tidepool-clipd`;
//! - a **CLI tool** — just a binary on `PATH`, with no unit to restart; set
//!   neither `launchd` nor `systemd_user`.
//!
//! The binary is built per target (cross-compiled when `goos`/`goarch` are given,
//! or a native build when they aren't). No health-check/rollback/ledger — these
//! are trivially replaceable user binaries, not the VPS's load-bearing services.

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
    /// Build command, run in the manifest's directory once per target. `{out}` is
    /// the output path the binary must end up at. `{goos}`/`{goarch}` expand to the
    /// target platform for a cross-compiled build (e.g. Go); omit them in both the
    /// command and the target for a native build (e.g. `cargo build` for this
    /// machine's own CLI).
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
    /// Target OS for a cross-compiled build (`{goos}`). Omit for a native build.
    #[serde(default)]
    pub goos: Option<String>,
    /// Target arch for a cross-compiled build (`{goarch}`). Omit for a native build.
    #[serde(default)]
    pub goarch: Option<String>,
    /// Absolute install path (a leading `~/` is expanded on the target).
    pub dest: String,
    /// Restart as this launchd agent label (daemon). Mutually exclusive with
    /// `systemd_user`; omit both for a CLI tool (nothing to restart).
    #[serde(default)]
    pub launchd: Option<String>,
    /// Restart as this `systemd --user` unit (daemon). Mutually exclusive with
    /// `launchd`; omit both for a CLI tool (nothing to restart).
    #[serde(default)]
    pub systemd_user: Option<String>,
}

impl Target {
    /// The shell command that restarts the daemon on its machine, or `None` for a
    /// CLI tool (neither `launchd` nor `systemd_user` set — nothing to restart).
    fn restart_cmd(&self) -> Option<String> {
        match (&self.launchd, &self.systemd_user) {
            (Some(label), None) => {
                Some(format!("launchctl kickstart -k gui/$(id -u)/{}", shq(label)))
            }
            (None, Some(unit)) => Some(format!("systemctl --user restart {}", shq(unit))),
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("validated: at most one restart kind"),
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
    let mut build_cmd = manifest.build.replace("{out}", &out.to_string_lossy());
    if let Some(goos) = &target.goos {
        build_cmd = build_cmd.replace("{goos}", goos);
    }
    if let Some(goarch) = &target.goarch {
        build_cmd = build_cmd.replace("{goarch}", goarch);
    }
    // A leftover placeholder means the build references a platform the target
    // didn't set — fail clearly rather than run a command with a literal `{goos}`.
    for placeholder in ["{goos}", "{goarch}"] {
        if build_cmd.contains(placeholder) {
            bail!(
                "agent target `{}`: build references `{placeholder}` but the target doesn't set it",
                target.name
            );
        }
    }
    let restart = target.restart_cmd();

    if dry_run {
        let platform = match (&target.goos, &target.goarch) {
            (Some(os), Some(arch)) => format!("{os}/{arch}"),
            _ => "native".to_string(),
        };
        println!("  build:   ({platform}) {build_cmd}");
        println!("  install: {} → {}", target.location(), target.dest);
        println!("  restart: {}", restart.as_deref().unwrap_or("(none — CLI)"));
        return Ok(());
    }

    let platform = match (&target.goos, &target.goarch) {
        (Some(os), Some(arch)) => format!("{os}/{arch}"),
        _ => "native".to_string(),
    };
    println!("==> BUILD ({platform}): {build_cmd}");
    run_in(dir, &build_cmd)?;
    if !out.is_file() {
        bail!("build produced no binary at {}", out.display());
    }

    match &target.ssh {
        Some(host) => install_remote(host, &out, &target.dest, restart.as_deref()),
        None => install_local(&out, &target.dest, restart.as_deref()),
    }
}

/// Install on this machine: atomic swap (write beside the dest, then rename, so a
/// running binary is replaced safely), then restart the daemon if there is one.
fn install_local(built: &Path, dest: &str, restart_cmd: Option<&str>) -> Result<()> {
    let dest = expand_tilde(dest);
    let staged = with_suffix(&dest, ".tug-new");
    println!("==> INSTALL (local): {}", dest.display());

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating install dir {}", parent.display()))?;
    }
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

    match restart_cmd {
        Some(cmd) => {
            println!("==> RESTART: {cmd}");
            run_local(cmd)
        }
        None => {
            println!("    installed (CLI — no restart)");
            Ok(())
        }
    }
}

/// Install on a remote machine over SSH: rsync the binary beside the dest, then a
/// remote atomic swap, and restart the daemon if there is one.
fn install_remote(host: &str, built: &Path, dest: &str, restart_cmd: Option<&str>) -> Result<()> {
    let log = StdoutSink;
    let staged = format!("{dest}.tug-new");
    println!("==> SHIP: {} → {host}:{dest}", built.display());
    deploy::rsync(built, &format!("{host}:{staged}"), ArtifactKind::File, &log)?;

    let action = if restart_cmd.is_some() { "INSTALL + RESTART" } else { "INSTALL" };
    println!("==> {action} on {host}");
    // `dest` may contain a leading `~`, which the remote shell expands only when
    // unquoted; agent paths have no spaces, so this is safe.
    let restart_line = restart_cmd.map(|c| format!("{c}\n")).unwrap_or_default();
    let script = format!(
        "set -euo pipefail\nmkdir -p \"$(dirname {dest})\"\nchmod 755 {staged}\nmv {staged} {dest}\n{restart_line}echo \"    installed {dest}\""
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
        // A daemon sets exactly one restart kind; a CLI tool sets neither.
        if t.launchd.is_some() && t.systemd_user.is_some() {
            bail!("agent target `{}`: set `launchd` or `systemd_user`, not both", t.name);
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
