//! The deploy engine: plan → build → prepare → activate → health-check →
//! compensate-on-failure → cleanup → (optional) enroll in lighthouse.target →
//! end-to-end verify.
//!
//! All human-facing progress goes through a [`LogSink`] rather than straight to
//! stdout, so the same pipeline drives both the `tugboat deploy` CLI (which
//! prints to the terminal) and `tugboat serve` (which streams the transcript to
//! a browser). Subprocess stdout/stderr is captured and forwarded line-by-line
//! into the sink as it arrives, so the log stays live even when no terminal is
//! attached.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use crate::events;
use crate::git;
use crate::manifest::{self, ArtifactKind, BuildRequirement, Health, Manifest, Verify};
use crate::subprocess::{run_streamed, LogSink};
use crate::transport::{self, shell_quote as shq};

mod remote;

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

/// Wraps another sink and also accumulates every line, so the engine can persist
/// the full transcript to the host after the deploy finishes — regardless of
/// whether the live sink is the CLI's stdout or the daemon's SSE channel.
struct CapturingSink<'a> {
    inner: &'a dyn LogSink,
    capture: Mutex<CaptureState>,
}

struct CaptureState {
    file: File,
    error: Option<String>,
}

impl<'a> CapturingSink<'a> {
    fn new(inner: &'a dyn LogSink) -> Result<Self> {
        Ok(Self {
            inner,
            capture: Mutex::new(CaptureState {
                file: tempfile::tempfile().context("creating deploy transcript spool")?,
                error: None,
            }),
        })
    }

    fn into_reader(self) -> Result<File> {
        let mut capture = self
            .capture
            .into_inner()
            .map_err(|_| anyhow::anyhow!("deploy transcript lock was poisoned"))?;
        if let Some(error) = capture.error {
            bail!("writing deploy transcript spool: {error}");
        }
        capture
            .file
            .flush()
            .context("flushing deploy transcript spool")?;
        capture
            .file
            .seek(SeekFrom::Start(0))
            .context("rewinding deploy transcript spool")?;
        Ok(capture.file)
    }
}

impl LogSink for CapturingSink<'_> {
    fn line(&self, line: &str) {
        let Ok(mut capture) = self.capture.lock() else {
            self.inner.line(line);
            return;
        };
        self.inner.line(line);
        if capture.error.is_none() {
            if let Err(error) = writeln!(capture.file, "{line}") {
                capture.error = Some(error.to_string());
            }
        }
    }
}

/// Run one fallible attempt through a capturing sink and invoke `finalize`
/// before returning its result. Keeping finalization in this control-flow
/// primitive makes an early `?` inside source preparation, build, shipping, or
/// activation unable to bypass transcript persistence.
fn with_transcript<T>(
    live_log: &dyn LogSink,
    attempt: impl FnOnce(&dyn LogSink) -> Result<T>,
    finalize: impl FnOnce(Result<File>),
) -> Result<T> {
    let capture = CapturingSink::new(live_log)?;
    let outcome = attempt(&capture);
    finalize(capture.into_reader());
    outcome
}

/// Seconds since the Unix epoch (0 if the clock is before it, which never
/// happens in practice).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where a deploy gets the code it builds and ships.
#[derive(Clone, Copy)]
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

/// Read-only local information needed to describe or prepare a default-branch
/// checkout.
struct DefaultBranchLayout {
    repo: PathBuf,
    relative_service: PathBuf,
    checkout_root: PathBuf,
}

/// One artifact after its local source has been resolved for this deployment.
struct PlannedArtifact<'a> {
    src: PathBuf,
    manifest: &'a crate::manifest::Artifact,
}

impl PlannedArtifact<'_> {
    fn staged(&self, plan: &DeploymentPlan<'_>) -> String {
        format!("{}.tug-new-{}", self.manifest.dest, plan.id)
    }

    fn backup(&self, plan: &DeploymentPlan<'_>) -> String {
        format!("{}.tug-backup-{}", self.manifest.dest, plan.id)
    }
}

/// The single resolved description consumed by both dry-run rendering and
/// execution. Source paths, placeholders, artifacts, and the deploy identity
/// are decided once here instead of being independently reconstructed by each
/// path.
struct DeploymentPlan<'a> {
    manifest: &'a Manifest,
    source: Source,
    build_dir: PathBuf,
    build_cmd: String,
    artifacts: Vec<PlannedArtifact<'a>>,
    skip_build: bool,
    stamp: Option<Stamp>,
    id: String,
    /// Keeps a default-branch worktree alive until execution has finished.
    _worktree: Option<WorktreeGuard>,
}

struct PlannedSource {
    build_dir: PathBuf,
    checkout_root: PathBuf,
    stamp: Option<Stamp>,
    skip_build: bool,
    worktree: Option<WorktreeGuard>,
}

