//! `tugboat serve` — a long-running daemon that exposes the fleet's deploy
//! pipeline over HTTP so it can be triggered from another machine (e.g. the
//! lighthouse dashboard on the VPS, reached from any device on the tailnet).
//!
//! The build still happens here, on the dev machine where the source and the
//! toolchain live: a deploy request runs the exact same [`deploy::run`] pipeline
//! as the CLI, against the member's current working tree. The transcript is
//! streamed back to the caller line-by-line over Server-Sent Events.
//!
//! The daemon binds to a caller-supplied address (point it at this machine's
//! tailnet IP) and requires a bearer token on every request — the endpoint
//! executes builds, so it gets an explicit credential rather than relying on
//! network reachability alone.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Notify};

use crate::deploy::{self, LogSink};
use crate::docs;
use crate::fleet::{self, Fleet};
use crate::git;
use crate::manifest;
use crate::version::BuildInfo;

/// How long to wait for a burst of commits to settle before rebuilding the docs,
/// so committing across several repos at once coalesces into one build.
const DOCS_DEBOUNCE: Duration = Duration::from_secs(20);

/// How often the daemon checks for fleet changes on its own — the backstop that
/// keeps the docs current even if a commit hook never fired (daemon was down, a
/// repo lacks the hook, or a commit landed on another machine and was pulled).
const DOCS_CATCHUP_INTERVAL: Duration = Duration::from_secs(300);

/// How often the daemon fetches every deployable's `origin`, so the dashboard's
/// "undeployed commits" reflects freshly-merged work without waiting for a deploy
/// or a manual pull. Fetch only updates refs/objects — never the working tree.
const FETCH_INTERVAL: Duration = Duration::from_secs(180);

/// CLI arguments for `tugboat serve`.
pub struct ServeArgs {
    pub bind: IpAddr,
    pub port: u16,
    pub manifest: Option<PathBuf>,
}

/// Capacity of each job's live-event channel. The full transcript is always
/// available from the job's buffer on connect; this only bounds how far a
/// momentarily-slow live viewer can fall behind before it skips ahead.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Once the job map exceeds this, finished jobs are dropped to bound memory.
/// Running jobs are always retained.
const MAX_RETAINED_JOBS: usize = 64;

/// A single deploy run and its streamable transcript.
struct Job {
    tx: broadcast::Sender<JobEvent>,
    inner: Mutex<JobInner>,
}

struct JobInner {
    /// Every transcript line so far, replayed to any client that connects late.
    log: Vec<String>,
    /// `None` while running; `Some` once the deploy has finished.
    outcome: Option<Outcome>,
}

#[derive(Clone)]
struct Outcome {
    ok: bool,
    error: Option<String>,
}

/// An event in a job's stream: one transcript line, or the terminal outcome.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum JobEvent {
    Line {
        text: String,
    },
    Done {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// A [`LogSink`] that appends to a job's buffer and broadcasts each line to any
/// connected live viewers. Used by the deploy pipeline running on a blocking
/// task; the buffer push and the broadcast happen under the same lock the SSE
/// handler takes when it snapshots, so no line is ever both buffered and
/// delivered live (no duplicates) nor missed (no gaps).
struct ChannelSink {
    job: Arc<Job>,
}

impl LogSink for ChannelSink {
    fn line(&self, line: &str) {
        let mut inner = self.job.inner.lock().unwrap();
        inner.log.push(line.to_owned());
        // A send with no receivers returns Err; that's expected and ignored.
        let _ = self.job.tx.send(JobEvent::Line { text: line.to_owned() });
    }
}

struct ServeState {
    /// The fleet, reloaded from disk per request via [`ServeState::fleet`] so
    /// edits to `fleet.toml` (e.g. `tugboat deploy` auto-registering a new
    /// member) take effect without restarting the daemon.
    fleet: RwLock<Fleet>,
    /// Path to `fleet.toml`, the source for the reloads above.
    manifest_path: PathBuf,
    token: String,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// Service labels with a deploy currently in flight (one at a time each).
    in_flight: Mutex<HashSet<String>>,
    counter: AtomicU64,
    /// Unix seconds when this daemon process started. A self-deploy watches this
    /// flip to a newer value to confirm the daemon actually restarted onto the
    /// new binary (see [`health`]).
    started_unix: u64,
    /// Pulsed to wake the docs keeper — by a commit-hook ping (`/docs/refresh`)
    /// and by the periodic catch-up. `notify_one` coalesces, so a burst of
    /// triggers collapses to a single wake.
    docs_notify: Arc<Notify>,
    /// The docs auto-refresh state (last build outcome, whether one is running).
    docs: Mutex<DocsStatus>,
}

impl ServeState {
    /// A read view of the fleet, reloaded from `fleet.toml` first so changes to
    /// the manifest take effect without a daemon restart. If the reload fails
    /// (a partial write, or a malformed file), the last good copy is kept so a
    /// transient bad read can't blank out the fleet.
    fn fleet(&self) -> RwLockReadGuard<'_, Fleet> {
        match fleet::load(&self.manifest_path) {
            Ok(fresh) => *self.fleet.write().unwrap() = fresh,
            Err(err) => {
                eprintln!("tugboat serve: keeping last-good fleet.toml (reload failed: {err:#})")
            }
        }
        self.fleet.read().unwrap()
    }
}

