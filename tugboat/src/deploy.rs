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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::events;
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
        .map_or(0, |d| d.as_secs())
}

/// A temp directory that is removed when this guard drops.
struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where a deploy gets the code it builds and ships.
pub enum Source {
    /// Origin's default branch, fetched fresh and built in a clean, detached
    /// worktree. Reproducible and independent of whatever is checked out in the
    /// shared working tree — the normal path, and the only one the daemon uses.
    DefaultBranch,
    /// The project's working tree exactly as it is on disk (its current branch,
    /// uncommitted changes included). An explicit opt-in for local iteration and
    /// branch smoke-tests; `skip_build` reuses the tree's existing artifacts.
    WorkingTree { skip_build: bool },
}

/// A git worktree removed when this guard drops, so a deploy's throwaway checkout
/// never outlives the deploy.
struct WorktreeGuard {
    repo: PathBuf,
    path: PathBuf,
}
impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        git::remove_worktree(&self.repo, &self.path);
    }
}

/// The clean checkout a default-branch deploy builds from.
struct Prepared {
    /// The directory to build in: the service's directory inside the worktree
    /// (the worktree root itself when the service is its own repository).
    build_dir: PathBuf,
    /// The worktree root — the repository checkout containing `build_dir`, and
    /// what `{workspace}` expands to. For fleet monorepo members this is the
    /// cargo workspace root, where `target/` lives.
    checkout_root: PathBuf,
    /// The ledger stamp for the checked-out default-branch commit.
    stamp: Stamp,
    /// Removes the worktree when dropped; held for the deploy's lifetime.
    guard: WorktreeGuard,
}

/// Serializes `git fetch` across concurrent deploys. Monorepo members share one
/// repository, and concurrent fetches of the same remote can contend on git's
/// ref locks; a fetch is brief next to a build, so taking them one at a time
/// costs nothing.
static FETCH_LOCK: Mutex<()> = Mutex::new(());

/// Fetch origin and check its default branch out into a fresh detached worktree,
/// so the deploy builds exactly what's on the canonical branch — not whatever the
/// shared checkout is parked on (which the drydock worker, or a stray `git
/// checkout`, can leave on a feature branch). The worktree checks out the whole
/// repository; the returned `build_dir` is the service's directory within it, so
/// a monorepo member builds inside a full workspace checkout. The worktree is
/// removed when the returned guard drops.
fn prepare_default_branch(project_dir: &Path, at: u64, log: &dyn LogSink) -> Result<Prepared> {
    if !git::state(project_dir).is_repo {
        bail!(
            "cannot deploy the default branch: {} is not a git checkout \
             (use `--working-tree` to deploy a non-git directory as-is)",
            project_dir.display()
        );
    }
    // The service's path inside its repository — empty for a standalone repo,
    // e.g. `drydock` for a fleet monorepo member. Canonicalize both sides so
    // symlinked paths (macOS /tmp, ~ aliases) can't break the prefix match.
    let toplevel = git::toplevel(project_dir)?
        .canonicalize()
        .context("resolving the repository root")?;
    let rel = project_dir
        .canonicalize()
        .context("resolving the service directory")?
        .strip_prefix(&toplevel)
        .map(Path::to_path_buf)
        .with_context(|| {
            format!(
                "{} is not under its repository root {}",
                project_dir.display(),
                toplevel.display()
            )
        })?;

    let branch = git::default_branch(project_dir).context("resolving origin's default branch")?;
    step(log, "FETCH", &format!("origin ({branch})"));
    {
        let _serialized = FETCH_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        git::fetch(project_dir)?;
    }

    let target = format!("origin/{branch}");
    let sha = git::rev_parse(project_dir, &target)?
        .with_context(|| format!("{target} not found after fetch"))?;

    // Per-service path (the dir's final component) so concurrent deploys of *different*
    // services in one daemon don't collide; same-service deploys are serialized.
    let name = project_dir.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let path = std::env::temp_dir().join(format!("tugboat-src-{name}-{}", std::process::id()));
    // Clear any worktree a previous interrupted deploy may have left at this path.
    git::remove_worktree(&toplevel, &path);
    git::add_worktree(&toplevel, &path, &sha)?;
    let guard = WorktreeGuard {
        repo: toplevel,
        path: path.clone(),
    };
    step(log, "CHECKOUT", &format!("{target} @ {}", git::short(&sha)));

    let stamp = Stamp {
        short: git::short(&sha).to_owned(),
        sha,
        dirty: false,
        branch: Some(branch),
        deployed_at: at,
    };
    Ok(Prepared {
        build_dir: path.join(&rel),
        checkout_root: path,
        stamp,
        guard,
    })
}

