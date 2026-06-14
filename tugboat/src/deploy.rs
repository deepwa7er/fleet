//! The deploy engine: build → ship → atomic install → restart → health-check
//! → rollback-on-failure → (optional) enroll in lighthouse.target → verify.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::manifest::{Health, Manifest, Verify};

/// A temp directory that is removed when this guard drops.
struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn run(
    manifest: &Manifest,
    project_dir: &Path,
    skip_build: bool,
    dry_run: bool,
) -> Result<()> {
    let workdir = std::env::temp_dir()
        .join(format!("tugboat-{}-{}", manifest.name, std::process::id()));
    let workdir_str = workdir.to_string_lossy().into_owned();

    let build_cmd = subst(&manifest.build.cmd, &workdir_str);
    // Resolve artifact sources (relative paths are relative to the manifest dir;
    // an absolute path, e.g. one under {workdir}, is used as-is).
    let artifacts: Vec<(PathBuf, &crate::manifest::Artifact)> = manifest
        .artifacts
        .iter()
        .map(|a| (project_dir.join(subst(&a.src, &workdir_str)), a))
        .collect();

    if dry_run {
        print_plan(manifest, &build_cmd, &artifacts);
        return Ok(());
    }

    std::fs::create_dir_all(&workdir)
        .with_context(|| format!("creating work dir {}", workdir.display()))?;
    let _guard = WorkDir(workdir.clone());

    let host = manifest.host();

    // 1. Build.
    if skip_build {
        note("skipping build (--skip-build)");
    } else {
        step("BUILD", &build_cmd);
        run_local(&build_cmd, project_dir).context("build failed")?;
    }

    // 2. Confirm every artifact exists locally before touching the host.
    for (src, _) in &artifacts {
        if !src.exists() {
            bail!("artifact not found after build: {}", src.display());
        }
    }

    // 3. Ship each artifact next to its destination (same filesystem, so the
    //    install step's rename is atomic).
    for (src, artifact) in &artifacts {
        step("SHIP", &format!("{} → {}:{}", src.display(), host, artifact.dest));
        scp(src, &format!("{host}:{}.tug-new", artifact.dest))?;
    }

    // 4. Atomic install, restart, health-check, rollback-on-failure, enroll —
    //    all in one remote transaction.
    step(
        "INSTALL",
        &format!("{host}: swap binary, restart {}, health-check", manifest.name),
    );
    ssh_script(host, &remote_script(manifest))
        .context("remote install failed (the host rolled back to the previous binary)")?;

    // 5. End-to-end verify from this machine (informational).
    if let Some(verify_cfg) = &manifest.verify {
        step("VERIFY", &verify_cfg.url);
        match verify(verify_cfg) {
            Ok(()) => println!("    reachable at {}", verify_cfg.url),
            Err(err) => eprintln!(
                "    warning: {} not reachable from here ({err}); the service is healthy on the host",
                verify_cfg.url
            ),
        }
    }

    println!("\n✓ {} deployed to {host}", manifest.name);
    Ok(())
}

fn subst(input: &str, workdir: &str) -> String {
    input.replace("{workdir}", workdir)
}

