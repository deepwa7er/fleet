//! Local deploy events: one line per deploy attempt, on the machine that ran it.
//!
//! This is deliberately *not* the host ledger (see the `tugboat-ledger` crate).
//! The two answer different questions and have different durability needs:
//!
//! - **The ledger** is the durable, per-host answer to "what is this service
//!   running right now". It is written inside the same atomic remote transaction
//!   as the install, so a dashboard can never disagree with the box. It records
//!   *what shipped*, and its entry is composed before the deploy runs — which is
//!   exactly why nothing measured *during* a deploy can live there.
//! - **This file** is the analytics record: how the deploy went. Timing
//!   breakdown, and the failures that never reached the host at all — a build
//!   that didn't compile, an artifact the build didn't produce. Losing a line
//!   here costs a row in a chart, not the truth about what is running.
//!
//! Nothing reads it live. It is an emitter in the same shape as breakwater's
//! access log: emit the fact now, let the warehouse ingest the file locally
//! whenever it cares to. (An HTTP forward to depot existed until depot was
//! archived, 2026-08-15; the local file was always the durable record.)
//!
//! Location: `${XDG_DATA_HOME:-$HOME/.local/share}/tugboat/deploys.jsonl`,
//! appended one line at a time (`O_APPEND`, one short line per deploy, so an
//! interrupted or concurrent write cannot tear an entry).

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::subprocess::LogSink;

/// Current event schema version. Bump when the shape changes; readers should
/// ignore versions they don't know rather than misread them.
pub const EVENT_VERSION: u32 = 1;

/// Where a deploy ended. `Deployed` is the only success.
///
/// Note what is deliberately absent: **`rolled_back`**. When the remote
/// transaction fails, tugboat sees only a non-zero exit — it cannot tell a
/// health-check rollback from a failed `sudo` or a dropped ssh connection.
/// Claiming a rollback here would be a guess. The host ledger *does* know (it
/// writes `rolled_back` from inside the rollback branch), so a reader that wants
/// rollback rate joins these events to the ledger on `at`, which both sides
/// stamp from the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Build,
    Artifacts,
    Ship,
    Install,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::Build => "build",
            Stage::Artifacts => "artifacts",
            Stage::Ship => "ship",
            Stage::Install => "install",
        }
    }
}

/// One deploy attempt.
#[derive(Debug, Serialize)]
pub struct DeployEvent {
    v: u32,
    /// Deploy start, Unix epoch seconds. Shared verbatim with the host ledger's
    /// `at` and with the transcript id, so the three join.
    at: u64,
    name: String,
    host: String,
    /// `working_tree` or `default_branch`.
    source: &'static str,
    /// Absent when the project is not a git checkout.
    #[serde(skip_serializing_if = "Option::is_none")]
    sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    short: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    /// Whether the built tree had uncommitted changes.
    dirty: bool,
    /// `deployed` or `failed`. See [`Stage`] on why `rolled_back` is not here.
    result: &'static str,
    /// The stage that ended a failed deploy. Absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<&'static str>,
    /// The failure, first line only. Absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Local build. Absent when the build was skipped (`--skip-build`).
    #[serde(skip_serializing_if = "Option::is_none")]
    build_ms: Option<u64>,
    /// rsync of every artifact to the host.
    #[serde(skip_serializing_if = "Option::is_none")]
    ship_ms: Option<u64>,
    /// The remote transaction: atomic swap, restart, health-check, and (on
    /// failure) the host's rollback. A service getting slower to come up shows
    /// up here.
    #[serde(skip_serializing_if = "Option::is_none")]
    install_ms: Option<u64>,
    /// Everything, including source prep (fetch + worktree checkout) — what a
    /// human actually waited for.
    total_ms: u64,
}

/// Accumulates timings across a deploy so an event can be emitted on every exit
/// path, success or failure.
pub struct Recorder {
    at: u64,
    started: Instant,
    name: String,
    host: String,
    source: &'static str,
    sha: Option<String>,
    short: Option<String>,
    branch: Option<String>,
    dirty: bool,
    /// The stage currently running — reported as the failure site if the deploy
    /// ends here.
    stage: Stage,
    build_ms: Option<u64>,
    ship_ms: Option<u64>,
    install_ms: Option<u64>,
}

impl Recorder {
    pub fn new(at: u64, name: &str, host: &str, source: &'static str) -> Self {
        Self {
            at,
            started: Instant::now(),
            name: name.to_owned(),
            host: host.to_owned(),
            source,
            sha: None,
            short: None,
            branch: None,
            dirty: false,
            stage: Stage::Build,
            build_ms: None,
            ship_ms: None,
            install_ms: None,
        }
    }

    /// Record which commit the deploy resolved to, once source prep has run.
    pub fn stamped(&mut self, sha: &str, short: &str, branch: Option<&str>, dirty: bool) {
        self.sha = Some(sha.to_owned());
        self.short = Some(short.to_owned());
        self.branch = branch.map(str::to_owned);
        self.dirty = dirty;
    }

    /// Mark the stage about to run, so a failure is attributed to it.
    pub fn entering(&mut self, stage: Stage) {
        self.stage = stage;
    }

    /// Store a completed stage's duration.
    pub fn completed(&mut self, stage: Stage, elapsed: Instant) {
        let ms = elapsed.elapsed().as_millis() as u64;
        match stage {
            Stage::Build => self.build_ms = Some(ms),
            Stage::Ship => self.ship_ms = Some(ms),
            Stage::Install => self.install_ms = Some(ms),
            // Verifying artifacts exist is a stat() per artifact; timing it
            // would measure nothing.
            Stage::Artifacts => {}
        }
    }

