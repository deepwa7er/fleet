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
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
use tokio::sync::broadcast;

use crate::deploy::{self, LogSink};
use crate::fleet::{self, Fleet};
use crate::git;
use crate::manifest;
use crate::version::BuildInfo;

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
    fleet: Fleet,
    token: String,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// Service labels with a deploy currently in flight (one at a time each).
    in_flight: Mutex<HashSet<String>>,
    counter: AtomicU64,
    /// Unix seconds when this daemon process started. A self-deploy watches this
    /// flip to a newer value to confirm the daemon actually restarted onto the
    /// new binary (see [`health`]).
    started_unix: u64,
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
        fleet,
        token,
        jobs: Mutex::new(HashMap::new()),
        in_flight: Mutex::new(HashSet::new()),
        counter: AtomicU64::new(1),
        started_unix,
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(async move {
        // Everything is behind the bearer token except `/health`, which carries
        // no secrets and must be reachable by a self-deploy that is restarting
        // the daemon (and so cannot assume it holds the token).
        let authed = Router::new()
            .route("/services", get(list_services))
            .route("/status", get(list_status))
            .route("/deploy/{name}", post(deploy_service))
            .route("/jobs/{id}/stream", get(job_stream))
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
    let services = state
        .fleet
        .members
        .iter()
        .filter(|m| m.deploy)
        .map(|m| ServiceInfo {
            name: m.label().to_owned(),
            manifest_present: state.fleet.manifest_path(m).is_file(),
        })
        .collect();
    Json(services)
}

/// Local repo state for one deployable member, plus (when the caller supplied
/// the currently-deployed sha) how the working tree relates to it.
#[derive(Serialize)]
struct StatusInfo {
    name: String,
    /// GitHub web base (`https://github.com/owner/repo`) from the member's
    /// remote, so a reader can build commit/compare links. `None` off GitHub.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_url: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
    head_short: Option<String>,
    dirty: bool,
    dirty_files: u32,
    upstream_ahead: u32,
    upstream_behind: u32,
    /// Commits on local HEAD not yet in the deployed sha (only when the deployed
    /// sha was supplied and is an ancestor of HEAD).
    #[serde(skip_serializing_if = "Option::is_none")]
    undeployed_commits: Option<u32>,
    /// Whether the deployed sha is an ancestor of HEAD — i.e. local is strictly
    /// ahead (`true`) vs diverged (`false`). Absent when no deployed sha given.
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

/// `GET /status[?deployed=name:sha,…]` — per-member local git state, plus the
/// relationship to the deployed sha when one is provided.
async fn list_status(
    State(state): State<Arc<ServeState>>,
    Query(query): Query<StatusQuery>,
) -> Json<Vec<StatusInfo>> {
    let deployed = parse_deployed(&query.deployed);
    let infos = state
        .fleet
        .members
        .iter()
        .filter(|m| m.deploy)
        .map(|m| {
            let name = m.label().to_owned();
            let dir = state.fleet.dir(m);
            let st = git::state(&dir);
            let head_short = st.head_sha.as_deref().map(|s| git::short(s).to_owned());

            let (undeployed_commits, deployed_is_ancestor) =
                match (deployed.get(&name), st.head_sha.as_deref()) {
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
                repo_url: git::github_web_url(&m.repo),
                branch: st.branch,
                head_sha: st.head_sha,
                head_short,
                dirty: st.dirty,
                dirty_files: st.dirty_files,
                upstream_ahead: st.upstream_ahead,
                upstream_behind: st.upstream_behind,
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
    // never leaves a service marked busy.
    let member = state
        .fleet
        .find(&name)
        .ok_or((StatusCode::NOT_FOUND, format!("no fleet member named `{name}`")))?;
    if !member.deploy {
        return Err((
            StatusCode::FORBIDDEN,
            format!("`{name}` is configured with deploy = false"),
        ));
    }
    let manifest_path = state.fleet.manifest_path(member);
    if !manifest_path.is_file() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("deploy manifest not found at {}", manifest_path.display()),
        ));
    }

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
            deploy::run(&manifest, project_dir, false, false, &sink)
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
