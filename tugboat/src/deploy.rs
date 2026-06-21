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
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::git;
use crate::manifest::{ArtifactKind, Health, Manifest, Verify};

/// What was deployed, recorded on the host so the dashboard can tell whether a
/// service is running the latest local code. Written only on a successful
/// deploy; a rolled-back deploy leaves the previous stamp untouched.
struct Stamp {
    sha: String,
    short: String,
    dirty: bool,
    branch: Option<String>,
    deployed_at: u64,
}

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

/// Wraps another sink and also accumulates every line, so the engine can persist
/// the full transcript to the host after the deploy finishes — regardless of
/// whether the live sink is the CLI's stdout or the daemon's SSE channel.
struct CapturingSink<'a> {
    inner: &'a dyn LogSink,
    lines: Mutex<Vec<String>>,
}
impl<'a> CapturingSink<'a> {
    fn new(inner: &'a dyn LogSink) -> Self {
        Self { inner, lines: Mutex::new(Vec::new()) }
    }
    /// The accumulated transcript as one newline-joined string.
    fn transcript(&self) -> String {
        self.lines.lock().expect("transcript lock").join("\n")
    }
}
impl LogSink for CapturingSink<'_> {
    fn line(&self, line: &str) {
        self.inner.line(line);
        if let Ok(mut lines) = self.lines.lock() {
            lines.push(line.to_owned());
        }
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it, which never
/// happens in practice).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    // Stamp the deploy at its start so the id (transcript filename), the ledger
    // `at`, and the on-host transcript all agree on one timestamp. Tee the live
    // sink through a capturing one so we can persist the full transcript below.
    let at = now_unix();
    let stamp = build_stamp(project_dir, at);
    let id = deploy_id(at, stamp.as_ref());
    let cap = CapturingSink::new(log);
    let orig = log;
    let log: &dyn LogSink = &cap;

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
    //    install step's rename is atomic). Both files and directories go over
    //    rsync — one transfer path, with compression on the binary for free.
    for (src, artifact) in &artifacts {
        let staged = format!("{host}:{}.tug-new", artifact.dest);
        match artifact.kind {
            ArtifactKind::File => {
                step(log, "SHIP", &format!("{} → {}:{}", src.display(), host, artifact.dest));
            }
            ArtifactKind::Dir => {
                step(log, "SHIP DIR", &format!("{}/ → {}:{}", src.display(), host, artifact.dest));
            }
        }
        rsync(src, &staged, artifact.kind, log)?;
    }

    // 4. Atomic install, restart, health-check, rollback-on-failure, enroll,
    //    and record what was deployed — all in one remote transaction.
    step(
        log,
        "INSTALL",
        &format!("{host}: swap binary, restart {}, health-check", manifest.name),
    );
    let install = ssh_script(host, &remote_script(manifest, stamp.as_ref(), &id), log);

    // 5. End-to-end verify from this machine (informational) — only on success.
    if install.is_ok() {
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
    }

    // 6. Persist the full transcript next to the ledger on the host — for both
    //    outcomes, since a rolled-back deploy's log is the most useful to keep.
    //    Best-effort: a transcript hiccup must never change the deploy result.
    //    Warn to the original sink so the warning isn't folded into the saved log.
    persist_transcript(host, &manifest.name, &id, &cap.transcript(), orig);

    install.context("remote install failed (the host rolled back to the previous binary)")
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

/// Ship an artifact to `remote` over rsync. A `File` is copied as-is; a `Dir`
/// gets trailing slashes (so rsync copies the *contents* into `remote`) plus
/// `--delete` to prune stale files. The remote already needs rsync for the dir
/// case, so files ride the same path and pick up `-z` compression for free.
///
/// `-a` preserves permissions and times (the binary keeps its exec bit), while
/// `--no-owner --no-group` stops the local machine's uid/gid from carrying over
/// — the files land owned by the remote SSH user, not whatever uid matches
/// locally. Each ship targets a fresh `.tug-new` path, so there is no basis file
/// for rsync to delta against; this is whole-file transfer, just compressed.
fn rsync(local: &Path, remote: &str, kind: ArtifactKind, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("rsync");
    command.args(["-az", "--no-owner", "--no-group"]);

    let (from, to) = match kind {
        ArtifactKind::File => (local.display().to_string(), remote.to_string()),
        ArtifactKind::Dir => {
            command.arg("--delete");
            (format!("{}/", local.display()), format!("{remote}/"))
        }
    };
    command.arg(&from).arg(&to);

    let status = run_streamed(command, None, log).context("spawning rsync")?;
    if !status.success() {
        bail!("rsync failed: {from} → {to}");
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

/// How many past deploy transcripts to keep per service on the host.
const TRANSCRIPT_KEEP: usize = 50;

/// Persist the deploy transcript to `/var/lib/tugboat/<name>/<id>.log` on the
/// host, alongside the ledger, then prune to the most recent [`TRANSCRIPT_KEEP`].
/// Best-effort: any failure is surfaced as a warning and otherwise ignored, so a
/// transcript hiccup can never change a deploy's outcome. The transcript arrives
/// on stdin (→ `tee`), so its contents never have to be shell-quoted.
fn persist_transcript(host: &str, name: &str, id: &str, content: &str, log: &dyn LogSink) {
    let dir = format!("/var/lib/tugboat/{name}");
    let file = format!("{dir}/{id}.log");
    let remote = format!(
        "sudo=\"\"; [ \"$(id -u)\" -eq 0 ] || sudo=\"sudo\"; \
         $sudo mkdir -p {dir} && $sudo tee {file} >/dev/null && $sudo chmod 0644 {file}; rc=$?; \
         {{ ls -1t {dir}/*.log 2>/dev/null | tail -n +{keep} | $sudo xargs -r rm -f; }} >/dev/null 2>&1 || true; \
         exit $rc",
        dir = shq(&dir),
        file = shq(&file),
        keep = TRANSCRIPT_KEEP + 1,
    );
    if let Err(err) = ssh_pipe_quiet(host, &remote, content.as_bytes()) {
        log.line(&format!("    warning: could not persist deploy transcript: {err}"));
    }
}

/// Run a remote command over ssh, feeding `stdin_data` to it and discarding its
/// stdout; returns an error (with captured stderr) on a non-zero exit. Unlike
/// [`ssh_script`], this stays silent on success — transcript persistence is
/// plumbing, not part of the transcript it ships.
fn ssh_pipe_quiet(host: &str, remote_cmd: &str, stdin_data: &[u8]) -> Result<()> {
    let mut child = Command::new("ssh")
        .arg(host)
        .arg(remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning ssh (transcript)")?;
    let mut stdin = child.stdin.take().context("ssh stdin unavailable")?;
    let mut stderr = child.stderr.take().context("ssh stderr unavailable")?;

    // Write stdin on its own thread while we drain stderr, so neither pipe can
    // fill and stall the other.
    let mut errbuf = String::new();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = stdin.write_all(stdin_data);
            // Dropping `stdin` here closes the pipe so the remote `tee` sees EOF.
        });
        let _ = stderr.read_to_string(&mut errbuf);
    });

    let status = child.wait().context("waiting on ssh (transcript)")?;
    if !status.success() {
        bail!("ssh exited {status}: {}", errbuf.trim());
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

/// The local repo state at deploy time, or `None` when the project isn't a git
/// checkout (nothing meaningful to record). `at` is stamped once at deploy start
/// and shared with the deploy id, so the ledger entry and its transcript file
/// agree on the timestamp.
fn build_stamp(project_dir: &Path, at: u64) -> Option<Stamp> {
    let state = git::state(project_dir);
    let sha = state.head_sha?;
    Some(Stamp {
        short: git::short(&sha).to_owned(),
        sha,
        dirty: state.dirty,
        branch: state.branch,
        deployed_at: at,
    })
}

/// The deploy id used to name the transcript file and link it from the ledger:
/// `{start-unix-seconds}-{short-sha}` (or `…-nogit` outside a checkout). Matches
/// the reader's `^[0-9]+-[0-9a-z]+$` validation.
fn deploy_id(at: u64, stamp: Option<&Stamp>) -> String {
    let short = stamp.map_or("nogit", |s| s.short.as_str());
    format!("{at}-{short}")
}

/// Current ledger schema version (see README "The deploy ledger"). v2 adds `id`,
/// which names the deploy's transcript file at `/var/lib/tugboat/<name>/<id>.log`.
const LEDGER_VERSION: u32 = 2;

/// One ledger line: the JSON record written for a deploy (see README "The deploy
/// ledger"). Kept separate from the bash wrapper so it can be unit-tested.
fn ledger_payload(stamp: &Stamp, id: &str, result: &str) -> String {
    json!({
        "v": LEDGER_VERSION,
        "id": id,
        "sha": stamp.sha,
        "short": stamp.short,
        "dirty": stamp.dirty,
        "branch": stamp.branch,
        "result": result,
        "at": stamp.deployed_at,
    })
    .to_string()
}

/// The bash that appends one entry to the host deploy ledger with the given
/// outcome. Empty when there's nothing to record (no git checkout). The append
/// is a single short line to an `O_APPEND` file, so concurrent or interrupted
/// writes can't tear an entry.
fn ledger_append(name: &str, stamp: Option<&Stamp>, id: &str, result: &str) -> String {
    let Some(stamp) = stamp else {
        return String::new();
    };
    let payload = ledger_payload(stamp, id, result);
    let path = format!("/var/lib/tugboat/{name}.jsonl");
    // `|| true` so a ledger write hiccup never fails an otherwise-successful
    // deploy (its backups are already gone) nor skips the rollback's `exit 1`.
    format!(
        "{{ $sudo mkdir -p /var/lib/tugboat \
         && printf '%s\\n' {payload} | $sudo tee -a {path} >/dev/null \
         && $sudo chmod 0644 {path}; }} || true",
        payload = shq(&payload),
        path = shq(&path),
    )
}

/// Build the remote transaction script. Uses a token-replacement template so
/// the bash (which is brace-heavy) stays readable.
fn remote_script(manifest: &Manifest, stamp: Option<&Stamp>, id: &str) -> String {
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
        .replace("@LEDGER_OK@", &ledger_append(name, stamp, id, "deployed"))
        .replace("@LEDGER_FAIL@", &ledger_append(name, stamp, id, "rolled_back"))
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
  @LEDGER_FAIL@
  exit 1
fi

for i in "${!DESTS[@]}"; do $sudo rm -rf "${DESTS[$i]}.tug-bak"; done
echo "    @NAME@ is active and healthy"
@LEDGER_OK@
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stamp() -> Stamp {
        Stamp {
            sha: "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b".into(),
            short: "1a2b3c4d".into(),
            dirty: false,
            branch: Some("main".into()),
            deployed_at: 1_718_900_000,
        }
    }

    /// The id names the transcript file and must satisfy the reader's
    /// `^[0-9]+-[0-9a-z]+$` validation. It also pairs the ledger `at` with the
    /// short sha so a reader can locate the log.
    #[test]
    fn deploy_id_format() {
        let stamp = sample_stamp();
        assert_eq!(deploy_id(1_718_900_000, Some(&stamp)), "1718900000-1a2b3c4d");
        assert_eq!(deploy_id(42, None), "42-nogit");

        let is_valid = |id: &str| {
            let (l, r) = id.split_once('-').expect("id has a dash");
            !l.is_empty()
                && l.bytes().all(|b| b.is_ascii_digit())
                && !r.is_empty()
                && r.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        };
        assert!(is_valid(&deploy_id(1_718_900_000, Some(&stamp))));
        assert!(is_valid(&deploy_id(42, None)));
    }

    /// The ledger line is v2 and carries the id linking it to the transcript.
    #[test]
    fn ledger_payload_is_v2_with_id() {
        let stamp = sample_stamp();
        let id = deploy_id(stamp.deployed_at, Some(&stamp));
        let line = ledger_payload(&stamp, &id, "deployed");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON line");
        assert_eq!(v["v"], 2);
        assert_eq!(v["id"], "1718900000-1a2b3c4d");
        assert_eq!(v["short"], "1a2b3c4d");
        assert_eq!(v["result"], "deployed");
        assert_eq!(v["at"], 1_718_900_000u64);
        assert_eq!(v["dirty"], false);
        assert_eq!(v["branch"], "main");
    }

    /// A rolled-back deploy records the same id with the failing outcome, so its
    /// transcript is still locatable.
    #[test]
    fn ledger_records_rollback_outcome() {
        let stamp = sample_stamp();
        let line = ledger_payload(&stamp, "1718900000-1a2b3c4d", "rolled_back");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["result"], "rolled_back");
        assert_eq!(v["id"], "1718900000-1a2b3c4d");
    }

    /// CapturingSink forwards to its inner sink and accumulates the transcript.
    #[test]
    fn capturing_sink_tees_and_accumulates() {
        struct Collect(Mutex<Vec<String>>);
        impl LogSink for Collect {
            fn line(&self, line: &str) {
                self.0.lock().unwrap().push(line.to_owned());
            }
        }
        let inner = Collect(Mutex::new(Vec::new()));
        let cap = CapturingSink::new(&inner);
        cap.line("first");
        cap.line("second");
        assert_eq!(cap.transcript(), "first\nsecond");
        assert_eq!(inner.0.lock().unwrap().as_slice(), ["first", "second"]);
    }
}
