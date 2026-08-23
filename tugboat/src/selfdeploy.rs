//! `tugboat self-deploy` — rebuild tugboat and roll the new binary into the
//! running `tugboat serve` service on **this** machine — a launchd agent on
//! macOS, a systemd user unit on the Linux boxes.
//!
//! Every other deploy targets a remote host over ssh and restarts it with
//! systemd. tugboat itself runs locally, and the hazard is self-restart: the
//! daemon cannot cleanly restart itself in the middle of handling a request.
//! This command sidesteps that by being **CLI-only** — it runs as a separate,
//! short-lived process, so swapping the binary and restarting the service never
//! kills the process doing the work.
//!
//! Flow: build → atomically swap the installed binary (backing up the old one) →
//! restart the service (`launchctl kickstart -k` on macOS, `systemctl --user
//! restart` on Linux; the manager is detected from PATH) → poll its
//! unauthenticated `GET /health` until it reports a newer start time, proving
//! it came back up on the new binary. If it doesn't return in time (e.g. the
//! new build crashes on boot), the previous binary is restored and restarted,
//! so a bad build can't leave the daemon down.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::fleet::expand_tilde;
use crate::version::BuildInfo;

/// The launchd agent label from `deploy/tugboat-serve.plist` (macOS).
const LAUNCHD_LABEL: &str = "com.deepwa7er.tugboat-serve";
/// The systemd user unit name on the Linux boxes (installed by the desktop
/// provisioning; the `tugboat serve` daemon runs under `systemctl --user`).
const SYSTEMD_UNIT: &str = "tugboat-serve";
/// Where `cargo install` puts the binary the agent runs (see the plist).
const DEFAULT_INSTALL: &str = "~/.cargo/bin/tugboat";
/// How often to re-probe `/health` while waiting for the restart.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub struct SelfDeployArgs {
    /// The tugboat source checkout to build.
    pub repo: PathBuf,
    /// Where to install the binary (default `~/.cargo/bin/tugboat`).
    pub install_path: Option<PathBuf>,
    /// launchd agent label / systemd unit name to restart. Defaults to the
    /// platform's own service (launchd agent `com.deepwa7er.tugboat-serve` on
    /// macOS, systemd user unit `tugboat-serve` on Linux).
    pub label: Option<String>,
    /// Full `/health` URL to poll (default derived from `tailscale ip -4`).
    pub health_url: Option<String>,
    /// Port for the derived health URL when `--health-url` isn't given.
    pub port: u16,
    /// How long to wait for the daemon to come back, in seconds.
    pub timeout_secs: u64,
    /// Reuse `target/release/tugboat` instead of rebuilding.
    pub skip_build: bool,
    /// Print the plan and exit without changing anything.
    pub dry_run: bool,
}

/// What `GET /health` returns. `pid` + `started_unix` together identify a daemon
/// instance, so self-deploy can tell the restarted process from the old one.
#[derive(Deserialize)]
struct RemoteHealth {
    build: BuildInfo,
    pid: u32,
    started_unix: u64,
}

impl RemoteHealth {
    /// Whether this looks like a *different* daemon instance than `prev` — a newer
    /// start time, or (covering two restarts within the same wall-clock second) a
    /// different pid. launchd gives the restarted process a fresh pid.
    fn is_restart_of(&self, prev: &RemoteHealth) -> bool {
        self.started_unix > prev.started_unix || self.pid != prev.pid
    }
}