impl<'a> DeploymentPlan<'a> {
    fn working_tree(
        manifest: &'a Manifest,
        project_dir: &Path,
        skip_build: bool,
        at: u64,
        workdir: PathBuf,
        nonce: &str,
    ) -> Self {
        let resolved = PlannedSource {
            build_dir: project_dir.to_path_buf(),
            checkout_root: working_tree_workspace(project_dir),
            stamp: build_stamp(project_dir, at),
            skip_build,
            worktree: None,
        };
        Self::resolve(
            manifest,
            Source::WorkingTree { skip_build },
            at,
            workdir,
            nonce,
            resolved,
        )
    }

    fn default_branch(
        manifest: &'a Manifest,
        prepared: Prepared,
        at: u64,
        workdir: PathBuf,
        nonce: &str,
    ) -> Self {
        let resolved = PlannedSource {
            build_dir: prepared.build_dir,
            checkout_root: prepared.checkout_root,
            stamp: Some(prepared.stamp),
            skip_build: false,
            worktree: Some(prepared.guard),
        };
        Self::resolve(
            manifest,
            Source::DefaultBranch,
            at,
            workdir,
            nonce,
            resolved,
        )
    }

    /// Resolve the plan without fetching, changing refs, or creating a
    /// worktree. Default-branch paths use the same layout a real deployment
    /// will prepare, but source availability is checked only during execution.
    fn preview(
        manifest: &'a Manifest,
        project_dir: &Path,
        source: Source,
        at: u64,
        workdir: PathBuf,
        nonce: &str,
    ) -> Result<Self> {
        let resolved = match &source {
            Source::WorkingTree { skip_build } => PlannedSource {
                build_dir: project_dir.to_path_buf(),
                checkout_root: working_tree_workspace(project_dir),
                stamp: build_stamp(project_dir, at),
                skip_build: *skip_build,
                worktree: None,
            },
            Source::DefaultBranch => {
                let layout = default_branch_layout(project_dir)?;
                PlannedSource {
                    build_dir: layout.checkout_root.join(layout.relative_service),
                    checkout_root: layout.checkout_root,
                    stamp: None,
                    skip_build: false,
                    worktree: None,
                }
            }
        };
        Ok(Self::resolve(
            manifest, source, at, workdir, nonce, resolved,
        ))
    }

    fn resolve(
        manifest: &'a Manifest,
        source: Source,
        at: u64,
        workdir: PathBuf,
        nonce: &str,
        resolved: PlannedSource,
    ) -> Self {
        let workdir_string = workdir.to_string_lossy().into_owned();
        let checkout_string = resolved.checkout_root.to_string_lossy().into_owned();
        let build_cmd = subst(&manifest.build.cmd, &workdir_string, &checkout_string);
        let artifacts = manifest
            .artifacts
            .iter()
            .map(|artifact| PlannedArtifact {
                src: resolved.build_dir.join(subst(
                    &artifact.src,
                    &workdir_string,
                    &checkout_string,
                )),
                manifest: artifact,
            })
            .collect();
        let id = deploy_id(at, resolved.stamp.as_ref(), nonce);
        Self {
            manifest,
            source,
            build_dir: resolved.build_dir,
            build_cmd,
            artifacts,
            skip_build: resolved.skip_build,
            stamp: resolved.stamp,
            id,
            _worktree: resolved.worktree,
        }
    }

    fn print(&self, project_dir: &Path, log: &dyn LogSink) {
        log.line(&format!(
            "DRY RUN — plan for {} → {}\n",
            self.manifest.name,
            self.manifest.host()
        ));
        log.line("  source:");
        match self.source {
            Source::DefaultBranch => log.line(&format!(
                "    origin's default branch at {}",
                self.build_dir.display()
            )),
            Source::WorkingTree { skip_build } => log.line(&format!(
                "    working tree at {} ({})",
                project_dir.display(),
                if skip_build {
                    "reusing existing artifacts"
                } else {
                    "rebuilt in place"
                }
            )),
        }
        log.line("  build:");
        log.line(&format!("    {}", self.build_cmd));
        for requirement in &self.manifest.build.requirements {
            log.line(&format!("    requires: {requirement}"));
        }
        log.line("  ship:");
        for artifact in &self.artifacts {
            match artifact.manifest.kind {
                ArtifactKind::File => log.line(&format!(
                    "    {} → {} (file, mode {})",
                    artifact.src.display(),
                    artifact.manifest.dest,
                    artifact.manifest.mode
                )),
                ArtifactKind::Dir => log.line(&format!(
                    "    {}/ → {} (dir, rsync --delete)",
                    artifact.src.display(),
                    artifact.manifest.dest
                )),
            }
        }
        let health = match &self.manifest.health {
            Some(Health { url: Some(url), .. }) => format!("curl {url} (on host loopback)"),
            _ => format!("systemctl is-active {}", self.manifest.name),
        };
        log.line(&format!(
            "  transaction: prepare → activate → {health} → cleanup"
        ));
        log.line(&format!(
            "  compensate: restore every previous artifact and restart {} after any activation or health failure",
            self.manifest.name
        ));
        log.line(&format!(
            "  enroll:  {}",
            if self.manifest.lighthouse.enroll {
                "systemctl add-wants lighthouse.target"
            } else {
                "(none)"
            }
        ));
        if let Some(verify) = &self.manifest.verify {
            log.line(&format!("  verify:  {} (from here)", verify.url));
        }
    }
}