pub fn run(
    manifest: &Manifest,
    project_dir: &Path,
    source: Source,
    dry_run: bool,
    log: &dyn LogSink,
) -> Result<()> {
    let workdir = std::env::temp_dir()
        .join(format!("tugboat-{}-{}", manifest.name, std::process::id()));
    let workdir_str = workdir.to_string_lossy().into_owned();

    if dry_run {
        // For display, expand {workspace} against the on-disk checkout; a real
        // default-branch deploy expands it to the corresponding path inside its
        // throwaway worktree.
        let checkout = git::toplevel(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
        print_plan(manifest, project_dir, &source, &workdir_str, &checkout, log);
        return Ok(());
    }

    // Stamp the deploy at its start so the id (transcript filename), the ledger
    // `at`, the deploy event, and the on-host transcript all agree on one
    // timestamp — that shared value is what joins them.
    let at = now_unix();
    let mut rec = events::Recorder::new(
        at,
        &manifest.name,
        manifest.host(),
        match source {
            Source::WorkingTree { .. } => "working_tree",
            Source::DefaultBranch => "default_branch",
        },
    );

    // Emit the event on every exit path, success or failure — the failures that
    // never reach the host (a build that didn't compile, a missing artifact) are
    // exactly the ones the host ledger can't see. Warned about, never fatal.
    let outcome = run_measured(manifest, project_dir, source, log, at, &workdir, &workdir_str, &mut rec);
    events::record(&rec.finish(&outcome), log);
    outcome
}

/// The deploy proper. Split from [`run`] so that every `?` here still produces a
/// deploy event.
#[allow(clippy::too_many_arguments)]
fn run_measured(
    manifest: &Manifest,
    project_dir: &Path,
    source: Source,
    log: &dyn LogSink,
    at: u64,
    workdir: &Path,
    workdir_str: &str,
    rec: &mut events::Recorder,
) -> Result<()> {
    let host = manifest.host();

    // Tee the live sink through a capturing one so we can persist the full
    // transcript below — set up first so the source-prep steps (fetch/checkout)
    // are captured too.
    let cap = CapturingSink::new(log);
    let orig = log;
    let log: &dyn LogSink = &cap;

    // Resolve where to build from. The default-branch path fetches origin and
    // checks it out into a throwaway worktree, so the build is reproducible and
    // can't be perturbed by whatever branch the shared checkout is parked on. The
    // worktree guard lives to the end of this function, removing it after the ship.
    let (build_dir, checkout_root, stamp, skip_build, _worktree) = match &source {
        Source::WorkingTree { skip_build } => {
            // {workspace} in a working-tree deploy is the on-disk repository
            // root (the directory itself for a non-git deploy).
            let root = git::toplevel(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
            (project_dir.to_path_buf(), root, build_stamp(project_dir, at), *skip_build, None)
        }
        Source::DefaultBranch => {
            let p = prepare_default_branch(project_dir, at, log)?;
            (p.build_dir, p.checkout_root, Some(p.stamp), false, Some(p.guard))
        }
    };
    let checkout_str = checkout_root.to_string_lossy().into_owned();
    let build_cmd = subst(&manifest.build.cmd, workdir_str, &checkout_str);
    let id = deploy_id(at, stamp.as_ref());

    // Which commit this deploy resolved to — known only after source prep.
    if let Some(stamp) = &stamp {
        rec.stamped(&stamp.sha, &stamp.short, stamp.branch.as_deref(), stamp.dirty);
    }

    // Resolve artifact sources (relative paths are relative to the build dir; an
    // absolute path, e.g. one under {workdir} or {workspace}, is used as-is).
    let artifacts: Vec<(PathBuf, &crate::manifest::Artifact)> = manifest
        .artifacts
        .iter()
        .map(|a| (build_dir.join(subst(&a.src, workdir_str, &checkout_str)), a))
        .collect();

    std::fs::create_dir_all(workdir)
        .with_context(|| format!("creating work dir {}", workdir.display()))?;
    let _guard = WorkDir(workdir.to_path_buf());

    // 1. Build.
    rec.entering(events::Stage::Build);
    if skip_build {
        note(log, "skipping build (--skip-build)");
    } else {
        step(log, "BUILD", &build_cmd);
        let build_env = match musl_toolchain(&build_cmd)? {
            Some(tool) => {
                note(log, &format!("musl C toolchain: {}", tool.cc));
                musl_env(tool)
            }
            None => Vec::new(),
        };
        let t = Instant::now();
        run_local(&build_cmd, &build_dir, &build_env, log).context("build failed")?;
        rec.completed(events::Stage::Build, t);
    }

    // 2. Confirm every artifact exists locally, of the right kind, before
    //    touching the host.
    rec.entering(events::Stage::Artifacts);
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
    rec.entering(events::Stage::Ship);
    let t = Instant::now();
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
    rec.completed(events::Stage::Ship, t);

    // 4. Atomic install, restart, health-check, rollback-on-failure, enroll,
    //    and record what was deployed — all in one remote transaction.
    step(
        log,
        "INSTALL",
        &format!("{host}: swap binary, restart {}, health-check", manifest.name),
    );
    rec.entering(events::Stage::Install);
    let t = Instant::now();
    let install = ssh_script(host, &remote_script(manifest, stamp.as_ref(), &id), log);
    rec.completed(events::Stage::Install, t);

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

/// Expand manifest placeholders: `{workdir}` (the deploy's fresh temp dir) and
/// `{workspace}` (the repository checkout root being built — the cargo
/// workspace root for fleet members, where the shared `target/` lives).
fn subst(input: &str, workdir: &str, workspace: &str) -> String {
    input
        .replace("{workdir}", workdir)
        .replace("{workspace}", workspace)
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

fn run_local(
    cmd: &str,
    dir: &Path,
    env: &[(&'static str, String)],
    log: &dyn LogSink,
) -> Result<()> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd).current_dir(dir);
    command.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
    let status = run_streamed(command, None, log).with_context(|| format!("spawning: {cmd}"))?;
    if !status.success() {
        bail!("command exited with {status}: {cmd}");
    }
    Ok(())
}

/// The cross-target every VPS-bound fleet build compiles for.
const MUSL_TARGET: &str = "x86_64-unknown-linux-musl";

/// A C toolchain that can build for [`MUSL_TARGET`]: the compiler (which is
/// also the linker driver) and the matching archiver for cc-rs-built C deps.
struct MuslToolchain {
    cc: &'static str,
    ar: &'static str,
}

/// Known toolchains in preference order: the dedicated cross tool (macOS, via
/// musl-cross, which ships its own binutils), then the distro's wrapper for
/// the native arch (Fedora's `musl-gcc` package — same-arch objects, so the
/// system `ar` is the right archiver). Which one a machine carries is a
/// property of the machine, not of any service — so manifests never name a
/// toolchain; the engine resolves one here and exports it to the build.
const MUSL_TOOLCHAINS: [MuslToolchain; 2] = [
    MuslToolchain { cc: "x86_64-linux-musl-gcc", ar: "x86_64-linux-musl-ar" },
    MuslToolchain { cc: "musl-gcc", ar: "ar" },
];

/// Resolve the musl C toolchain for a build command that targets
/// [`MUSL_TARGET`]; `Ok(None)` when the build doesn't. A musl build on a
/// machine with no toolchain fails here, at the top of BUILD with install
/// hints — not minutes into the compile inside a cc-rs build script.
fn musl_toolchain(build_cmd: &str) -> Result<Option<&'static MuslToolchain>> {
    if !build_cmd.contains(MUSL_TARGET) {
        return Ok(None);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    match first_toolchain_in(&dirs) {
        Some(tool) => Ok(Some(tool)),
        None => bail!(
            "build targets {MUSL_TARGET} but no musl C toolchain is on PATH \
             (looked for `x86_64-linux-musl-gcc` or `musl-gcc`); install one — \
             macOS: `brew install filosottile/musl-cross/musl-cross`, Fedora: \
             `sudo dnf install musl-gcc musl-devel musl-libc-static`"
        ),
    }
}

fn first_toolchain_in(dirs: &[PathBuf]) -> Option<&'static MuslToolchain> {
    MUSL_TOOLCHAINS
        .iter()
        .find(|tool| dirs.iter().any(|dir| is_executable(&dir.join(tool.cc))))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Env that points cargo (the linker) and cc-rs (the C compiler and archiver
/// for build scripts) at `tool` for [`MUSL_TARGET`]. Cargo gives environment
/// variables precedence over `.cargo/config.toml`, so this also corrects a
/// repo whose checked-in config names a toolchain this machine doesn't have.
fn musl_env(tool: &MuslToolchain) -> Vec<(&'static str, String)> {
    vec![
        ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER", tool.cc.to_string()),
        ("CC_x86_64_unknown_linux_musl", tool.cc.to_string()),
        ("AR_x86_64_unknown_linux_musl", tool.ar.to_string()),
    ]
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
pub(crate) fn rsync(local: &Path, remote: &str, kind: ArtifactKind, log: &dyn LogSink) -> Result<()> {
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

pub(crate) fn ssh_script(host: &str, script: &str, log: &dyn LogSink) -> Result<()> {
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
pub(crate) fn shq(s: &str) -> String {
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

/// The deploy id naming the transcript file, in the shared contract's format
/// (see the `tugboat-ledger` crate, which readers validate against).
fn deploy_id(at: u64, stamp: Option<&Stamp>) -> String {
    tugboat_ledger::deploy_id(at, stamp.map(|s| s.short.as_str()))
}

/// One ledger line: the JSON record written for a deploy, through the shared
/// `tugboat-ledger` contract type — so the writer and lighthouse's reader can
/// only change together. Kept separate from the bash wrapper for unit tests.
fn ledger_payload(stamp: &Stamp, id: &str, result: &str) -> String {
    serde_json::to_string(&tugboat_ledger::LedgerEntry {
        v: tugboat_ledger::LEDGER_VERSION,
        id: Some(id.to_owned()),
        sha: stamp.sha.clone(),
        short: stamp.short.clone(),
        dirty: stamp.dirty,
        branch: stamp.branch.clone(),
        result: result.to_owned(),
        at: stamp.deployed_at,
    })
    .expect("a ledger entry always serializes")
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
    project_dir: &Path,
    source: &Source,
    workdir_str: &str,
    checkout: &Path,
    log: &dyn LogSink,
) {
    let checkout_str = checkout.to_string_lossy();
    let build_cmd = subst(&manifest.build.cmd, workdir_str, &checkout_str);
    log.line(&format!("DRY RUN — plan for {} → {}\n", manifest.name, manifest.host()));
    log.line("  source:");
    match source {
        Source::DefaultBranch => log.line(
            "    origin's default branch (fetched fresh, built in a clean detached worktree)",
        ),
        Source::WorkingTree { skip_build } => log.line(&format!(
            "    working tree at {} ({})",
            project_dir.display(),
            if *skip_build { "reusing existing artifacts" } else { "rebuilt in place" }
        )),
    }
    log.line("  build:");
    log.line(&format!("    {build_cmd}"));
    log.line("  ship:");
    // Artifact sources are shown against the on-disk checkout; a default-branch
    // deploy builds the same relative paths inside its worktree.
    for artifact in &manifest.artifacts {
        let src = project_dir.join(subst(&artifact.src, workdir_str, &checkout_str));
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

    /// A build that doesn't target musl needs no toolchain — and must not fail
    /// on machines that have none (e.g. a Docker-artifact deploy).
    #[test]
    fn non_musl_build_needs_no_toolchain() {
        let cmd = "docker build -t readout . && docker save readout";
        assert!(matches!(musl_toolchain(cmd), Ok(None)));
    }

    /// Toolchain discovery prefers the dedicated cross tool, falls back to the
    /// distro wrapper, and ignores non-executable files — exercised against a
    /// scratch dir rather than the real PATH so the test is hermetic.
    #[test]
    fn toolchain_discovery_prefers_cross_tool() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tugboat-musl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let executable = |name: &str| {
            let p = dir.join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };

        let dirs = vec![dir.clone()];
        assert!(first_toolchain_in(&dirs).is_none());

        // A plain file is not a toolchain.
        std::fs::write(dir.join("musl-gcc"), "").unwrap();
        std::fs::set_permissions(
            dir.join("musl-gcc"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(first_toolchain_in(&dirs).is_none());

        executable("musl-gcc");
        assert_eq!(first_toolchain_in(&dirs).map(|t| t.cc), Some("musl-gcc"));

        executable("x86_64-linux-musl-gcc");
        assert_eq!(
            first_toolchain_in(&dirs).map(|t| t.cc),
            Some("x86_64-linux-musl-gcc")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The exported env covers every half of a musl cross-build: cargo's
    /// linker plus cc-rs's C compiler and archiver for build scripts. The
    /// distro wrapper pairs with the system `ar` — its objects are native-arch.
    #[test]
    fn musl_env_sets_linker_cc_and_ar() {
        let tool = &MUSL_TOOLCHAINS[1];
        assert_eq!(tool.cc, "musl-gcc");
        let env = musl_env(tool);
        assert!(env
            .iter()
            .any(|(k, v)| *k == "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER" && v == "musl-gcc"));
        assert!(env
            .iter()
            .any(|(k, v)| *k == "CC_x86_64_unknown_linux_musl" && v == "musl-gcc"));
        assert!(env
            .iter()
            .any(|(k, v)| *k == "AR_x86_64_unknown_linux_musl" && v == "ar"));
    }

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

    /// Placeholders: {workdir} is the deploy temp dir, {workspace} the checkout
    /// root — the paths a monorepo member's artifacts are anchored to.
    #[test]
    fn subst_expands_workdir_and_workspace() {
        assert_eq!(
            subst("{workspace}/target/release/svc", "/wd", "/checkout"),
            "/checkout/target/release/svc"
        );
        assert_eq!(subst("{workdir}/bundle.tar", "/wd", "/checkout"), "/wd/bundle.tar");
        assert_eq!(subst("web/dist", "/wd", "/checkout"), "web/dist");
    }

    /// Run git with a deterministic identity (mirrors git.rs's test helper).
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=Test", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A default-branch deploy of a monorepo member checks out the whole
    /// repository and builds in the member's directory within it: `build_dir`
    /// is `<checkout_root>/<member>`, with the member's files present. A deploy
    /// of the repository root keeps the two equal (the standalone-repo case).
    #[test]
    fn prepares_monorepo_member_inside_full_checkout() {
        let base = std::env::temp_dir().join(format!("tugboat-mono-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // A bare origin with one commit containing svc/hello.txt, and a clone —
        // the shape of the dev box's fleet checkout.
        let origin = base.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let clone = base.join("clone");
        git(&base, &["clone", "-q", "origin.git", "clone"]);
        std::fs::create_dir_all(clone.join("svc")).unwrap();
        std::fs::write(clone.join("svc/hello.txt"), "hi").unwrap();
        git(&clone, &["add", "."]);
        git(&clone, &["commit", "-q", "-m", "init"]);
        git(&clone, &["push", "-q", "origin", "main"]);

        struct Quiet;
        impl LogSink for Quiet {
            fn line(&self, _: &str) {}
        }

        // Member deploy: builds in <worktree>/svc of a full checkout.
        let prepared = prepare_default_branch(&clone.join("svc"), 42, &Quiet).unwrap();
        assert_eq!(prepared.build_dir, prepared.checkout_root.join("svc"));
        assert!(prepared.build_dir.join("hello.txt").is_file());
        assert!(prepared.checkout_root.join(".git").exists(), "worktree root is the checkout");
        assert_eq!(prepared.stamp.branch.as_deref(), Some("main"));
        let worktree = prepared.checkout_root.clone();
        drop(prepared);
        assert!(!worktree.exists(), "worktree removed when the guard drops");

        // Repo-root deploy: the standalone case, build_dir == checkout_root.
        let prepared = prepare_default_branch(&clone, 42, &Quiet).unwrap();
        assert_eq!(prepared.build_dir, prepared.checkout_root);
        assert!(prepared.build_dir.join("svc/hello.txt").is_file());
        drop(prepared);

        let _ = std::fs::remove_dir_all(&base);
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