pub fn run(args: SelfDeployArgs) -> Result<()> {
    let repo = args
        .repo
        .canonicalize()
        .with_context(|| format!("repo not found: {}", args.repo.display()))?;
    verify_tugboat_repo(&repo)?;

    let manager = detect_manager()?;
    let target = args
        .label
        .clone()
        .unwrap_or_else(|| default_target(manager).to_owned());
    let restart_cmd = restart_argv(manager, &target)?.join(" ");

    let install = match args.install_path {
        Some(p) => p,
        None => expand_tilde(DEFAULT_INSTALL),
    };
    let health_url = match &args.health_url {
        Some(u) => u.clone(),
        None => derive_health_url(args.port)?,
    };
    let new_bin = target_dir(&repo)?.join("release/tugboat");

    if args.dry_run {
        println!("DRY RUN — plan for self-deploy\n");
        println!("  build:   {}", if args.skip_build { "(skipped)" } else { "cargo build --release" });
        println!("  repo:    {}", repo.display());
        println!("  install: {} → {}", new_bin.display(), install.display());
        println!("  restart: {restart_cmd}");
        println!("  health:  poll {} until it reports a newer start time", health_url);
        println!("  timeout: {}s", args.timeout_secs);
        return Ok(());
    }

    // 1. Build.
    if args.skip_build {
        note("skipping build (--skip-build)");
    } else {
        step("BUILD", "cargo build --release");
        build(&repo)?;
    }
    if !new_bin.is_file() {
        bail!("binary not found after build: {}", new_bin.display());
    }
    if let Some(info) = binary_info(&new_bin) {
        note(&format!("built {}", info.describe()));
    }

    // 2. Snapshot the running daemon so we can tell it apart from the restarted
    //    instance. Unreachable here just means it's currently down.
    let prev = probe_health(&health_url);
    if prev.is_none() {
        note(&format!("daemon not currently reachable at {health_url}; will install and start"));
    }

    // 3. Atomically swap the binary, keeping a backup to roll back to.
    step("INSTALL", &format!("{} → {}", new_bin.display(), install.display()));
    let backup = install_swap(&new_bin, &install)?;

    // 4. Restart the agent and wait for the new instance to report in. Any
    //    failure past the swap rolls the old binary back.
    let outcome = (|| -> Result<RemoteHealth> {
        step("RESTART", &restart_cmd);
        restart(&manager, &target)?;
        step("HEALTH", &format!("waiting for {health_url} (up to {}s)", args.timeout_secs));
        wait_for_restart(&health_url, prev.as_ref(), Duration::from_secs(args.timeout_secs))
    })();

    match outcome {
        Ok(running) => {
            if let Some(bak) = &backup {
                let _ = std::fs::remove_file(bak);
            }
            println!("\n✓ tugboat self-deployed — now running {}", running.build.describe());
            Ok(())
        }
        Err(err) => {
            eprintln!("!! self-deploy failed; rolling back to the previous binary");
            rollback(&backup, &install)?;
            let _ = restart(&manager, &target);
            Err(err).context("self-deploy failed; rolled back to the previous binary")
        }
    }
}

/// Build a release binary in `repo`, streaming cargo's output to the terminal.
fn build(repo: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(repo)
        .status()
        .context("spawning cargo build")?;
    if !status.success() {
        bail!("cargo build --release failed");
    }
    Ok(())
}

/// Copy `new_bin` into place at `install`, atomically (stage beside the
/// destination, then rename). Any existing binary is copied to `<install>.tug-bak`
/// first; the returned path is that backup, or `None` if nothing was there.
fn install_swap(new_bin: &Path, install: &Path) -> Result<Option<PathBuf>> {
    let dir = install.parent().context("install path has no parent directory")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating install dir {}", dir.display()))?;

    let staged = with_suffix(install, ".tug-new");
    std::fs::copy(new_bin, &staged)
        .with_context(|| format!("staging {} → {}", new_bin.display(), staged.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod 0755 {}", staged.display()))?;

    let backup = if install.exists() {
        let bak = with_suffix(install, ".tug-bak");
        std::fs::copy(install, &bak)
            .with_context(|| format!("backing up {} → {}", install.display(), bak.display()))?;
        Some(bak)
    } else {
        None
    };

    std::fs::rename(&staged, install)
        .with_context(|| format!("installing {} → {}", staged.display(), install.display()))?;
    Ok(backup)
}

/// Restore the backed-up binary over the installed one (atomic rename). With no
/// backup there is nothing to restore — the daemon had no prior binary.
fn rollback(backup: &Option<PathBuf>, install: &Path) -> Result<()> {
    match backup {
        Some(bak) => std::fs::rename(bak, install)
            .with_context(|| format!("restoring {} → {}", bak.display(), install.display())),
        None => {
            eprintln!("    no previous binary to restore at {}", install.display());
            Ok(())
        }
    }
}

/// The service manager the daemon runs under — detected from PATH, since the
/// same binary serves both hosts: macOS runs `tugboat serve` as a launchd
/// agent, the Linux boxes as a systemd user unit.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ServiceManager {
    Launchd,
    Systemd,
}

/// Pick the manager from which service binaries exist. Pure so the precedence
/// is unit-testable without touching PATH. launchd wins when both are present
/// (that combination only happens inside a macOS container with systemd tools
/// installed, where the GUI-domain kickstart is still the right restart).
fn resolve_manager(has_launchctl: bool, has_systemctl: bool) -> Result<ServiceManager> {
    match (has_launchctl, has_systemctl) {
        (true, _) => Ok(ServiceManager::Launchd),
        (false, true) => Ok(ServiceManager::Systemd),
        (false, false) => bail!(
            "neither `launchctl` (macOS) nor `systemctl` (Linux) is on PATH; \
             cannot restart the daemon — pass --label and restart the service yourself"
        ),
    }
}

fn detect_manager() -> Result<ServiceManager> {
    resolve_manager(which("launchctl"), which("systemctl"))
}

/// Whether `bin` is executable somewhere on PATH. No shell involved.
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// The platform's own service name, when `--label` isn't given.
fn default_target(manager: ServiceManager) -> &'static str {
    match manager {
        ServiceManager::Launchd => LAUNCHD_LABEL,
        ServiceManager::Systemd => SYSTEMD_UNIT,
    }
}