fn run_local(cmd: &str, dir: &Path) -> Result<()> {
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

fn scp(local: &Path, remote: &str) -> Result<()> {
    let status = Command::new("scp")
        .arg("-q")
        .arg(local)
        .arg(remote)
        .status()
        .context("spawning scp")?;
    if !status.success() {
        bail!("scp failed: {} → {remote}", local.display());
    }
    Ok(())
}

fn ssh_script(host: &str, script: &str) -> Result<()> {
    let mut child = Command::new("ssh")
        .arg(host)
        .arg("bash -s")
        .stdin(Stdio::piped())
        .spawn()
        .context("spawning ssh")?;
    child
        .stdin
        .take()
        .context("ssh stdin unavailable")?
        .write_all(script.as_bytes())
        .context("writing remote script")?;
    let status = child.wait().context("waiting on ssh")?;
    if !status.success() {
        bail!("remote script exited with {status}");
    }
    Ok(())
}

fn verify(cfg: &Verify) -> Result<()> {
    for attempt in 1..=cfg.retries {
        let status = Command::new("curl")
            .args(["-fs", "-o", "/dev/null", "--max-time", "12", &cfg.url])
            .status()
            .context("spawning curl")?;
        if status.success() {
            return Ok(());
        }
        if attempt < cfg.retries {
            std::thread::sleep(Duration::from_millis(cfg.interval_ms));
        }
    }
    bail!("not reachable after {} attempts", cfg.retries);
}

/// Quote a value for safe interpolation as a single shell word.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the remote transaction script. Uses a token-replacement template so
/// the bash (which is brace-heavy) stays readable.
fn remote_script(manifest: &Manifest) -> String {
    let name = &manifest.name;
    let dests = manifest
        .artifacts
        .iter()
        .map(|a| shq(&a.dest))
        .collect::<Vec<_>>()
        .join(" ");
    let modes = manifest
        .artifacts
        .iter()
        .map(|a| shq(&a.mode))
        .collect::<Vec<_>>()
        .join(" ");

    let (retries, interval_ms, healthcheck) = match &manifest.health {
        Some(Health { url: Some(url), retries, interval_ms }) => (
            *retries,
            *interval_ms,
            // Silent (-fs, not -fsS): failures during the restart window are
            // expected and retried, so they shouldn't print alarming errors.
            format!("curl -fs -o /dev/null {}", shq(url)),
        ),
        Some(Health { url: None, retries, interval_ms }) => (
            *retries,
            *interval_ms,
            systemctl_healthcheck(name),
        ),
        None => (10, 500, systemctl_healthcheck(name)),
    };
    let interval_s = format!("{}", interval_ms as f64 / 1000.0);

    let enroll = if manifest.lighthouse.enroll {
        format!(
            "$sudo systemctl add-wants lighthouse.target {unit}\n\
             $sudo systemctl daemon-reload\n\
             echo \"    enrolled {name}.service in lighthouse.target\"",
            unit = shq(&format!("{name}.service")),
        )
    } else {
        String::new()
    };

    TEMPLATE
        .replace("@DESTS@", &dests)
        .replace("@MODES@", &modes)
        .replace("@NAME_Q@", &shq(name))
        .replace("@RETRIES@", &retries.to_string())
        .replace("@INTERVAL@", &interval_s)
        .replace("@HEALTHCHECK@", &healthcheck)
        .replace("@ENROLL@", &enroll)
        .replace("@NAME@", name)
}

fn systemctl_healthcheck(name: &str) -> String {
    format!("[ \"$($sudo systemctl is-active {})\" = active ]", shq(name))
}

const TEMPLATE: &str = r#"set -euo pipefail
sudo=""; [ "$(id -u)" -eq 0 ] || sudo="sudo"

DESTS=( @DESTS@ )
MODES=( @MODES@ )

# Atomic install: back up the live file, then rename the new one over it (a
# running ELF can't be written in place, but rename swaps the inode safely).
for i in "${!DESTS[@]}"; do
  d="${DESTS[$i]}"; mode="${MODES[$i]}"
  $sudo chmod "$mode" "$d.tug-new"
  if [ -e "$d" ]; then $sudo cp -a "$d" "$d.tug-bak"; fi
  $sudo mv "$d.tug-new" "$d"
done

$sudo systemctl restart @NAME_Q@

healthy=""
for _ in $(seq 1 @RETRIES@); do
  if @HEALTHCHECK@; then healthy=1; break; fi
  sleep @INTERVAL@
done

if [ -z "$healthy" ]; then
  echo "!! @NAME@ did not become healthy; rolling back" >&2
  for i in "${!DESTS[@]}"; do
    d="${DESTS[$i]}"
    if [ -e "$d.tug-bak" ]; then $sudo mv "$d.tug-bak" "$d"; fi
  done
  $sudo systemctl restart @NAME_Q@ || true
  $sudo systemctl --no-pager --lines=20 status @NAME_Q@ >&2 || true
  exit 1
fi

for i in "${!DESTS[@]}"; do $sudo rm -f "${DESTS[$i]}.tug-bak"; done
echo "    @NAME@ is active and healthy"
@ENROLL@
"#;

fn print_plan(
    manifest: &Manifest,
    build_cmd: &str,
    artifacts: &[(PathBuf, &crate::manifest::Artifact)],
) {
    println!("DRY RUN — plan for {} → {}\n", manifest.name, manifest.host());
    println!("  build:");
    println!("    {build_cmd}");
    println!("  ship:");
    for (src, artifact) in artifacts {
        println!("    {} → {} (mode {})", src.display(), artifact.dest, artifact.mode);
    }
    let health = match &manifest.health {
        Some(Health { url: Some(url), .. }) => format!("curl {url} (on host loopback)"),
        _ => format!("systemctl is-active {}", manifest.name),
    };
    println!("  restart: systemctl restart {}", manifest.name);
    println!("  health:  {health}");
    println!(
        "  enroll:  {}",
        if manifest.lighthouse.enroll {
            "systemctl add-wants lighthouse.target"
        } else {
            "(none)"
        }
    );
    if let Some(v) = &manifest.verify {
        println!("  verify:  {} (from here)", v.url);
    }
}

fn step(tag: &str, msg: &str) {
    println!("==> {tag}: {msg}");
}

fn note(msg: &str) {
    println!("==> {msg}");
}
