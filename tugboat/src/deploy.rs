//! The deploy engine: build → ship → atomic install → restart → health-check
//! → rollback-on-failure → (optional) enroll in lighthouse.target → verify.
//!
//! All human-facing progress goes through a [`LogSink`] rather than straight to
//! stdout, so the same pipeline drives both the `tugboat deploy` CLI (which
//! prints to the terminal) and `tugboat serve` (which streams the transcript to
//! a browser). Subprocess stdout/stderr is captured and forwarded line-by-line
//! into the sink as it arrives, so the log stays live even when no terminal is
//! attached.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::manifest::{ArtifactKind, Health, Manifest, Verify};

/// A destination for the deploy transcript: progress lines emitted by the engine
/// plus every line of captured subprocess output. Implementations must be
/// `Sync` because stdout and stderr are drained from separate threads.
pub trait LogSink: Send + Sync {
    fn line(&self, line: &str);
}

/// Writes the transcript straight to this process's stdout — the CLI's sink.
pub struct StdoutSink;
impl LogSink for StdoutSink {
    fn line(&self, line: &str) {
        // `println!` locks stdout, so concurrent stdout/stderr reader threads
        // can't interleave within a single line.
        println!("{line}");
    }
}

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
    log: &dyn LogSink,
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
        print_plan(manifest, &build_cmd, &artifacts, log);
        return Ok(());
    }

    std::fs::create_dir_all(&workdir)
        .with_context(|| format!("creating work dir {}", workdir.display()))?;
    let _guard = WorkDir(workdir.clone());

    let host = manifest.host();

    // 1. Build.
    if skip_build {
        note(log, "skipping build (--skip-build)");
    } else {
        step(log, "BUILD", &build_cmd);
        run_local(&build_cmd, project_dir, log).context("build failed")?;
    }

    // 2. Confirm every artifact exists locally, of the right kind, before
    //    touching the host.
    for (src, artifact) in &artifacts {
        match artifact.kind {
            ArtifactKind::File if !src.is_file() => {
                bail!("file artifact not found after build: {}", src.display())
            }
            ArtifactKind::Dir if !src.is_dir() => {
                bail!("dir artifact not found after build: {}", src.display())
            }
            _ => {}
        }
    }

    // 3. Ship each artifact next to its destination (same filesystem, so the
    //    install step's rename is atomic): files via scp, dirs via rsync.
    for (src, artifact) in &artifacts {
        let staged = format!("{host}:{}.tug-new", artifact.dest);
        match artifact.kind {
            ArtifactKind::File => {
                step(log, "SHIP", &format!("{} → {}:{}", src.display(), host, artifact.dest));
                scp(src, &staged, log)?;
            }
            ArtifactKind::Dir => {
                step(log, "SHIP DIR", &format!("{}/ → {}:{}", src.display(), host, artifact.dest));
                rsync_dir(src, &staged, log)?;
            }
        }
    }

    // 4. Atomic install, restart, health-check, rollback-on-failure, enroll —
    //    all in one remote transaction.
    step(
        log,
        "INSTALL",
        &format!("{host}: swap binary, restart {}, health-check", manifest.name),
    );
    ssh_script(host, &remote_script(manifest), log)
        .context("remote install failed (the host rolled back to the previous binary)")?;

    // 5. End-to-end verify from this machine (informational).
    if let Some(verify_cfg) = &manifest.verify {
        step(log, "VERIFY", &verify_cfg.url);
        match verify(verify_cfg, log) {
            Ok(()) => log.line(&format!("    reachable at {}", verify_cfg.url)),
            Err(err) => log.line(&format!(
                "    warning: {} not reachable from here ({err}); the service is healthy on the host",
                verify_cfg.url
            )),
        }
    }

    log.line(&format!("\n✓ {} deployed to {host}", manifest.name));
    Ok(())
}

fn subst(input: &str, workdir: &str) -> String {
    input.replace("{workdir}", workdir)
}

/// Run a child process, capturing its stdout and stderr and forwarding every
/// line into `log` as it arrives. If `stdin_data` is given it is written to the
/// child's stdin (concurrently, so a child that writes output while reading its
/// input cannot deadlock). Returns the child's exit status.
fn run_streamed(
    mut cmd: Command,
    stdin_data: Option<&[u8]>,
    log: &dyn LogSink,
) -> Result<ExitStatus> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = cmd.spawn().context("spawning command")?;
    let stdout = child.stdout.take().context("child stdout unavailable")?;
    let stderr = child.stderr.take().context("child stderr unavailable")?;
    let stdin = child.stdin.take();

    // Drain stdout and stderr (and feed stdin) on separate threads so none of
    // the three pipes can fill and stall the others. `scope` lets the threads
    // borrow `log` without an Arc.
    std::thread::scope(|scope| {
        if let (Some(mut stdin), Some(data)) = (stdin, stdin_data) {
            scope.spawn(move || {
                // A broken pipe (child exited early) is not worth surfacing —
                // the exit status below is the real signal.
                let _ = stdin.write_all(data);
                // Dropping `stdin` here closes the pipe so the child sees EOF.
            });
        }
        scope.spawn(|| pipe_lines(stdout, log));
        scope.spawn(|| pipe_lines(stderr, log));
    });

    child.wait().context("waiting on child process")
}

/// Forward each line read from `reader` into `log`. Stops on EOF or read error.
fn pipe_lines<R: Read>(reader: R, log: &dyn LogSink) {
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(line) => log.line(&line),
            Err(_) => break,
        }
    }
}