    /// Build the event for a finished deploy.
    pub fn finish(self, outcome: &Result<()>) -> DeployEvent {
        let (result, stage, error) = match outcome {
            Ok(()) => ("deployed", None, None),
            Err(err) => (
                "failed",
                Some(self.stage.as_str()),
                // First line only: the full chain is already in the transcript,
                // and a multi-line string would be unreadable in a table.
                Some(format!("{err}").lines().next().unwrap_or("").to_owned()),
            ),
        };
        DeployEvent {
            v: EVENT_VERSION,
            at: self.at,
            name: self.name,
            host: self.host,
            source: self.source,
            sha: self.sha,
            short: self.short,
            branch: self.branch,
            dirty: self.dirty,
            result,
            stage,
            error,
            build_ms: self.build_ms,
            ship_ms: self.ship_ms,
            install_ms: self.install_ms,
            total_ms: self.started.elapsed().as_millis() as u64,
        }
    }
}

/// The deploy event log's path, honoring `XDG_DATA_HOME`.
pub fn log_path() -> Result<PathBuf> {
    resolve_log_path(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// The path resolution itself, as a pure function of the two environment
/// variables — so it is testable without mutating the process environment
/// (which races with every other test in this binary).
fn resolve_log_path(xdg_data_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let base = match xdg_data_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home =
                home.context("no HOME or XDG_DATA_HOME to locate the tugboat data dir")?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(base.join("tugboat").join("deploys.jsonl"))
}

/// Record one event: append it to the local JSONL.
///
/// Best-effort by design — a deploy's outcome must never change because an
/// analytics write failed.
pub fn record(event: &DeployEvent, log: &dyn LogSink) {
    if let Err(err) = try_append(event) {
        log.line(&format!("    warning: could not record deploy event: {err:#}"));
    }
}

fn try_append(event: &DeployEvent) -> Result<()> {
    let path = log_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut line = serde_json::to_string(event).context("serializing deploy event")?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> Recorder {
        let mut rec = Recorder::new(1_718_900_000, "clothes", "deepwa7er", "default_branch");
        rec.stamped("aaaabbbbccccdddd", "aaaabbbb", Some("main"), false);
        rec
    }

    fn json(event: &DeployEvent) -> serde_json::Value {
        serde_json::from_str(&serde_json::to_string(event).unwrap()).unwrap()
    }

    #[test]
    fn success_records_the_full_shape() {
        let mut rec = recorder();
        rec.build_ms = Some(37_800);
        rec.ship_ms = Some(1_200);
        rec.install_ms = Some(2_400);
        let value = json(&rec.finish(&Ok(())));

        assert_eq!(value["v"], EVENT_VERSION);
        assert_eq!(value["at"], 1_718_900_000u64);
        assert_eq!(value["name"], "clothes");
        assert_eq!(value["host"], "deepwa7er");
        assert_eq!(value["source"], "default_branch");
        assert_eq!(value["short"], "aaaabbbb");
        assert_eq!(value["branch"], "main");
        assert_eq!(value["dirty"], false);
        assert_eq!(value["result"], "deployed");
        assert_eq!(value["build_ms"], 37_800);
        assert_eq!(value["install_ms"], 2_400);
        // A successful deploy carries no failure fields at all.
        assert!(value.get("stage").is_none());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn failure_is_attributed_to_the_stage_that_was_running() {
        let mut rec = recorder();
        rec.entering(Stage::Build);
        let value = json(&rec.finish(&Err(anyhow::anyhow!("build failed"))));
        assert_eq!(value["result"], "failed");
        assert_eq!(value["stage"], "build");
        assert_eq!(value["error"], "build failed");

        let mut rec = recorder();
        rec.entering(Stage::Install);
        let value = json(&rec.finish(&Err(anyhow::anyhow!("remote install failed"))));
        assert_eq!(value["stage"], "install");
    }

    #[test]
    fn error_is_reduced_to_one_line() {
        // A newline would split one event across two lines and make both
        // unparseable; the full chain lives in the transcript anyway.
        let mut rec = recorder();
        rec.entering(Stage::Ship);
        let err = anyhow::anyhow!("rsync failed\n  caused by: connection reset");
        let event = rec.finish(&Err(err));
        let line = serde_json::to_string(&event).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(json(&event)["error"], "rsync failed");
    }

    #[test]
    fn a_non_git_deploy_omits_commit_fields() {
        let rec = Recorder::new(42, "tidepool", "deepwa7er", "working_tree");
        let value = json(&rec.finish(&Ok(())));
        assert!(value.get("sha").is_none());
        assert!(value.get("short").is_none());
        assert!(value.get("branch").is_none());
    }

    #[test]
    fn skipped_build_records_no_build_time() {
        // --skip-build must read as "no build happened", not "the build took 0ms".
        let rec = recorder();
        let value = json(&rec.finish(&Ok(())));
        assert!(value.get("build_ms").is_none());
        assert!(value["total_ms"].as_u64().is_some());
    }

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn xdg_data_home_wins_when_set() {
        assert_eq!(
            resolve_log_path(os("/xdg/data"), os("/home/u")).unwrap(),
            PathBuf::from("/xdg/data/tugboat/deploys.jsonl")
        );
    }

    #[test]
    fn falls_back_to_home_when_xdg_is_unset_or_empty() {
        let expected = PathBuf::from("/home/u/.local/share/tugboat/deploys.jsonl");
        assert_eq!(resolve_log_path(None, os("/home/u")).unwrap(), expected);
        // An empty XDG_DATA_HOME is unset, per the XDG spec — not a path of "".
        assert_eq!(resolve_log_path(os(""), os("/home/u")).unwrap(), expected);
    }

    #[test]
    fn no_home_and_no_xdg_is_an_error_not_a_relative_path() {
        assert!(resolve_log_path(None, None).is_err());
    }
}