/// What `{workspace}` expands to for a working-tree deploy.
///
/// **The cargo workspace root first**, because that is where `target/` lives —
/// and `{workspace}/target/...` is what almost every Rust manifest ships from.
///
/// Resolving this from the *git* toplevel instead breaks every working-tree
/// deploy run from a **jj workspace**, which is the isolation the fleet's own
/// workflow mandates: jj workspaces share one colocated git repo, so
/// `git rev-parse --show-toplevel` answers with the main checkout while cargo
/// builds into `.workspaces/<slug>/target/`. The build then succeeds and the
/// ship fails, looking for an artifact in a directory nothing wrote to.
///
/// Non-Rust services (container builds, static directories) have no cargo
/// workspace, and fall back to the git toplevel exactly as before.
fn working_tree_workspace(project_dir: &Path) -> PathBuf {
    cargo_workspace_root(project_dir)
        .or_else(|| git::toplevel(project_dir).ok())
        .unwrap_or_else(|| project_dir.to_path_buf())
}

/// The nearest ancestor holding a `Cargo.toml` with a `[workspace]` table.
///
/// Walks up rather than shelling out to `cargo locate-project`: this runs
/// before any build, on machines where the manifest may not even be a Rust
/// project, and a missing or slow cargo should not decide where artifacts are
/// looked for.
fn cargo_workspace_root(from: &Path) -> Option<PathBuf> {
    for dir in from.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        // Parsed, not string-matched: `[workspace]` also appears inside
        // strings and comments, and `workspace = true` on a dependency is a
        // different thing entirely.
        let Ok(parsed) = text.parse::<toml::Table>() else {
            continue;
        };
        if parsed.contains_key("workspace") {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Serializes `git fetch` across concurrent deploys. Monorepo members share one
/// repository, and concurrent fetches of the same remote can contend on git's
/// ref locks; a fetch is brief next to a build, so taking them one at a time
/// costs nothing.
static FETCH_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the repository-relative service path and deterministic temporary
/// checkout path without changing either the repository or filesystem.
fn default_branch_layout(project_dir: &Path) -> Result<DefaultBranchLayout> {
    if !git::is_work_tree(project_dir) {
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

    // Per-service path (the dir's final component) so concurrent deploys of
    // different services in one daemon don't collide; same-service deploys are
    // serialized.
    let name = project_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let checkout_root =
        std::env::temp_dir().join(format!("tugboat-src-{name}-{}", std::process::id()));
    Ok(DefaultBranchLayout {
        repo: toplevel,
        relative_service: rel,
        checkout_root,
    })
}

/// Fetch origin and check its default branch out into a fresh detached worktree,
/// so the deploy builds exactly what's on the canonical branch. The worktree
/// checks out the whole repository; the returned `build_dir` is the service's
/// directory within it and the guard removes it after execution.
fn prepare_default_branch(project_dir: &Path, at: u64, log: &dyn LogSink) -> Result<Prepared> {
    let layout = default_branch_layout(project_dir)?;
    let branch = git::default_branch(project_dir).context("resolving origin's default branch")?;
    step(log, "FETCH", &format!("origin ({branch})"));
    {
        let _serialized = FETCH_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        git::fetch(project_dir)?;
    }

    let target = format!("origin/{branch}");
    let sha = git::rev_parse(project_dir, &target)?
        .with_context(|| format!("{target} not found after fetch"))?;

    // Clear any worktree a previous interrupted deploy may have left at this path.
    git::remove_worktree(&layout.repo, &layout.checkout_root);
    git::add_worktree(&layout.repo, &layout.checkout_root, &sha)?;
    let guard = WorktreeGuard {
        repo: layout.repo,
        path: layout.checkout_root.clone(),
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
        build_dir: layout.checkout_root.join(layout.relative_service),
        checkout_root: layout.checkout_root,
        stamp,
        guard,
    })
}

fn load_default_branch_manifest(
    manifest_path: &Path,
    prepared: &Prepared,
    overrides: &manifest::RuntimeOverrides,
) -> Result<Manifest> {
    let source_manifest_path = prepared.build_dir.join(
        manifest_path
            .file_name()
            .context("manifest path has no filename")?,
    );
    manifest::load_with_overrides(&source_manifest_path, overrides)
}

fn preview_default_branch_manifest(
    manifest_path: &Path,
    project_dir: &Path,
    overrides: &manifest::RuntimeOverrides,
) -> Result<Manifest> {
    let layout = default_branch_layout(project_dir)?;
    let branch = git::default_branch_local(project_dir)?.with_context(|| {
        format!(
            "cannot preview origin's default branch for {}: no local origin/HEAD, origin/main, or origin/master ref",
            project_dir.display()
        )
    })?;
    let target = format!("origin/{branch}");
    let relative_manifest = layout.relative_service.join(
        manifest_path
            .file_name()
            .context("manifest path has no filename")?,
    );
    let text = git::show_file(&layout.repo, &target, &relative_manifest)?
        .with_context(|| format!("{target} does not contain {}", relative_manifest.display()))?;
    manifest::load_text_with_overrides(
        &text,
        &format!("{target}:{}", relative_manifest.display()),
        overrides,
    )
}

pub fn run(
    manifest_path: &Path,
    project_dir: &Path,
    source: Source,
    dry_run: bool,
    host_override: Option<&str>,
    log: &dyn LogSink,
) -> Result<()> {
    let overrides = manifest::runtime_overrides(manifest_path, host_override)?;

    if dry_run {
        let preview_manifest = match source {
            Source::WorkingTree { .. } => manifest::load_with_overrides(manifest_path, &overrides)?,
            Source::DefaultBranch => {
                preview_default_branch_manifest(manifest_path, project_dir, &overrides)?
            }
        };
        let workdir =
            std::env::temp_dir().join(format!("tugboat-{}-<workdir>", preview_manifest.name));
        let plan = DeploymentPlan::preview(
            &preview_manifest,
            project_dir,
            source,
            now_unix(),
            workdir,
            "preview",
        )?;
        plan.print(project_dir, log);
        return Ok(());
    }

    let request_manifest = match source {
        Source::WorkingTree { .. } => {
            Some(manifest::load_with_overrides(manifest_path, &overrides)?)
        }
        Source::DefaultBranch => None,
    };
    let request_name = request_manifest
        .as_ref()
        .map(|manifest| manifest.name.clone())
        .or_else(|| {
            project_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .context("project directory has no service name")?;
    let request_host = request_manifest
        .as_ref()
        .map(|manifest| manifest.host())
        .or_else(|| overrides.host());

    // Stamp the deploy at its start so the id (transcript filename), the ledger
    // `at`, the deploy event, and the on-host transcript all agree on one
    // timestamp — that shared value is what joins them.
    let at = now_unix();
    let mut recorder = events::Recorder::new(
        at,
        &request_name,
        request_host,
        match source {
            Source::WorkingTree { .. } => "working_tree",
            Source::DefaultBranch => "default_branch",
        },
    );

    let nonce = Uuid::new_v4().simple().to_string();
    let transcript_id = RefCell::new(deploy_id(at, None, &nonce));
    let transcript_target =
        RefCell::new(request_host.map(|host| (host.to_owned(), request_name.clone())));
    let outcome = with_transcript(
        log,
        |captured| {
            let workdir = deploy_workdir(&request_name)?;
            let workdir_path = workdir.path().to_path_buf();
            match source {
                Source::WorkingTree { skip_build } => {
                    let request_manifest = request_manifest
                        .as_ref()
                        .expect("working-tree manifest loaded above");
                    let plan = DeploymentPlan::working_tree(
                        request_manifest,
                        project_dir,
                        skip_build,
                        at,
                        workdir_path,
                        &nonce,
                    );
                    record_plan(&plan, &transcript_id, &mut recorder);
                    execute(&plan, captured, &mut recorder)
                }
                Source::DefaultBranch => {
                    let prepared = prepare_default_branch(project_dir, at, captured)?;
                    let source_manifest =
                        load_default_branch_manifest(manifest_path, &prepared, &overrides)?;
                    recorder.identity(&source_manifest.name, source_manifest.host());
                    transcript_target.replace(Some((
                        source_manifest.host().to_owned(),
                        source_manifest.name.clone(),
                    )));
                    let plan = DeploymentPlan::default_branch(
                        &source_manifest,
                        prepared,
                        at,
                        workdir_path,
                        &nonce,
                    );
                    record_plan(&plan, &transcript_id, &mut recorder);
                    execute(&plan, captured, &mut recorder)
                }
            }
        },
        |transcript| {
            let target = transcript_target.borrow();
            match target.as_ref() {
                Some((host, name)) => {
                    persist_transcript(host, name, &transcript_id.borrow(), transcript, log)
                }
                None => log.line(
                    "    warning: could not persist deploy transcript: deploy host was not resolved",
                ),
            }
        },
    );
    events::record(&recorder.finish(&outcome), log);
    outcome
}

fn deploy_workdir(name: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("tugboat-{name}-"))
        .tempdir()
        .context("creating deploy work directory")
}

fn record_plan(
    plan: &DeploymentPlan<'_>,
    transcript_id: &RefCell<String>,
    recorder: &mut events::Recorder,
) {
    transcript_id.replace(plan.id.clone());
    if let Some(stamp) = &plan.stamp {
        recorder.stamped(
            &stamp.sha,
            &stamp.short,
            stamp.branch.as_deref(),
            stamp.dirty,
        );
    }
}

fn execute(plan: &DeploymentPlan<'_>, log: &dyn LogSink, rec: &mut events::Recorder) -> Result<()> {
    let host = plan.manifest.host();

    // 1. Build.
    rec.entering(events::Stage::Build);
    if plan.skip_build {
        note(log, "skipping build (--skip-build)");
    } else {
        step(log, "BUILD", &plan.build_cmd);
        let build_env = build_environment(&plan.manifest.build.requirements, log)?;
        let t = Instant::now();
        run_local(&plan.build_cmd, &plan.build_dir, &build_env, log).context("build failed")?;
        rec.completed(events::Stage::Build, t);
    }

    // 2. Confirm every artifact exists locally, of the right kind, before
    //    touching the host.
    rec.entering(events::Stage::Artifacts);
    for artifact in &plan.artifacts {
        match artifact.manifest.kind {
            ArtifactKind::File if !artifact.src.is_file() => {
                bail!(
                    "file artifact not found after build: {}",
                    artifact.src.display()
                )
            }
            ArtifactKind::Dir if !artifact.src.is_dir() => {
                bail!(
                    "dir artifact not found after build: {}",
                    artifact.src.display()
                )
            }
            _ => {}
        }
    }

    // 3. Execute the explicit prepare → activate → verify → compensate/cleanup
    //    state machine. Its report distinguishes a restored deployment from an
    //    ambiguous or incomplete recovery; no inference is made from SSH alone.
    rec.entering(events::Stage::Install);
    let t = Instant::now();
    let transaction = remote::execute(plan, log);
    rec.transaction_outcome(transaction.report.outcome);
    rec.completed(events::Stage::Install, t);

    if transaction.error.is_none() {
        if let Some(verify_cfg) = &plan.manifest.verify {
            step(log, "VERIFY", &verify_cfg.url);
            match verify(verify_cfg, log) {
                Ok(()) => log.line(&format!("    reachable at {}", verify_cfg.url)),
                Err(err) => log.line(&format!(
                    "    warning: {} not reachable from here ({err}); the service is healthy on the host",
                    verify_cfg.url
                )),
            }
        }
        log.line(&format!("\n✓ {} deployed to {host}", plan.manifest.name));
    }
    match transaction.error {
        Some(error) => {
            log.line(&format!("!! deployment failed: {error:#}"));
            Err(error)
        }
        None => Ok(()),
    }
}

/// Expand manifest placeholders: `{workdir}` (the deploy's fresh temp dir) and
/// `{workspace}` (the repository checkout root being built — the cargo
/// workspace root for fleet members, where the shared `target/` lives).
fn subst(input: &str, workdir: &str, workspace: &str) -> String {
    input
        .replace("{workdir}", workdir)
        .replace("{workspace}", workspace)
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
/// property of the machine, not of any service. A manifest declares the
/// capability it requires; the engine resolves that capability here.
const MUSL_TOOLCHAINS: [MuslToolchain; 2] = [
    MuslToolchain {
        cc: "x86_64-linux-musl-gcc",
        ar: "x86_64-linux-musl-ar",
    },
    MuslToolchain {
        cc: "musl-gcc",
        ar: "ar",
    },
];

/// Resolve all explicitly declared build capabilities into subprocess
/// environment. The shell recipe remains opaque: changing its wording cannot
/// silently change which toolchain tugboat selects.
fn build_environment(
    requirements: &[BuildRequirement],
    log: &dyn LogSink,
) -> Result<Vec<(&'static str, String)>> {
    let mut environment = Vec::new();
    for requirement in requirements {
        match requirement {
            BuildRequirement::X86_64LinuxMusl => {
                let tool = musl_toolchain()?;
                note(log, &format!("musl C toolchain: {}", tool.cc));
                environment.extend(musl_env(tool));
            }
        }
    }
    Ok(environment)
}

fn musl_toolchain() -> Result<&'static MuslToolchain> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    match first_toolchain_in(&dirs) {
        Some(tool) => Ok(tool),
        None => bail!(
            "build requires {MUSL_TARGET} but no musl C toolchain is on PATH \
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
        (
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
            tool.cc.to_string(),
        ),
        ("CC_x86_64_unknown_linux_musl", tool.cc.to_string()),
        ("AR_x86_64_unknown_linux_musl", tool.ar.to_string()),
    ]
}

/// How many past deploy transcripts to keep per service on the host.
const TRANSCRIPT_KEEP: usize = 50;

/// Persist the deploy transcript to `/var/lib/tugboat/<name>/<id>.log` on the
/// host, alongside the ledger, then prune to the most recent [`TRANSCRIPT_KEEP`].
/// Best-effort: any failure is surfaced as a warning and otherwise ignored, so a
/// transcript hiccup can never change a deploy's outcome. The transcript arrives
/// on stdin (→ `tee`), so its contents never have to be shell-quoted.
fn persist_transcript(
    host: &str,
    name: &str,
    id: &str,
    transcript: Result<File>,
    log: &dyn LogSink,
) {
    let transcript = match transcript {
        Ok(transcript) => transcript,
        Err(error) => {
            log.line(&format!(
                "    warning: could not finalize deploy transcript: {error:#}"
            ));
            return;
        }
    };
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
    if let Err(err) =
        transport::ssh_pipe_file_quiet(host, &remote, transcript, Duration::from_secs(30))
    {
        log.line(&format!(
            "    warning: could not persist deploy transcript: {err}"
        ));
    }
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
fn deploy_id(at: u64, stamp: Option<&Stamp>, nonce: &str) -> String {
    tugboat_ledger::deploy_id(at, stamp.map(|s| s.short.as_str()), nonce)
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
    format!(
        "$sudo mkdir -p /var/lib/tugboat \
         && printf '%s\\n' {payload} | $sudo tee -a {path} >/dev/null \
         && $sudo chmod 0644 {path}",
        payload = shq(&payload),
        path = shq(&path),
    )
}

fn step(log: &dyn LogSink, tag: &str, msg: &str) {
    log.line(&format!("==> {tag}: {msg}"));
}

fn note(log: &dyn LogSink, msg: &str) {
    log.line(&format!("==> {msg}"));
}

#[cfg(test)]
mod tests {
    use super::{cargo_workspace_root, working_tree_workspace};

    /// A cargo workspace root, a member inside it, and a nested directory.
    fn cargo_workspace(root: &std::path::Path) {
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"svc\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("svc/src")).unwrap();
        std::fs::write(
            root.join("svc/Cargo.toml"),
            "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
    }

    #[test]
    fn the_workspace_root_is_found_from_a_member() {
        let dir = tempfile::tempdir().unwrap();
        // The temp dir may be a symlink (/tmp on macOS); compare resolved.
        let root = dir.path().canonicalize().unwrap();
        cargo_workspace(&root);
        assert_eq!(cargo_workspace_root(&root.join("svc")), Some(root.clone()));
        assert_eq!(cargo_workspace_root(&root), Some(root));
    }

    #[test]
    fn a_member_manifest_alone_is_not_a_workspace_root() {
        // `[package]` is not `[workspace]`. Returning the member would put
        // `target/` one level too deep.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("svc")).unwrap();
        std::fs::write(
            root.join("svc/Cargo.toml"),
            "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(cargo_workspace_root(&root.join("svc")), None);
    }

    #[test]
    fn a_dependency_marked_workspace_true_is_not_a_workspace_root() {
        // `workspace = true` on a dependency is a different thing entirely,
        // and a string match for "workspace" would be fooled by it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n\n             [dependencies]\nserde = { workspace = true }\n",
        )
        .unwrap();
        assert_eq!(cargo_workspace_root(&root), None);
    }

    #[test]
    fn an_unparseable_manifest_does_not_stop_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        cargo_workspace(&root);
        std::fs::write(root.join("svc/Cargo.toml"), "this is not toml {{{").unwrap();
        assert_eq!(cargo_workspace_root(&root.join("svc")), Some(root));
    }

    #[test]
    fn the_nearest_workspace_root_wins() {
        // A vendored sub-workspace builds into its own target dir.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        cargo_workspace(&root);
        let inner = root.join("svc/inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(cargo_workspace_root(&inner), Some(inner));
    }

    #[test]
    fn a_cargo_workspace_beats_the_git_toplevel() {
        // The bug this fixes. A jj workspace lives INSIDE the git checkout and
        // has its own cargo workspace root; resolving from git would answer
        // with the outer checkout, whose target/ the build never writes to.
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&outer)
            .status()
            .unwrap()
            .success());
        cargo_workspace(&outer);

        let nested = outer.join(".workspaces/slug");
        std::fs::create_dir_all(&nested).unwrap();
        cargo_workspace(&nested);

        assert_eq!(
            working_tree_workspace(&nested.join("svc")),
            nested,
            "the artifact path must follow cargo, not git"
        );
    }

    #[test]
    fn a_non_rust_project_still_falls_back_to_the_git_toplevel() {
        // Container builds and static-directory services have no cargo
        // workspace, and must keep behaving exactly as before.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let svc = root.join("svc");
        std::fs::create_dir_all(&svc).unwrap();
        assert_eq!(working_tree_workspace(&svc), root);
    }

    #[test]
    fn a_directory_that_is_neither_answers_with_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(working_tree_workspace(&root), root);
    }

    use super::*;

    /// A build that doesn't target musl needs no toolchain — and must not fail
    /// on machines that have none (e.g. a Docker-artifact deploy).
    #[test]
    fn non_musl_build_needs_no_toolchain() {
        struct Quiet;
        impl LogSink for Quiet {
            fn line(&self, _: &str) {}
        }
        assert!(build_environment(&[], &Quiet).unwrap().is_empty());
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
        std::fs::set_permissions(dir.join("musl-gcc"), std::fs::Permissions::from_mode(0o644))
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
        assert!(env.iter().any(
            |(k, v)| *k == "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER" && v == "musl-gcc"
        ));
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
        let nonce = "0123456789abcdef";
        assert_eq!(
            deploy_id(1_718_900_000, Some(&stamp), nonce),
            "1718900000-1a2b3c4d0123456789abcdef"
        );
        assert_eq!(deploy_id(42, None, nonce), "42-nogit0123456789abcdef");

        let is_valid = |id: &str| {
            let (l, r) = id.split_once('-').expect("id has a dash");
            !l.is_empty()
                && l.bytes().all(|b| b.is_ascii_digit())
                && !r.is_empty()
                && r.bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        };
        assert!(is_valid(&deploy_id(1_718_900_000, Some(&stamp), nonce)));
        assert!(is_valid(&deploy_id(42, None, nonce)));
        assert_ne!(
            deploy_id(42, Some(&stamp), "0000000000000000"),
            deploy_id(42, Some(&stamp), "0000000000000001")
        );
    }

    /// The ledger line is v2 and carries the id linking it to the transcript.
    #[test]
    fn ledger_payload_is_v2_with_id() {
        let stamp = sample_stamp();
        let id = deploy_id(stamp.deployed_at, Some(&stamp), "0123456789abcdef");
        let line = ledger_payload(&stamp, &id, "deployed");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON line");
        assert_eq!(v["v"], 2);
        assert_eq!(v["id"], "1718900000-1a2b3c4d0123456789abcdef");
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
        assert_eq!(
            subst("{workdir}/bundle.tar", "/wd", "/checkout"),
            "/wd/bundle.tar"
        );
        assert_eq!(subst("web/dist", "/wd", "/checkout"), "web/dist");
    }

    #[test]
    fn each_deploy_gets_a_fresh_work_directory() {
        let first = deploy_workdir("service").unwrap();
        std::fs::write(first.path().join("stale-artifact"), "old").unwrap();
        let second = deploy_workdir("service").unwrap();

        assert_ne!(first.path(), second.path());
        assert!(std::fs::read_dir(second.path()).unwrap().next().is_none());
    }

    #[test]
    fn dry_run_renders_the_same_resolved_plan_execution_consumes() {
        use crate::manifest::{Artifact, Build, Lighthouse};

        struct Collect(Mutex<Vec<String>>);
        impl LogSink for Collect {
            fn line(&self, line: &str) {
                self.0.lock().unwrap().push(line.to_owned());
            }
        }

        let project = tempfile::tempdir().unwrap();
        let workdir = project.path().join("work");
        let manifest = Manifest {
            name: "service".to_owned(),
            description: None,
            host: Some("deepwa7er".to_owned()),
            port: None,
            state: None,
            build: Build {
                cmd: "build --out {workdir}/service --root {workspace}".to_owned(),
                requirements: Vec::new(),
            },
            artifacts: vec![Artifact {
                src: "{workdir}/service".to_owned(),
                dest: "/usr/local/bin/service".to_owned(),
                kind: ArtifactKind::File,
                mode: "0755".to_owned(),
            }],
            health: None,
            verify: None,
            lighthouse: Lighthouse::default(),
        };
        let log = Collect(Mutex::new(Vec::new()));
        let plan = DeploymentPlan::working_tree(
            &manifest,
            project.path(),
            false,
            42,
            workdir.clone(),
            "0123456789abcdef",
        );

        assert_eq!(
            plan.build_cmd,
            format!(
                "build --out {}/service --root {}",
                workdir.display(),
                project.path().display()
            )
        );
        assert_eq!(plan.artifacts[0].src, workdir.join("service"));

        plan.print(project.path(), &log);
        let transcript = log.0.lock().unwrap().join("\n");
        assert!(transcript.contains(&plan.build_cmd));
        assert!(transcript.contains(&plan.artifacts[0].src.display().to_string()));
    }

    /// Run git with a deterministic identity (mirrors git.rs's test helper).
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
            ])
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
        assert!(
            prepared.checkout_root.join(".git").exists(),
            "worktree root is the checkout"
        );
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

    #[test]
    fn default_branch_uses_its_committed_manifest_with_local_runtime_overrides() {
        let base =
            std::env::temp_dir().join(format!("tugboat-source-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let origin = base.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let clone = base.join("clone");
        git(&base, &["clone", "-q", "origin.git", "clone"]);
        let service = clone.join("source-svc");
        std::fs::create_dir_all(&service).unwrap();
        let manifest_path = service.join("deploy.toml");
        let manifest_text = |build: &str, artifact: &str| {
            format!(
                "name = \"source-svc\"\nhost = \"origin-host\"\n[build]\ncmd = \"{build}\"\n[[artifacts]]\nsrc = \"{artifact}\"\ndest = \"/tmp/source-svc\"\n"
            )
        };
        std::fs::write(
            &manifest_path,
            manifest_text("origin build", "origin-artifact"),
        )
        .unwrap();
        git(&clone, &["add", "."]);
        git(&clone, &["commit", "-q", "-m", "init"]);
        git(&clone, &["push", "-q", "origin", "main"]);

        std::fs::write(&manifest_path, "invalid local manifest {{{").unwrap();
        std::fs::write(
            service.join("deploy.local.toml"),
            "host = \"runtime-host\"\n",
        )
        .unwrap();

        struct Quiet;
        impl LogSink for Quiet {
            fn line(&self, _: &str) {}
        }
        let overrides = manifest::runtime_overrides(&manifest_path, Some("runtime-host")).unwrap();
        let prepared = prepare_default_branch(&service, 42, &Quiet).unwrap();
        let source_manifest =
            load_default_branch_manifest(&manifest_path, &prepared, &overrides).unwrap();
        let plan = DeploymentPlan::default_branch(
            &source_manifest,
            prepared,
            42,
            base.join("work"),
            "nonce",
        );

        assert_eq!(plan.build_cmd, "origin build");
        assert_eq!(
            plan.artifacts[0].src,
            plan.build_dir.join("origin-artifact")
        );
        assert_eq!(plan.manifest.host(), "runtime-host");
        drop(plan);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dry_run_of_default_branch_does_not_fetch_or_create_a_worktree() {
        struct Collect(Mutex<Vec<String>>);
        impl LogSink for Collect {
            fn line(&self, line: &str) {
                self.0.lock().unwrap().push(line.to_owned());
            }
        }

        let base = std::env::temp_dir().join(format!("tugboat-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let origin = base.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let clone = base.join("clone");
        git(&base, &["clone", "-q", "origin.git", "clone"]);
        let service = clone.join("preview-svc");
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(service.join("artifact"), "contents").unwrap();
        let manifest_path = service.join("deploy.toml");
        std::fs::write(
            &manifest_path,
            "name = \"preview-svc\"\nhost = \"deepwa7er\"\n[build]\ncmd = \"origin-build\"\n[[artifacts]]\nsrc = \"artifact\"\ndest = \"/tmp/preview-svc\"\n",
        )
        .unwrap();
        git(&clone, &["add", "."]);
        git(&clone, &["commit", "-q", "-m", "init"]);
        git(&clone, &["push", "-q", "origin", "main"]);
        git(
            &clone,
            &["remote", "set-url", "origin", "/definitely/missing"],
        );

        let checkout = default_branch_layout(&service).unwrap().checkout_root;
        assert!(!checkout.exists());
        std::fs::write(&manifest_path, "invalid local manifest {{{").unwrap();
        let log = Collect(Mutex::new(Vec::new()));
        run(
            &manifest_path,
            &service,
            Source::DefaultBranch,
            true,
            None,
            &log,
        )
        .unwrap();
        assert!(
            !checkout.exists(),
            "dry-run must not create its planned worktree"
        );
        assert!(log.0.lock().unwrap().join("\n").contains("origin-build"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// CapturingSink forwards to its inner sink and spools the transcript.
    #[test]
    fn capturing_sink_tees_and_spools() {
        use std::io::Read;

        struct Collect(Mutex<Vec<String>>);
        impl LogSink for Collect {
            fn line(&self, line: &str) {
                self.0.lock().unwrap().push(line.to_owned());
            }
        }
        let inner = Collect(Mutex::new(Vec::new()));
        let cap = CapturingSink::new(&inner).unwrap();
        cap.line("first");
        cap.line("second");
        let mut transcript = String::new();
        cap.into_reader()
            .unwrap()
            .read_to_string(&mut transcript)
            .unwrap();
        assert_eq!(transcript, "first\nsecond\n");
        assert_eq!(inner.0.lock().unwrap().as_slice(), ["first", "second"]);
    }

    #[test]
    fn transcript_finalization_runs_after_an_early_failure() {
        use std::cell::Cell;

        struct Quiet;
        impl LogSink for Quiet {
            fn line(&self, _: &str) {}
        }

        let finalized = Cell::new(false);
        let outcome: Result<()> = with_transcript(
            &Quiet,
            |log| {
                log.line("build output before failure");
                bail!("build failed")
            },
            |transcript| {
                use std::io::Read;
                let mut transcript = transcript.unwrap();
                let mut text = String::new();
                transcript.read_to_string(&mut text).unwrap();
                assert_eq!(text, "build output before failure\n");
                finalized.set(true);
            },
        );
        assert!(outcome.is_err());
        assert!(finalized.get());
    }
}