fn run_local(cmd: &str, dir: &Path, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd).current_dir(dir);
    let status = run_streamed(command, None, log).with_context(|| format!("spawning: {cmd}"))?;
    if !status.success() {
        bail!("command exited with {status}: {cmd}");
    }
    Ok(())
}

fn scp(local: &Path, remote: &str, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("scp");
    command.arg("-q").arg(local).arg(remote);
    let status = run_streamed(command, None, log).context("spawning scp")?;
    if !status.success() {
        bail!("scp failed: {} → {remote}", local.display());
    }
    Ok(())
}

/// Mirror a local directory's contents to a remote path (trailing slashes make
/// rsync copy the contents into `remote`, and `--delete` removes stale files).
///
/// `--no-owner --no-group`: keep permissions/times but do NOT carry the local
/// machine's uid/gid to the server — the files land owned by the remote SSH
/// user, not whatever uid happens to match locally.
fn rsync_dir(local: &Path, remote: &str, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("rsync");
    command
        .args(["-az", "--no-owner", "--no-group", "--delete"])
        .arg(format!("{}/", local.display()))
        .arg(format!("{remote}/"));
    let status = run_streamed(command, None, log).context("spawning rsync")?;
    if !status.success() {
        bail!("rsync failed: {}/ → {remote}/", local.display());
    }
    Ok(())
}

fn ssh_script(host: &str, script: &str, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("ssh");
    command.arg(host).arg("bash -s");
    let status =
        run_streamed(command, Some(script.as_bytes()), log).context("spawning ssh")?;
    if !status.success() {
        bail!("remote script exited with {status}");
    }
    Ok(())
}

fn verify(cfg: &Verify, log: &dyn LogSink) -> Result<()> {
    for attempt in 1..=cfg.retries {
        let mut command = Command::new("curl");
        command.args(["-fs", "-o", "/dev/null", "--max-time", "12", &cfg.url]);
        let status = run_streamed(command, None, log).context("spawning curl")?;
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

/// Shell-quote each item and join with spaces (for a bash array literal).
fn join_quoted<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.map(shq).collect::<Vec<_>>().join(" ")
}

/// Build the remote transaction script. Uses a token-replacement template so
/// the bash (which is brace-heavy) stays readable.
fn remote_script(manifest: &Manifest) -> String {
    let name = &manifest.name;
    let dests = join_quoted(manifest.artifacts.iter().map(|a| a.dest.as_str()));
    let modes = join_quoted(manifest.artifacts.iter().map(|a| a.mode.as_str()));
    let kinds = join_quoted(manifest.artifacts.iter().map(|a| match a.kind {
        ArtifactKind::File => "file",
        ArtifactKind::Dir => "dir",
    }));

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
        .replace("@KINDS@", &kinds)
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
KINDS=( @KINDS@ )

# Atomic install: move the live file/dir aside to .tug-bak, then rename the new
# one into place. rename swaps inodes safely even for a running ELF.
for i in "${!DESTS[@]}"; do
  d="${DESTS[$i]}"; mode="${MODES[$i]}"; kind="${KINDS[$i]}"
  if [ "$kind" = file ]; then
    $sudo chmod "$mode" "$d.tug-new"
    if [ -e "$d" ]; then $sudo cp -a "$d" "$d.tug-bak"; fi
  else
    $sudo rm -rf "$d.tug-bak"
    if [ -e "$d" ]; then $sudo mv "$d" "$d.tug-bak"; fi
  fi
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
    d="${DESTS[$i]}"; kind="${KINDS[$i]}"
    if [ -e "$d.tug-bak" ]; then
      [ "$kind" = dir ] && $sudo rm -rf "$d"
      $sudo mv "$d.tug-bak" "$d"
    fi
  done
  $sudo systemctl restart @NAME_Q@ || true
  $sudo systemctl --no-pager --lines=20 status @NAME_Q@ >&2 || true
  exit 1
fi

for i in "${!DESTS[@]}"; do $sudo rm -rf "${DESTS[$i]}.tug-bak"; done
echo "    @NAME@ is active and healthy"
@ENROLL@
"#;

fn print_plan(
    manifest: &Manifest,
    build_cmd: &str,
    artifacts: &[(PathBuf, &crate::manifest::Artifact)],
    log: &dyn LogSink,
) {
    log.line(&format!("DRY RUN — plan for {} → {}\n", manifest.name, manifest.host()));
    log.line("  build:");
    log.line(&format!("    {build_cmd}"));
    log.line("  ship:");
    for (src, artifact) in artifacts {
        match artifact.kind {
            ArtifactKind::File => log.line(&format!(
                "    {} → {} (file, mode {})",
                src.display(),
                artifact.dest,
                artifact.mode
            )),
            ArtifactKind::Dir => log.line(&format!(
                "    {}/ → {} (dir, rsync --delete)",
                src.display(),
                artifact.dest
            )),
        }
    }
    let health = match &manifest.health {
        Some(Health { url: Some(url), .. }) => format!("curl {url} (on host loopback)"),
        _ => format!("systemctl is-active {}", manifest.name),
    };
    log.line(&format!("  restart: systemctl restart {}", manifest.name));
    log.line(&format!("  health:  {health}"));
    log.line(&format!(
        "  enroll:  {}",
        if manifest.lighthouse.enroll {
            "systemctl add-wants lighthouse.target"
        } else {
            "(none)"
        }
    ));
    if let Some(v) = &manifest.verify {
        log.line(&format!("  verify:  {} (from here)", v.url));
    }
}

fn step(log: &dyn LogSink, tag: &str, msg: &str) {
    log.line(&format!("==> {tag}: {msg}"));
}

fn note(log: &dyn LogSink, msg: &str) {
    log.line(&format!("==> {msg}"));
}