/// The argv that restarts the daemon under `manager`. Pure given the uid
/// (launchd addresses its GUI domain by numeric uid), so both command shapes
/// are unit-testable without a service manager present.
fn restart_argv(manager: ServiceManager, target: &str) -> Result<Vec<String>> {
    Ok(match manager {
        ServiceManager::Launchd => vec![
            "launchctl".to_owned(),
            "kickstart".to_owned(),
            "-k".to_owned(),
            format!("gui/{}/{target}", uid()?),
        ],
        ServiceManager::Systemd => vec![
            "systemctl".to_owned(),
            "--user".to_owned(),
            "restart".to_owned(),
            target.to_owned(),
        ],
    })
}

/// Restart the daemon service, dispatching on the detected manager. `kickstart
/// -k` kills the launchd agent and starts a fresh one from the installed
/// binary; `systemctl --user restart` does the same for the systemd unit.
fn restart(manager: &ServiceManager, target: &str) -> Result<()> {
    let argv = restart_argv(*manager, target)?;
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("spawning {}", argv[0]))?;
    if !status.success() {
        bail!("`{}` failed (is the service loaded?)", argv.join(" "));
    }
    Ok(())
}

/// Poll `/health` until the daemon reports a different instance than `prev` (or
/// any live instance, when it was down before), or the timeout elapses.
fn wait_for_restart(
    url: &str,
    prev: Option<&RemoteHealth>,
    timeout: Duration,
) -> Result<RemoteHealth> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(h) = probe_health(url) {
            let restarted = match prev {
                Some(prev) => h.is_restart_of(prev),
                None => true,
            };
            if restarted {
                return Ok(h);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "daemon did not report a restart within {}s at {url}",
                timeout.as_secs()
            );
        }
        sleep(POLL_INTERVAL);
    }
}

