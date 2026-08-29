//! HTTP handlers.
//!
//! The set of monitored services is discovered at request time from the
//! configured systemd target (plus any units pinned in the config). That same
//! set is the allowlist: every handler that touches a unit first resolves it
//! against the monitored set, and an unknown unit returns `404` before any
//! subprocess runs.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use crate::AppState;
use crate::config::{Config, DeployConfig};
use crate::store::Sample;
use crate::systemd::{self, ServiceStatus};

const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 5_000;

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    lines: Option<u32>,
}

/// A monitored service: its unit name and resolved display label.
struct Monitored {
    unit: String,
    name: String,
}

/// `GET /api/services` — status of every monitored service.
pub async fn list_services(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ServiceStatus>>, StatusCode> {
    let services = monitored(&state.config).await;
    // Reference data (description, URL, docs link) to merge onto live status,
    // joined by unit. Best-effort: an absent fleet.json just leaves it unenriched.
    let fleet = crate::fleet::load(&state.config.fleet_path);
    let mut statuses = Vec::with_capacity(services.len());
    for svc in services {
        match systemd::status(&svc.unit, &svc.name).await {
            Ok(mut status) => {
                status.fleet = fleet.get(&status.unit).cloned();
                statuses.push(status);
            }
            Err(err) => {
                eprintln!("failed to query {}: {err:#}", svc.unit);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
    Ok(Json(statuses))
}

/// `GET /api/services/{unit}/logs?lines=N` — most recent log entries.
pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Vec<systemd::LogEntry>>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES).clamp(1, MAX_LOG_LINES);

    match systemd::recent_logs(&svc.unit, lines).await {
        Ok(entries) => Ok(Json(entries)),
        Err(err) => {
            eprintln!("failed to read logs for {}: {err:#}", svc.unit);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /api/services/{unit}/control/{action}` — start, stop, or restart a
/// service. Returns the unit's status after the action completes so the UI can
/// update immediately.
pub async fn control_service(
    State(state): State<Arc<AppState>>,
    Path((unit, action)): Path<(String, String)>,
) -> Result<Json<ServiceStatus>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let action = systemd::ServiceAction::parse(&action).ok_or(StatusCode::BAD_REQUEST)?;

    if let Err(err) = systemd::control(&svc.unit, action).await {
        eprintln!("failed to {action:?} {}: {err:#}", svc.unit);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    match systemd::status(&svc.unit, &svc.name).await {
        Ok(status) => Ok(Json(status)),
        Err(err) => {
            eprintln!("failed to read status for {} after {action:?}: {err:#}", svc.unit);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `GET /api/services/{unit}/logs/stream` — live log tail over Server-Sent Events.
pub async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;

    let entries = systemd::follow_logs(&svc.unit).map_err(|err| {
        eprintln!("failed to follow logs for {}: {err:#}", svc.unit);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let events = entries.map(|entry| Event::default().json_data(entry));
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// The monitored set: units that are members of the target, unioned with units
/// pinned in the config, each paired with its display label. This is both the
/// dashboard's service list and the allowlist for logs/control.
async fn monitored(config: &Config) -> Vec<Monitored> {
    let mut units = match systemd::discover_units(&config.target).await {
        Ok(units) => units,
        Err(err) => {
            eprintln!("service discovery failed for target {}: {err:#}", config.target);
            Vec::new()
        }
    };
    for pinned in config.pinned_units() {
        if !units.iter().any(|u| u == pinned) {
            units.push(pinned.to_owned());
        }
    }
    units.sort();
    units.dedup();
    units
        .into_iter()
        .map(|unit| {
            let name = config.display_name(&unit);
            Monitored { unit, name }
        })
        .collect()
}

/// Resolve a single unit against the monitored set (the allowlist gate). Unknown
/// units yield `404` before any command runs.
async fn resolve(config: &Config, unit: &str) -> Result<Monitored, StatusCode> {
    monitored(config)
        .await
        .into_iter()
        .find(|m| m.unit == unit)
        .ok_or(StatusCode::NOT_FOUND)
}

/// One member's `origin/<default-branch>` state — the code a deploy ships — from
/// the tugboat daemon's `/status`.
#[derive(Debug, Deserialize)]
struct DaemonStatus {
    name: String,
    /// GitHub web base (`https://github.com/owner/repo`), when the remote is on
    /// GitHub — used to link undeployed commits. Absent otherwise.
    #[serde(default)]
    repo_url: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
    head_short: Option<String>,
    dirty: bool,
    undeployed_commits: Option<u32>,
    deployed_is_ancestor: Option<bool>,
}

use tugboat_ledger::{LedgerEntry, LedgerResult};

/// Read a service's deploy ledger through the shared `tugboat-ledger` contract,
/// oldest entry first. Missing/unreadable → empty. Entries written by a NEWER
/// tugboat than this reader understands are skipped with a warning — better a
/// visible gap than history silently misread.
fn read_ledger(version_dir: &std::path::Path, deploy_name: &str) -> Vec<LedgerEntry> {
    let path = tugboat_ledger::ledger_path(version_dir, deploy_name);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    tugboat_ledger::parse_ledger(&text)
        .into_iter()
        .filter(|e| {
            if !e.is_supported() {
                eprintln!(
                    "ledger entry for {deploy_name} has schema v{} (this reader understands v{}) — skipped; update lighthouse",
                    e.v,
                    tugboat_ledger::LEDGER_VERSION
                );
            }
            e.is_supported()
        })
        .collect()
}

/// The currently-running version: the most recent entry that actually deployed.
/// A trailing `rolled_back` entry means the prior `deployed` one is still live.
fn current_deployed(entries: &[LedgerEntry]) -> Option<&LedgerEntry> {
    for entry in entries.iter().rev() {
        match entry.result {
            LedgerResult::Deployed => return Some(entry),
            LedgerResult::RolledBack => continue,
            LedgerResult::Indeterminate => return None,
        }
    }
    None
}

/// Freshness of one deployable service: how the running version compares to the
/// head of origin's default branch (the code a deploy would ship).
#[derive(Debug, Serialize)]
pub struct DeployStatus {
    unit: String,
    /// `current` | `dirty` | `stale` | `diverged` | `unknown`.
    verdict: &'static str,
    deployed: Option<DeployedInfo>,
    local: Option<LocalInfo>,
    /// GitHub compare link (`…/compare/<deployed>...<head>`) showing the
    /// committed-but-undeployed commits. `None` unless there's such work and the
    /// repo is on GitHub.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes_url: Option<String>,
}

/// A GitHub compare link for the committed work not yet deployed
/// (`deployed...HEAD`), or `None` when there's nothing to link: the verdict has
/// no undeployed commits, the repo isn't on GitHub, or a sha is missing.
fn changes_url(
    verdict: &str,
    repo_url: Option<&str>,
    deployed: Option<&LedgerEntry>,
    head_short: Option<&str>,
) -> Option<String> {
    if verdict != "stale" && verdict != "diverged" {
        return None;
    }
    let base = repo_url?.trim_end_matches('/');
    let deployed = deployed?;
    let head = head_short?;
    Some(format!("{base}/compare/{}...{head}", deployed.short))
}

/// A GitHub link to a single commit (`<repo>/commit/<sha>`), or `None` when the
/// repo isn't on GitHub. Built from the full sha so the link is unambiguous even
/// though the dashboard shows the short form.
fn commit_url(repo_url: Option<&str>, sha: &str) -> Option<String> {
    let base = repo_url?.trim_end_matches('/');
    Some(format!("{base}/commit/{sha}"))
}

/// Query the tugboat daemon's `/status` for every fleet member. `deployed`
/// carries the `name:sha` pairs that let the daemon compute each one's
/// ahead/diverged relationship; pass `""` when only the static fields (repo_url,
/// branch) are wanted. Best-effort: an unreachable or unparsable daemon yields an
/// empty list, so callers degrade to live-only rather than failing.
async fn daemon_statuses(
    http: &reqwest::Client,
    cfg: &DeployConfig,
    deployed: &str,
) -> Vec<DaemonStatus> {
    let url = format!("{}/status", cfg.tugboat_url.trim_end_matches('/'));
    match http
        .get(&url)
        .query(&[("deployed", deployed)])
        .bearer_auth(&cfg.token)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(resp) => resp.json().await.unwrap_or_else(|err| {
            eprintln!("parsing tugboat /status failed: {err}");
            Vec::new()
        }),
        Err(err) => {
            eprintln!("tugboat /status unreachable: {err}");
            Vec::new()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeployedInfo {
    short: String,
    /// GitHub link to this commit, when the repo is on GitHub.
    commit_url: Option<String>,
    dirty: bool,
    deployed_at: u64,
}

#[derive(Debug, Serialize)]
pub struct LocalInfo {
    branch: Option<String>,
    head_short: Option<String>,
    dirty: bool,
    undeployed_commits: Option<u32>,
}

/// Decide the freshness verdict from the deployed entry and live local state.
fn verdict(deployed: Option<&LedgerEntry>, local: &DaemonStatus) -> &'static str {
    let Some(deployed) = deployed else {
        // Deployable, but never recorded (deployed before this feature, or only
        // ever by some other path) — don't guess.
        return "unknown";
    };
    let Some(head) = &local.head_sha else {
        return "unknown";
    };
    if *head == deployed.sha {
        if local.dirty { "dirty" } else { "current" }
    } else if local.deployed_is_ancestor == Some(true) {
        // The branch moved on, but the daemon scopes the count to commits that
        // touch this member (its directory + the fleet's shared paths). Zero
        // relevant commits means the running build is still current for this
        // service; only unscoped churn happened elsewhere in the monorepo.
        if local.undeployed_commits == Some(0) {
            "current"
        } else {
            "stale"
        }
    } else {
        "diverged"
    }
}

/// `GET /api/deploy-status` — freshness of every deployable service: which sha
/// is running vs the head of origin's default branch. The set of entries is also
/// the set of services that show a Deploy button. Empty (no buttons, no badges)
/// when deploy is unconfigured or the daemon is unreachable.
pub async fn deploy_status(State(state): State<Arc<AppState>>) -> Json<Vec<DeployStatus>> {
    let Some(cfg) = &state.config.deploy else {
        return Json(Vec::new());
    };

    // Monitored units paired with the tugboat name they deploy as, and the
    // currently-running entry from each one's deploy ledger (if any).
    let units: Vec<(String, String, Option<LedgerEntry>)> = monitored(&state.config)
        .await
        .into_iter()
        .map(|m| {
            let deploy_name = state.config.deploy_name(&m.unit);
            let deployed = current_deployed(&read_ledger(&cfg.version_dir, &deploy_name)).cloned();
            (m.unit, deploy_name, deployed)
        })
        .collect();

    // Tell the daemon the deployed shas we know, so it can compute each one's
    // relationship (ahead vs diverged, commit count) against origin's default branch.
    let deployed_param = units
        .iter()
        .filter_map(|(_, name, dep)| dep.as_ref().map(|d| format!("{name}:{}", d.sha)))
        .collect::<Vec<_>>()
        .join(",");

    let statuses = daemon_statuses(&state.http, cfg, &deployed_param).await;
    let by_name: HashSet<&str> = statuses.iter().map(|s| s.name.as_str()).collect();

    let mut out = Vec::new();
    for (unit, deploy_name, deployed) in &units {
        // Deployable iff the daemon lists it as a member.
        if !by_name.contains(deploy_name.as_str()) {
            continue;
        }
        let local = statuses.iter().find(|s| &s.name == deploy_name).unwrap();
        let v = verdict(deployed.as_ref(), local);
        out.push(DeployStatus {
            unit: unit.clone(),
            verdict: v,
            changes_url: changes_url(
                v,
                local.repo_url.as_deref(),
                deployed.as_ref(),
                local.head_short.as_deref(),
            ),
            deployed: deployed.as_ref().map(|d| DeployedInfo {
                short: d.short.clone(),
                commit_url: commit_url(local.repo_url.as_deref(), &d.sha),
                dirty: d.dirty,
                deployed_at: d.at,
            }),
            local: Some(LocalInfo {
                branch: local.branch.clone(),
                head_short: local.head_short.clone(),
                dirty: local.dirty,
                undeployed_commits: local.undeployed_commits,
            }),
        });
    }
    Json(out)
}

/// One deploy in a service's history, for the dashboard.
#[derive(Debug, Serialize)]
pub struct DeployHistoryEntry {
    /// Deploy id; present means a transcript can be fetched at
    /// `/api/services/{unit}/deploy-log/{id}`. `None` for pre-v2 deploys.
    id: Option<String>,
    sha: String,
    short: String,
    /// GitHub link to this commit, when the repo is on GitHub.
    commit_url: Option<String>,
    branch: Option<String>,
    dirty: bool,
    /// `deployed` or `rolled_back`.
    result: String,
    /// Unix epoch seconds.
    at: u64,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    limit: Option<usize>,
}

const DEFAULT_HISTORY_LIMIT: usize = 20;
const MAX_HISTORY_LIMIT: usize = 200;

/// `GET /api/services/{unit}/deploy-history?limit=N` — recent deploys of a
/// service, newest first. Empty when deploy is unconfigured or none recorded.
pub async fn deploy_history(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<DeployHistoryEntry>>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let Some(cfg) = &state.config.deploy else {
        return Ok(Json(Vec::new()));
    };

    let deploy_name = state.config.deploy_name(&svc.unit);
    let mut entries = read_ledger(&cfg.version_dir, &deploy_name);
    entries.reverse(); // newest first
    let limit = query.limit.unwrap_or(DEFAULT_HISTORY_LIMIT).min(MAX_HISTORY_LIMIT);
    entries.truncate(limit);

    // Each commit links to GitHub, but the repo's web URL lives only in the
    // daemon's status — fetch it. Best-effort: an asleep daemon just means the
    // history renders without commit links, exactly as before this feature.
    let repo_url = daemon_statuses(&state.http, cfg, "")
        .await
        .into_iter()
        .find(|s| s.name == deploy_name)
        .and_then(|s| s.repo_url);

    let history = entries
        .into_iter()
        .map(|e| DeployHistoryEntry {
            commit_url: commit_url(repo_url.as_deref(), &e.sha),
            id: e.id,
            sha: e.sha,
            short: e.short,
            branch: e.branch,
            dirty: e.dirty,
            result: e.result.as_str().to_owned(),
            at: e.at,
        })
        .collect();
    Ok(Json(history))
}

/// The tugboat daemon's `/changelog` payload: the commits a deploy shipped, plus
/// the repo's web base so each can be linked.
#[derive(Debug, Deserialize)]
struct DaemonChangelog {
    #[serde(default)]
    repo_url: Option<String>,
    commits: Vec<DaemonCommit>,
}

#[derive(Debug, Deserialize)]
struct DaemonCommit {
    sha: String,
    short: String,
    subject: String,
    at: u64,
}

/// One commit in a deploy's changelog, for the dashboard.
#[derive(Debug, Serialize)]
pub struct ChangelogCommit {
    short: String,
    subject: String,
    /// GitHub link to this commit, when the repo is on GitHub.
    commit_url: Option<String>,
    /// Unix epoch seconds.
    at: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChangelogQuery {
    /// The previously-deployed sha (base of the range).
    from: String,
    /// The sha this deploy shipped (tip of the range).
    to: String,
}

/// Whether a string is a plausible git sha (non-empty hex, no longer than a full
/// sha) — rejecting junk before it reaches the daemon's git argv.
fn is_sha(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `GET /api/services/{unit}/changelog?from=<sha>&to=<sha>` — the commits one
/// deploy shipped (the range from the previously-deployed sha to the one it
/// shipped), newest first. The git history lives on the dev box, so this proxies
/// to the tugboat daemon and links each commit. `502` when the daemon is
/// unreachable (the build host is asleep); `404` for an unknown unit or shas the
/// daemon can't resolve; `400` for a malformed sha.
pub async fn changelog(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
    Query(query): Query<ChangelogQuery>,
) -> Result<Json<Vec<ChangelogCommit>>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let Some(cfg) = &state.config.deploy else {
        return Err(StatusCode::NOT_FOUND);
    };
    if !is_sha(&query.from) || !is_sha(&query.to) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let deploy_name = state.config.deploy_name(&svc.unit);
    let url = format!("{}/changelog/{}", cfg.tugboat_url.trim_end_matches('/'), deploy_name);
    let resp = state
        .http
        .get(&url)
        .query(&[("from", &query.from), ("to", &query.to)])
        .bearer_auth(&cfg.token)
        .send()
        .await
        .map_err(|err| {
            eprintln!("tugboat /changelog unreachable: {err}");
            StatusCode::BAD_GATEWAY
        })?;

    // A 404 from the daemon (unknown member or unresolvable sha) is a real
    // not-found, distinct from the daemon being down — pass it through as such.
    if resp.status().as_u16() == 404 {
        return Err(StatusCode::NOT_FOUND);
    }
    let daemon: DaemonChangelog = resp
        .error_for_status()
        .map_err(|err| {
            eprintln!("tugboat /changelog error: {err}");
            StatusCode::BAD_GATEWAY
        })?
        .json()
        .await
        .map_err(|err| {
            eprintln!("parsing tugboat /changelog failed: {err}");
            StatusCode::BAD_GATEWAY
        })?;

    let commits = daemon
        .commits
        .into_iter()
        .map(|c| ChangelogCommit {
            commit_url: commit_url(daemon.repo_url.as_deref(), &c.sha),
            short: c.short,
            subject: c.subject,
            at: c.at,
        })
        .collect();
    Ok(Json(commits))
}

/// Default history window if none is given: the last 24 hours.
const DEFAULT_HISTORY_WINDOW_SECS: i64 = 86_400;
/// Smallest and largest windows the API will serve.
const MIN_HISTORY_WINDOW_SECS: i64 = 3_600;
const MAX_HISTORY_WINDOW_SECS: i64 = 30 * 86_400;
/// Fixed number of timeline buckets — the status-bar resolution, independent of
/// window length so the chart is a constant width and any downtime in a bucket
/// shows (a bucket is "down" if any sample in it was).
const TIMELINE_BUCKETS: usize = 96;

#[derive(Debug, Deserialize)]
pub struct HealthQuery {
    window_secs: Option<i64>,
}

/// A service's health over a window: a per-bucket timeline plus rollup figures.
#[derive(Debug, Serialize)]
pub struct HealthHistory {
    window_secs: i64,
    now: i64,
    interval_secs: u64,
    summary: HealthSummary,
    buckets: Vec<Bucket>,
}

#[derive(Debug, Serialize)]
pub struct HealthSummary {
    sample_count: usize,
    /// Percent of samples whose systemd state was `active` (0–100).
    systemd_uptime_pct: f64,
    /// Whether this service is probed at all (it has a public URL).
    probed: bool,
    /// Percent of probed samples reachable through breakwater (0–100). `None`
    /// when the service isn't probed.
    probe_uptime_pct: Option<f64>,
    /// Current reachability and when that state began. `None` when not probed.
    current: Option<CurrentProbe>,
    memory_current: Option<i64>,
    memory_peak: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CurrentProbe {
    ok: bool,
    status: Option<u16>,
    ms: Option<u32>,
    /// Unix seconds when the current reachable/unreachable state began.
    since: i64,
}

/// One timeline cell. `status` is the worst observation in the cell so an outage
/// is never hidden by averaging.
#[derive(Debug, Serialize)]
pub struct Bucket {
    at: i64,
    /// `up` | `unreachable` | `down` | `gap` (no samples collected).
    status: &'static str,
    /// Peak memory in the cell, for the resource sparkline.
    memory_bytes: Option<i64>,
}

/// `GET /api/services/{unit}/history?window_secs=N` — the service's recorded
/// health over the window: a bucketed up/unreachable/down timeline, reachability
/// and systemd uptime percentages, and a memory series. `503` when history
/// collection is disabled or its database couldn't be opened.
pub async fn health_history(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
    Query(query): Query<HealthQuery>,
) -> Result<Json<HealthHistory>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let Some(store) = &state.history else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let window = query
        .window_secs
        .unwrap_or(DEFAULT_HISTORY_WINDOW_SECS)
        .clamp(MIN_HISTORY_WINDOW_SECS, MAX_HISTORY_WINDOW_SECS);
    let now = now_unix();
    let since = now - window;

    let samples = store.window(&svc.unit, since).map_err(|err| {
        eprintln!("history query for {} failed: {err:#}", svc.unit);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(HealthHistory {
        window_secs: window,
        now,
        interval_secs: state.config.history.interval_secs,
        summary: summarize(&samples),
        buckets: bucketize(&samples, since, now, TIMELINE_BUCKETS),
    }))
}

/// Roll a window of samples up into the summary figures.
fn summarize(samples: &[Sample]) -> HealthSummary {
    let n = samples.len();
    let active = samples.iter().filter(|s| s.active_state == "active").count();
    let systemd_uptime_pct = pct(active, n);

    let probed: Vec<&Sample> = samples.iter().filter(|s| s.probe.is_some()).collect();
    let probe_uptime_pct = (!probed.is_empty()).then(|| {
        let ok = probed.iter().filter(|s| s.probe.as_ref().unwrap().ok).count();
        pct(ok, probed.len())
    });

    HealthSummary {
        sample_count: n,
        systemd_uptime_pct,
        probed: !probed.is_empty(),
        probe_uptime_pct,
        current: current_probe(samples),
        memory_current: samples.last().and_then(|s| s.memory_bytes),
        memory_peak: samples.iter().filter_map(|s| s.memory_bytes).max(),
    }
}

/// The most recent probe and how long the current reachable/unreachable state has
/// held — walking back while the `ok` value is unchanged (samples with no probe
/// don't break the run).
fn current_probe(samples: &[Sample]) -> Option<CurrentProbe> {
    let latest = samples.iter().rev().find_map(|s| s.probe.as_ref().map(|p| (s.at, p)))?;
    let (_, probe) = latest;
    let ok = probe.ok;
    let mut since = latest.0;
    for s in samples.iter().rev() {
        match &s.probe {
            Some(p) if p.ok == ok => since = s.at,
            Some(_) => break,
            None => continue,
        }
    }
    Some(CurrentProbe { ok, status: probe.status, ms: probe.ms, since })
}

/// Distribute samples into `k` fixed time buckets across `[since, now]`, each
/// taking the worst status it saw and its peak memory.
fn bucketize(samples: &[Sample], since: i64, now: i64, k: usize) -> Vec<Bucket> {
    let span = (now - since).max(1);
    let mut buckets: Vec<Bucket> = (0..k)
        .map(|i| Bucket {
            at: since + span * i as i64 / k as i64,
            status: "gap",
            memory_bytes: None,
        })
        .collect();

    for s in samples {
        let idx = (((s.at - since) * k as i64) / span).clamp(0, k as i64 - 1) as usize;
        let bucket = &mut buckets[idx];
        let observed = if s.active_state != "active" {
            "down"
        } else if s.probe.as_ref().is_some_and(|p| !p.ok) {
            "unreachable"
        } else {
            "up"
        };
        bucket.status = worse(bucket.status, observed);
        if let Some(mem) = s.memory_bytes {
            bucket.memory_bytes = Some(bucket.memory_bytes.map_or(mem, |cur| cur.max(mem)));
        }
    }
    buckets
}

/// The more-severe of two bucket statuses (`down` > `unreachable` > `up` > `gap`).
fn worse(a: &'static str, b: &'static str) -> &'static str {
    fn rank(s: &str) -> u8 {
        match s {
            "down" => 3,
            "unreachable" => 2,
            "up" => 1,
            _ => 0,
        }
    }
    if rank(b) > rank(a) { b } else { a }
}

/// A count as a percentage of a total, guarding the empty case.
fn pct(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 * 100.0 / total as f64
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The full transcript of a past deploy, newline-split into lines.
#[derive(Debug, Serialize)]
pub struct DeployLog {
    lines: Vec<String>,
}

/// A valid deploy id (the shared contract's format) is also path-traversal
/// safe — no `/`, no `.` — so it can name a file directly.
fn valid_log_id(id: &str) -> bool {
    tugboat_ledger::valid_deploy_id(id)
}

/// `GET /api/services/{unit}/deploy-log/{id}` — the saved transcript of a past
/// deploy, read from tugboat's on-host file `<version_dir>/<name>/<id>.log`.
/// `404` if the unit is unknown, deploy is unconfigured, the id is malformed, or
/// no transcript exists (e.g. a pre-v2 deploy).
pub async fn deploy_log(
    State(state): State<Arc<AppState>>,
    Path((unit, id)): Path<(String, String)>,
) -> Result<Json<DeployLog>, StatusCode> {
    let svc = resolve(&state.config, &unit).await?;
    let Some(cfg) = &state.config.deploy else {
        return Err(StatusCode::NOT_FOUND);
    };
    if !valid_log_id(&id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let deploy_name = state.config.deploy_name(&svc.unit);
    let path = tugboat_ledger::transcript_path(&cfg.version_dir, &deploy_name, &id);
    let text = std::fs::read_to_string(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    let lines = text.lines().map(str::to_owned).collect();
    Ok(Json(DeployLog { lines }))
}

/// `POST /api/services/{unit}/deploy` — relay a deploy request to the tugboat
/// daemon. The daemon's response (a `{job_id}` on success, or an error status)
/// is passed through unchanged; the token never leaves this server.
pub async fn deploy_service(
    State(state): State<Arc<AppState>>,
    Path(unit): Path<String>,
) -> Response {
    let svc = match resolve(&state.config, &unit).await {
        Ok(svc) => svc,
        Err(code) => return (code, "unknown service").into_response(),
    };
    let Some(cfg) = &state.config.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy integration not configured").into_response();
    };

    let name = state.config.deploy_name(&svc.unit);
    let url = format!("{}/deploy/{}", cfg.tugboat_url.trim_end_matches('/'), name);
    let resp = match state.http.post(&url).bearer_auth(&cfg.token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("deploy relay to {url} failed: {err}");
            return (StatusCode::BAD_GATEWAY, format!("tugboat daemon unreachable: {err}"))
                .into_response();
        }
    };

    let status = resp.status();
    match resp.bytes().await {
        Ok(body) => (status, [(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Err(err) => {
            (StatusCode::BAD_GATEWAY, format!("reading tugboat response: {err}")).into_response()
        }
    }
}

/// `GET /api/services/{unit}/deploy/{job}/stream` — proxy the daemon's live
/// deploy transcript (Server-Sent Events) straight through to the browser.
pub async fn deploy_stream(
    State(state): State<Arc<AppState>>,
    Path((unit, job)): Path<(String, String)>,
) -> Response {
    if let Err(code) = resolve(&state.config, &unit).await {
        return (code, "unknown service").into_response();
    }
    let Some(cfg) = &state.config.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy integration not configured").into_response();
    };

    let url = format!("{}/jobs/{}/stream", cfg.tugboat_url.trim_end_matches('/'), job);
    let resp = match state.http.get(&url).bearer_auth(&cfg.token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            return (StatusCode::BAD_GATEWAY, format!("tugboat daemon unreachable: {err}"))
                .into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.bytes().await.unwrap_or_default();
        return (status, body).into_response();
    }

    let body = Body::from_stream(resp.bytes_stream());
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        bucketize, changes_url, commit_url, current_deployed, current_probe, summarize,
        valid_log_id, worse,
    };
    use crate::store::{Probe, Sample};
    use tugboat_ledger::{LedgerEntry, LedgerResult};

    fn sample(at: i64, state: &str, mem: Option<i64>, probe: Option<Probe>) -> Sample {
        Sample { at, active_state: state.into(), memory_bytes: mem, probe }
    }

    fn ok_probe() -> Probe {
        Probe { ok: true, status: Some(200), ms: Some(12) }
    }
    fn bad_probe() -> Probe {
        Probe { ok: false, status: None, ms: None }
    }

    #[test]
    fn summary_counts_uptime_and_reachability() {
        let samples = vec![
            sample(10, "active", Some(100), Some(ok_probe())),
            sample(20, "active", Some(200), Some(bad_probe())), // up by systemd, unreachable
            sample(30, "failed", Some(150), Some(bad_probe())),
            sample(40, "active", Some(120), Some(ok_probe())),
        ];
        let s = summarize(&samples);
        assert_eq!(s.sample_count, 4);
        assert_eq!(s.systemd_uptime_pct, 75.0); // 3 of 4 active
        assert!(s.probed);
        assert_eq!(s.probe_uptime_pct, Some(50.0)); // 2 of 4 reachable
        assert_eq!(s.memory_current, Some(120)); // last
        assert_eq!(s.memory_peak, Some(200));
    }

    #[test]
    fn summary_handles_unprobed_service() {
        let samples = vec![sample(10, "active", Some(100), None)];
        let s = summarize(&samples);
        assert!(!s.probed);
        assert_eq!(s.probe_uptime_pct, None);
        assert!(s.current.is_none());
    }

    #[test]
    fn current_probe_tracks_state_start() {
        // Reachable, then a run of unreachable; `since` is the start of the run,
        // and a sample with no probe in the middle doesn't break it.
        let samples = vec![
            sample(10, "active", None, Some(ok_probe())),
            sample(20, "active", None, Some(bad_probe())), // unreachable begins here
            sample(30, "active", None, None),              // no probe — skipped
            sample(40, "active", None, Some(bad_probe())),
        ];
        let current = current_probe(&samples).unwrap();
        assert!(!current.ok);
        assert_eq!(current.since, 20);
    }

    #[test]
    fn buckets_take_worst_status_and_peak_memory() {
        // One bucket spanning [0,100): an unreachable sample then a down sample —
        // the cell must read as the worse of the two (down), with peak memory.
        let samples = vec![
            sample(10, "active", Some(100), Some(bad_probe())),
            sample(20, "failed", Some(300), Some(bad_probe())),
        ];
        let buckets = bucketize(&samples, 0, 100, 1);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].status, "down");
        assert_eq!(buckets[0].memory_bytes, Some(300));

        // No samples in a window → a single "gap" cell.
        assert_eq!(bucketize(&[], 0, 100, 1)[0].status, "gap");
    }

    #[test]
    fn worse_orders_statuses() {
        assert_eq!(worse("up", "down"), "down");
        assert_eq!(worse("unreachable", "up"), "unreachable");
        assert_eq!(worse("gap", "up"), "up");
        assert_eq!(worse("down", "unreachable"), "down");
    }

    fn deployed_entry(short: &str) -> LedgerEntry {
        LedgerEntry {
            v: tugboat_ledger::LEDGER_VERSION,
            id: Some(format!("1-{short}")),
            sha: format!("{short}00000000000000000000000000000000"),
            short: short.to_owned(),
            dirty: false,
            branch: Some("main".into()),
            result: LedgerResult::Deployed,
            at: 1,
        }
    }

    #[test]
    fn indeterminate_recovery_hides_stale_deployed_identity() {
        let deployed = deployed_entry("deadbeef");
        let mut indeterminate = deployed_entry("cafebabe");
        indeterminate.result = LedgerResult::Indeterminate;
        assert!(current_deployed(&[deployed, indeterminate]).is_none());
    }

    #[test]
    fn changes_url_links_undeployed_commits() {
        let entry = deployed_entry("deadbeef");
        let repo = Some("https://github.com/deepwa7er/lagoon");

        // Stale → a GitHub compare from the deployed sha to local HEAD.
        assert_eq!(
            changes_url("stale", repo, Some(&entry), Some("cafebabe")),
            Some("https://github.com/deepwa7er/lagoon/compare/deadbeef...cafebabe".into()),
        );
        // Diverged also has committed undeployed work, so it links too.
        assert!(changes_url("diverged", repo, Some(&entry), Some("cafebabe")).is_some());
        // A trailing slash on the repo base is normalized away.
        assert_eq!(
            changes_url(
                "stale",
                Some("https://github.com/deepwa7er/lagoon/"),
                Some(&entry),
                Some("cafebabe"),
            ),
            Some("https://github.com/deepwa7er/lagoon/compare/deadbeef...cafebabe".into()),
        );
        // No link when up to date, off GitHub, or missing a sha.
        assert_eq!(changes_url("current", repo, Some(&entry), Some("cafebabe")), None);
        assert_eq!(changes_url("dirty", repo, Some(&entry), Some("cafebabe")), None);
        assert_eq!(changes_url("stale", None, Some(&entry), Some("cafebabe")), None);
        assert_eq!(changes_url("stale", repo, None, Some("cafebabe")), None);
        assert_eq!(changes_url("stale", repo, Some(&entry), None), None);
    }

    #[test]
    fn commit_url_links_a_single_commit() {
        let repo = Some("https://github.com/deepwa7er/lagoon");
        // The full sha is used in the href, even though the UI shows the short form.
        assert_eq!(
            commit_url(repo, "deadbeef00000000000000000000000000000000"),
            Some(
                "https://github.com/deepwa7er/lagoon/commit/deadbeef00000000000000000000000000000000"
                    .into()
            ),
        );
        // A trailing slash on the repo base is normalized away.
        assert_eq!(
            commit_url(Some("https://github.com/deepwa7er/lagoon/"), "cafebabe"),
            Some("https://github.com/deepwa7er/lagoon/commit/cafebabe".into()),
        );
        // Off GitHub (no repo URL) → no link.
        assert_eq!(commit_url(None, "deadbeef"), None);
    }

    #[test]
    fn accepts_real_deploy_ids() {
        assert!(valid_log_id("1718900000-1a2b3c4d"));
        assert!(valid_log_id("42-nogit"));
    }

    #[test]
    fn rejects_traversal_and_malformed_ids() {
        // Path traversal and absolute paths must never read outside the dir.
        assert!(!valid_log_id("../../etc/passwd"));
        assert!(!valid_log_id("1718900000-../secret"));
        assert!(!valid_log_id("/etc/passwd"));
        assert!(!valid_log_id("1718900000-1a2b/3c4d"));
        // Structurally wrong: no dash, empty halves, uppercase, dots.
        assert!(!valid_log_id("nodash"));
        assert!(!valid_log_id("-nogit"));
        assert!(!valid_log_id("1718900000-"));
        assert!(!valid_log_id("1718900000-ABCD"));
        assert!(!valid_log_id("abc-1a2b3c4d"));
        assert!(!valid_log_id("1718900000-1a2b.log"));
    }
}
