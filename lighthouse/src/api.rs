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

/// One entry in tugboat's per-service deploy ledger (`<name>.jsonl`).
#[derive(Debug, Deserialize, Clone)]
struct LedgerEntry {
    /// Schema version; ignored for now but lets the format evolve.
    #[serde(default)]
    #[allow(dead_code)]
    v: u32,
    /// Deploy id (`{at}-{short}`), naming the transcript file `<name>/<id>.log`.
    /// Absent on pre-v2 entries, which have no saved transcript.
    #[serde(default)]
    id: Option<String>,
    sha: String,
    short: String,
    #[serde(default)]
    dirty: bool,
    branch: Option<String>,
    /// `deployed` (it came up healthy) or `rolled_back` (it didn't).
    result: String,
    /// Unix epoch seconds.
    at: u64,
}

/// Read a service's deploy ledger, oldest entry first. Missing/unreadable → empty.
fn read_ledger(version_dir: &std::path::Path, deploy_name: &str) -> Vec<LedgerEntry> {
    let path = version_dir.join(format!("{deploy_name}.jsonl"));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The currently-running version: the most recent entry that actually deployed.
/// A trailing `rolled_back` entry means the prior `deployed` one is still live.
fn current_deployed(entries: &[LedgerEntry]) -> Option<&LedgerEntry> {
    entries.iter().rev().find(|e| e.result == "deployed")
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
        "stale"
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
            result: e.result,
            at: e.at,
        })
        .collect();
    Ok(Json(history))
}

/// The full transcript of a past deploy, newline-split into lines.
#[derive(Debug, Serialize)]
pub struct DeployLog {
    lines: Vec<String>,
}

/// A deploy id is `{at}-{short}` — all digits, a dash, then a lowercase
/// alphanumeric sha (or `nogit`). Validating it before touching the filesystem
/// both rejects malformed ids and makes path traversal impossible (no `/`, `.`).
fn valid_log_id(id: &str) -> bool {
    let Some((at, rest)) = id.split_once('-') else {
        return false;
    };
    !at.is_empty()
        && at.bytes().all(|b| b.is_ascii_digit())
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
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
    let path = cfg.version_dir.join(&deploy_name).join(format!("{id}.log"));
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
    use super::{changes_url, commit_url, valid_log_id, LedgerEntry};

    fn deployed_entry(short: &str) -> LedgerEntry {
        LedgerEntry {
            v: 2,
            id: Some(format!("1-{short}")),
            sha: format!("{short}00000000000000000000000000000000"),
            short: short.to_owned(),
            dirty: false,
            branch: Some("main".into()),
            result: "deployed".into(),
            at: 1,
        }
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