/// One health probe. `None` on any failure (unreachable, non-2xx, bad JSON).
fn probe_health(url: &str) -> Option<RemoteHealth> {
    let out = Command::new("curl")
        .args(["-fs", "--max-time", "5", url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Read a binary's build identity by running `<bin> version --json`.
fn binary_info(bin: &Path) -> Option<BuildInfo> {
    let out = Command::new(bin).args(["version", "--json"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Derive `http://<tailnet-ip>:<port>/health` the same way the plist computes the
/// daemon's `--bind`. Errors (rather than guessing loopback) if tailscale can't
/// answer, since the daemon binds the tailnet IP, not loopback.
fn derive_health_url(port: u16) -> Result<String> {
    let out = Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .context("running `tailscale ip -4` to derive the daemon URL")?;
    if !out.status.success() {
        bail!("`tailscale ip -4` failed; pass --health-url explicitly");
    }
    let ip = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if ip.is_empty() {
        bail!("`tailscale ip -4` returned no address; pass --health-url explicitly");
    }
    Ok(format!("http://{ip}:{port}/health"))
}

/// The calling user's numeric uid (for the launchd `gui/<uid>` domain).
fn uid() -> Result<u32> {
    let out = Command::new("id").arg("-u").output().context("spawning `id -u`")?;
    if !out.status.success() {
        bail!("`id -u` failed");
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("parsing uid from `id -u`")
}

/// Fail unless `repo` is a checkout of the tugboat crate itself.
/// The cargo target directory for `repo`, from `cargo metadata`. Inside the
/// fleet workspace this is the workspace root's `target/`, not `<repo>/target/`,
/// so the built binary's location can't be assumed from the crate directory.
fn target_dir(repo: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(repo)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    #[derive(Deserialize)]
    struct Metadata {
        target_directory: PathBuf,
    }
    let meta: Metadata =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata output")?;
    Ok(meta.target_directory)
}

fn verify_tugboat_repo(repo: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct CargoToml {
        // Both optional so that a manifest which is *valid TOML but the wrong
        // manifest* is diagnosed here, with a remedy, rather than by serde. A
        // workspace root has no `[package]`, and since tugboat moved into the
        // monorepo that root is the likeliest thing `--repo` (default `.`)
        // points at — it produced `missing field 'package'` against line 1 of a
        // file the reader never knowingly chose.
        package: Option<CargoPackage>,
        workspace: Option<toml::Value>,
    }
    #[derive(Deserialize)]
    struct CargoPackage {
        name: String,
    }

    let cargo = repo.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo)
        .with_context(|| format!("reading {} (is --repo a tugboat checkout?)", cargo.display()))?;
    let parsed: CargoToml =
        toml::from_str(&text).with_context(|| format!("parsing {}", cargo.display()))?;

    let Some(package) = parsed.package else {
        if parsed.workspace.is_some() {
            bail!(
                "{} is a cargo workspace root, not a crate — \
                 run `tugboat self-deploy` from the tugboat directory itself \
                 (`cd {}/tugboat`), or pass `--repo {}/tugboat`",
                cargo.display(),
                repo.display(),
                repo.display()
            );
        }
        bail!(
            "{} declares no [package]; point --repo at the tugboat checkout",
            cargo.display()
        );
    };
    if package.name != "tugboat" {
        bail!(
            "{} is the `{}` crate, not tugboat; point --repo at the tugboat checkout",
            cargo.display(),
            package.name
        );
    }
    Ok(())
}

/// Append `suffix` to a path's filename (`/a/tugboat` + `.tug-new` →
/// `/a/tugboat.tug-new`), keeping it in the same directory for an atomic rename.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut name = p.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn step(tag: &str, msg: &str) {
    println!("==> {tag}: {msg}");
}

fn note(msg: &str) {
    println!("==> {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory holding one `Cargo.toml`. Named per test so
    /// concurrent tests can't collide.
    fn repo_with(name: &str, manifest: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tugboat-selfdeploy-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        dir
    }

    #[test]
    fn accepts_the_tugboat_crate() {
        let dir = repo_with("ok", "[package]\nname = \"tugboat\"\nversion = \"0.1.0\"\n");
        assert!(verify_tugboat_repo(&dir).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_workspace_root_names_the_remedy() {
        // The mistake this exists for: `--repo` defaults to `.`, and since
        // tugboat moved into the monorepo the root is what you're standing in.
        // Before, serde reported `missing field 'package'` against line 1.
        let dir = repo_with("ws", "# The fleet workspace\n[workspace]\nmembers = [\"tugboat\"]\n");
        let err = verify_tugboat_repo(&dir).unwrap_err().to_string();
        assert!(err.contains("workspace root"), "got: {err}");
        assert!(err.contains("/tugboat"), "must name the directory to use; got: {err}");
        assert!(!err.contains("missing field"), "serde's message must not surface; got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn another_crate_is_named() {
        let dir = repo_with("other", "[package]\nname = \"depot\"\nversion = \"0.1.0\"\n");
        let err = verify_tugboat_repo(&dir).unwrap_err().to_string();
        assert!(err.contains("`depot` crate"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_with_neither_section_still_explains_itself() {
        let dir = repo_with("empty", "[dependencies]\nserde = \"1\"\n");
        let err = verify_tugboat_repo(&dir).unwrap_err().to_string();
        assert!(err.contains("no [package]"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_manifest_is_reported_as_such() {
        let dir = std::env::temp_dir().join(format!("tugboat-selfdeploy-none-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let err = format!("{:#}", verify_tugboat_repo(&dir).unwrap_err());
        assert!(err.contains("tugboat checkout"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_manager_prefers_launchd_then_systemd() {
        assert_eq!(resolve_manager(true, true).unwrap(), ServiceManager::Launchd);
        assert_eq!(resolve_manager(true, false).unwrap(), ServiceManager::Launchd);
        assert_eq!(resolve_manager(false, true).unwrap(), ServiceManager::Systemd);
        assert!(resolve_manager(false, false).is_err());
    }

    #[test]
    fn default_target_matches_platform() {
        assert_eq!(default_target(ServiceManager::Launchd), "com.deepwa7er.tugboat-serve");
        assert_eq!(default_target(ServiceManager::Systemd), "tugboat-serve");
    }

    #[test]
    fn systemd_argv_restarts_the_user_unit() {
        assert_eq!(
            restart_argv(ServiceManager::Systemd, "tugboat-serve").unwrap(),
            vec!["systemctl", "--user", "restart", "tugboat-serve"]
        );
    }

    #[test]
    fn launchd_argv_kickstarts_the_gui_domain() {
        let argv = restart_argv(ServiceManager::Launchd, "com.deepwa7er.tugboat-serve").unwrap();
        assert_eq!(argv[0], "launchctl");
        assert_eq!(argv[1], "kickstart");
        assert_eq!(argv[2], "-k");
        assert_eq!(argv[3], format!("gui/{}/com.deepwa7er.tugboat-serve", uid().unwrap()));
    }
}