/// The state of the docs auto-refresh, surfaced at `GET /docs`.
#[derive(Default, Clone, Serialize)]
struct DocsStatus {
    /// Whether a docs build is running right now.
    building: bool,
    /// The most recent finished build, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<DocsRun>,
}

/// The outcome of one finished docs build.
#[derive(Clone, Serialize)]
struct DocsRun {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    finished_unix: u64,
}

/// `GET /health` payload — which build is running and since when. Unauthenticated
/// on purpose: it exposes no secrets and is the signal a self-deploy polls to
/// confirm the daemon came back up on the new binary.
#[derive(Serialize)]
struct HealthInfo {
    build: BuildInfo,
    pid: u32,
    started_unix: u64,
}

#[derive(Serialize)]
struct ServiceInfo {
    /// The fleet member label, also the name to POST to `/deploy/{name}`.
    name: String,
    /// Whether the member's deploy manifest is present on disk.
    manifest_present: bool,
}

#[derive(Serialize)]
struct DeployStarted {
    job_id: String,
}

/// Synchronous entry point: build the shared state and run the async server on a
/// dedicated runtime, keeping the rest of tugboat (deploy, fleet) plain sync.
pub fn run(args: ServeArgs) -> Result<()> {
    let token = std::env::var("TUGBOAT_SERVE_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .context(
            "TUGBOAT_SERVE_TOKEN must be set to a non-empty bearer token \
             (the deploy endpoint runs builds, so it is never unauthenticated)",
        )?;

    let manifest_path = fleet::resolve_manifest(args.manifest.as_deref())?;
    let fleet = fleet::load(&manifest_path)?;

    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let state = Arc::new(ServeState {
        fleet: RwLock::new(fleet),
        manifest_path,
        token,
        jobs: Mutex::new(HashMap::new()),
        in_flight: Mutex::new(HashSet::new()),
        counter: AtomicU64::new(1),
        started_unix,
        docs_notify: Arc::new(Notify::new()),
        docs: Mutex::new(DocsStatus::default()),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(async move {
        // Keep each deployable's origin refs current so /status (and the dashboard
        // it feeds) reflects merged-but-undeployed commits without a manual pull.
        tokio::spawn(fetch_keeper(state.clone()));

        // The docs auto-refresh runs only when the fleet actually has a docs site
        // configured. The keeper does the (debounced, one-at-a-time) builds; the
        // catch-up pulses it on a timer as the backstop.
        if state.fleet().docs.is_some() {
            tokio::spawn(docs_keeper(state.clone()));
            tokio::spawn(docs_catchup(state.clone()));
        }

        // Everything is behind the bearer token except `/health`, which carries
        // no secrets and must be reachable by a self-deploy that is restarting
        // the daemon (and so cannot assume it holds the token).
        let authed = Router::new()
            .route("/services", get(list_services))
            .route("/status", get(list_status))
            .route("/deploy/{name}", post(deploy_service))
            .route("/jobs/{id}/stream", get(job_stream))
            .route("/docs", get(docs_status))
            .route("/docs/refresh", post(docs_refresh))
            .layer(middleware::from_fn_with_state(state.clone(), auth));
        let app = Router::new()
            .route("/health", get(health))
            .merge(authed)
            .with_state(state);

        let addr = SocketAddr::new(args.bind, args.port);
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind {addr}"))?;
        println!("tugboat serve listening on http://{addr}");
        axum::serve(listener, app).await.context("server error")?;
        Ok::<_, anyhow::Error>(())
    })
}

/// Reject any request without a matching `Authorization: Bearer <token>`.
async fn auth(
    State(state): State<Arc<ServeState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match presented {
        Some(token) if constant_time_eq(token, &state.token) => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Length-independent, branch-free byte comparison, so a wrong token can't be
/// recovered from response timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `GET /health` — the running build and this process's start time. No auth (see
/// the router setup); used by `tugboat self-deploy` to confirm the daemon
/// restarted onto the new binary.
async fn health(State(state): State<Arc<ServeState>>) -> Json<HealthInfo> {
    Json(HealthInfo {
        build: BuildInfo::current(),
        pid: std::process::id(),
        started_unix: state.started_unix,
    })
}

/// `GET /services` — the deployable fleet members.
async fn list_services(State(state): State<Arc<ServeState>>) -> Json<Vec<ServiceInfo>> {
    let root = state.fleet().root_dir();
    let services = fleet::discover_deployables(&root)
        .unwrap_or_default()
        .into_iter()
        .map(|d| ServiceInfo {
            name: d.name,
            manifest_present: true,
        })
        .collect();
    Json(services)
}

/// One deployable member's deploy-relevant git state: origin's default branch —
/// the code a deploy ships — plus (when the caller supplied the currently-deployed
/// sha) how that branch relates to what's running.
#[derive(Serialize)]
struct StatusInfo {
    name: String,
    /// GitHub web base (`https://github.com/owner/repo`) from the member's
    /// remote, so a reader can build commit/compare links. `None` off GitHub.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_url: Option<String>,
    /// Origin's default branch — what a deploy ships.
    branch: Option<String>,
    /// Head of `origin/<default-branch>` (the sha a deploy would ship).
    head_sha: Option<String>,
    head_short: Option<String>,
    /// Always `false`: a deploy ships a clean checkout of the default branch, so
    /// the shipped tree is never dirty. Retained for the dashboard's status
    /// contract (its freshness verdict still reads this field).
    dirty: bool,
    /// Commits on `origin/<default-branch>` not yet in the deployed sha (only when
    /// the deployed sha was supplied and is an ancestor of that branch head).
    #[serde(skip_serializing_if = "Option::is_none")]
    undeployed_commits: Option<u32>,
    /// Whether the deployed sha is an ancestor of `origin/<default-branch>` — i.e.
    /// the branch is strictly ahead (`true`) vs diverged (`false`). Absent when no
    /// deployed sha was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    deployed_is_ancestor: Option<bool>,
}

#[derive(Deserialize)]
struct StatusQuery {
    /// Optional `name:sha,name:sha,…` of currently-deployed shas, so the daemon
    /// can compute each member's relationship to what's running.
    deployed: Option<String>,
}

/// Parse the `deployed` query into a name → sha map.
fn parse_deployed(raw: &Option<String>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(raw) = raw else { return map };
    for pair in raw.split(',') {
        if let Some((name, sha)) = pair.split_once(':') {
            let (name, sha) = (name.trim(), sha.trim());
            if !name.is_empty() && !sha.is_empty() {
                map.insert(name.to_owned(), sha.to_owned());
            }
        }
    }
    map
}

/// `GET /status[?deployed=name:sha,…]` — each member's `origin/<default-branch>`
/// state (the code a deploy ships), plus its relationship to the deployed sha when
/// one is provided. Reads local remote-tracking refs (no network per request);
/// the background fetch keeps them current.
async fn list_status(
    State(state): State<Arc<ServeState>>,
    Query(query): Query<StatusQuery>,
) -> Json<Vec<StatusInfo>> {
    let deployed = parse_deployed(&query.deployed);
    let root = state.fleet().root_dir();
    let infos = fleet::discover_deployables(&root)
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            let name = d.name;
            let dir = d.dir;
            let repo_url = git::out(&dir, &["remote", "get-url", "origin"])
                .ok()
                .flatten()
                .and_then(|r| git::github_web_url(&r));

            // Report origin's default branch — what a deploy ships — not whatever
            // branch the shared checkout is parked on.
            let branch = git::default_branch(&dir).ok();
            let head_sha = branch
                .as_deref()
                .and_then(|b| git::rev_parse(&dir, &format!("origin/{b}")).ok().flatten());
            let head_short = head_sha.as_deref().map(|s| git::short(s).to_owned());

            let (undeployed_commits, deployed_is_ancestor) =
                match (deployed.get(&name), head_sha.as_deref()) {
                    (Some(dep), Some(head)) => {
                        let ancestor = git::is_ancestor(&dir, dep, head);
                        let commits = if ancestor {
                            git::count_commits(&dir, dep, head)
                        } else {
                            None
                        };
                        (commits, Some(ancestor))
                    }
                    _ => (None, None),
                };

            StatusInfo {
                name,
                repo_url,
                branch,
                head_sha,
                head_short,
                dirty: false,
                undeployed_commits,
                deployed_is_ancestor,
            }
        })
        .collect();
    Json(infos)
}

/// `POST /deploy/{name}` — start a deploy job for one member and return its id.
async fn deploy_service(
    State(state): State<Arc<ServeState>>,
    Path(name): Path<String>,
) -> Result<Json<DeployStarted>, (StatusCode, String)> {
    // Validate fully before reserving the in-flight slot, so a rejected request
    // never leaves a service marked busy. Discovery guarantees the manifest
    // exists, so finding the service by name yields a ready-to-use manifest path.
    let root = state.fleet().root_dir();
    let manifest_path = fleet::discover_deployables(&root)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .into_iter()
        .find(|d| d.name == name)
        .map(|d| d.manifest_path)
        .ok_or((
            StatusCode::NOT_FOUND,
            format!(
                "no deployable service named `{name}` (needs a deploy.toml under {})",
                root.display()
            ),
        ))?;

    {
        let mut in_flight = state.in_flight.lock().unwrap();
        if !in_flight.insert(name.clone()) {
            return Err((
                StatusCode::CONFLICT,
                format!("a deploy of `{name}` is already in progress"),
            ));
        }
    }

    let job_id = format!("{name}-{}", state.counter.fetch_add(1, Ordering::SeqCst));
    let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let job = Arc::new(Job {
        tx,
        inner: Mutex::new(JobInner {
            log: Vec::new(),
            outcome: None,
        }),
    });
    {
        let mut jobs = state.jobs.lock().unwrap();
        prune_jobs(&mut jobs);
        jobs.insert(job_id.clone(), job.clone());
    }

    // The deploy engine is synchronous (it drives subprocesses and joins reader
    // threads), so it runs on a blocking task rather than the async executor.
    let state_for_task = state.clone();
    tokio::task::spawn_blocking(move || {
        let sink = ChannelSink { job: job.clone() };
        let result = (|| -> Result<()> {
            let project_dir = manifest_path
                .parent()
                .context("manifest has no parent directory")?;
            let manifest = manifest::load(&manifest_path, None)?;
            deploy::run(&manifest, project_dir, deploy::Source::DefaultBranch, false, &sink)
        })();
        finish(&job, result);
        state_for_task.in_flight.lock().unwrap().remove(&name);
    });

    Ok(Json(DeployStarted { job_id }))
}

/// Record a job's terminal outcome and broadcast it to live viewers.
fn finish(job: &Arc<Job>, result: Result<()>) {
    let outcome = match result {
        Ok(()) => Outcome {
            ok: true,
            error: None,
        },
        Err(err) => Outcome {
            ok: false,
            error: Some(format!("{err:#}")),
        },
    };
    let mut inner = job.inner.lock().unwrap();
    inner.outcome = Some(outcome.clone());
    let _ = job.tx.send(JobEvent::Done {
        ok: outcome.ok,
        error: outcome.error,
    });
}

/// Drop finished jobs once the map grows past the retention cap. Running jobs
/// are always kept (their sinks still hold an `Arc`, but the registry entry is
/// what lets a viewer find them).
fn prune_jobs(jobs: &mut HashMap<String, Arc<Job>>) {
    if jobs.len() <= MAX_RETAINED_JOBS {
        return;
    }
    jobs.retain(|_, job| job.inner.lock().unwrap().outcome.is_none());
}

/// `GET /jobs/{id}/stream` — replay the transcript so far, then stream the rest
/// live over Server-Sent Events, closing once the deploy finishes.
async fn job_stream(State(state): State<Arc<ServeState>>, Path(id): Path<String>) -> Response {
    let Some(job) = state.jobs.lock().unwrap().get(&id).cloned() else {
        return (StatusCode::NOT_FOUND, "no such job").into_response();
    };

    // Snapshot the buffer and subscribe under the same lock the sink uses, so
    // the live subscription begins exactly where the snapshot ends.
    let (buffered, terminal, rx) = {
        let inner = job.inner.lock().unwrap();
        let rx = job.tx.subscribe();
        let buffered: Vec<JobEvent> = inner
            .log
            .iter()
            .map(|text| JobEvent::Line { text: text.clone() })
            .collect();
        let terminal = inner.outcome.as_ref().map(|o| JobEvent::Done {
            ok: o.ok,
            error: o.error.clone(),
        });
        (buffered, terminal, rx)
    };

    let stream = async_stream::stream! {
        for event in buffered {
            if let Ok(sse) = Event::default().json_data(&event) {
                yield Ok::<_, Infallible>(sse);
            }
        }
        // If the job already finished before we connected, its Done event went
        // out before we subscribed — emit it from the snapshot and stop.
        if let Some(done) = terminal {
            if let Ok(sse) = Event::default().json_data(&done) {
                yield Ok(sse);
            }
            return;
        }
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let done = matches!(event, JobEvent::Done { .. });
                    if let Ok(sse) = Event::default().json_data(&event) {
                        yield Ok(sse);
                    }
                    if done {
                        break;
                    }
                }
                // Fell behind the channel: the missed lines are still in the
                // buffer for a reconnect, so keep following the live tail.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /docs` — the docs auto-refresh state (last outcome, whether building).
async fn docs_status(State(state): State<Arc<ServeState>>) -> Json<DocsStatus> {
    Json(state.docs.lock().unwrap().clone())
}

/// `POST /docs/refresh` — request a docs rebuild and return immediately. The
/// commit hooks call this; it just wakes the keeper (which debounces and skips
/// no-op rebuilds), so a commit never waits on a build.
async fn docs_refresh(State(state): State<Arc<ServeState>>) -> StatusCode {
    state.docs_notify.notify_one();
    StatusCode::ACCEPTED
}

/// Fetch every deployable's `origin` on a timer, so the dashboard's
/// "undeployed commits" reflects merged-but-undeployed work without waiting for a
/// deploy or a manual pull. The first tick fires immediately. Fetch touches only
/// refs/objects, so it is safe to run while another tool has a checkout open.
async fn fetch_keeper(state: Arc<ServeState>) {
    let mut ticker = tokio::time::interval(FETCH_INTERVAL);
    loop {
        ticker.tick().await;
        let root = state.fleet().root_dir();
        let dirs: Vec<PathBuf> = fleet::discover_deployables(&root)
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.dir)
            .collect();
        // git is blocking; keep it off the async executor.
        let _ = tokio::task::spawn_blocking(move || {
            for dir in dirs {
                if let Err(err) = git::fetch(&dir) {
                    eprintln!(
                        "tugboat serve: background fetch of {} failed: {err:#}",
                        dir.display()
                    );
                }
            }
        })
        .await;
    }
}

/// Pulse the docs keeper on a timer — the backstop that catches changes no commit
/// hook reported. `interval`'s first tick fires immediately, so a daemon that
/// just started (perhaps onto commits made while it was down) refreshes at once.
async fn docs_catchup(state: Arc<ServeState>) {
    let mut ticker = tokio::time::interval(DOCS_CATCHUP_INTERVAL);
    loop {
        ticker.tick().await;
        state.docs_notify.notify_one();
    }
}

/// Rebuild + reship the docs whenever the fleet changes. Woken by `/docs/refresh`
/// and the catch-up timer; debounces a burst into one build, then rebuilds only
/// if the fleet fingerprint actually moved since the last successful ship — so a
/// redundant wake (or the periodic backstop) costs just a cheap git check.
///
/// Builds run one at a time (this is a single task looping), and the fingerprint
/// is recorded only after a successful ship, so a failed build is retried rather
/// than masked, and a commit landing mid-build is caught on the next wake.
async fn docs_keeper(state: Arc<ServeState>) {
    loop {
        state.docs_notify.notified().await;
        // Let a burst settle. Triggers during this window leave a permit, so the
        // next `notified()` returns at once — nothing is lost, just coalesced.
        tokio::time::sleep(DOCS_DEBOUNCE).await;

        let fingerprint = docs::fleet_fingerprint(&state.fleet());
        if docs::read_stored_fingerprint().as_deref() == Some(fingerprint.as_str()) {
            continue; // nothing changed (e.g. a periodic backstop tick)
        }

        state.docs.lock().unwrap().building = true;
        println!("tugboat: fleet changed — rebuilding docs");

        // Snapshot the fleet so the long build doesn't hold the read lock.
        let snapshot = state.fleet().clone();
        let outcome = tokio::task::spawn_blocking(move || {
            docs::generate(
                &snapshot,
                &docs::Options {
                    out: None,
                    skip_build: false,
                    skip_rustdoc: false,
                    only: None,
                    dry_run: false,
                },
            )
        })
        .await;

        let result = match outcome {
            Ok(result) => result,
            Err(join) => Err(anyhow::anyhow!("docs build task panicked: {join}")),
        };

        let error = match &result {
            Ok(()) => {
                if let Err(err) = docs::write_fingerprint(&fingerprint) {
                    eprintln!("tugboat: docs shipped but recording the fingerprint failed: {err:#}");
                }
                None
            }
            Err(err) => {
                eprintln!("tugboat: docs refresh failed: {err:#}");
                Some(format!("{err:#}"))
            }
        };

        let mut docs_state = state.docs.lock().unwrap();
        docs_state.building = false;
        docs_state.last = Some(DocsRun {
            ok: result.is_ok(),
            error,
            finished_unix: now_unix(),
        });
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
