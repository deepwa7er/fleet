//! Local, append-only history of multi-target agent deployment attempts.
//!
//! This is an operational journal, not authoritative target state. The running
//! binaries remain authoritative through their health identity; journal writes
//! are best-effort and can never change a deployment result.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use super::transaction::{Execution, Outcome, StepOutcome};
use super::{location_name, DeploymentPlan};

const JOURNAL_VERSION: u32 = 1;

pub(super) struct Recorder {
    at: Option<u64>,
    started: Instant,
    destination: Destination,
}

enum Destination {
    System,
    #[cfg(test)]
    Path(PathBuf),
}

impl Recorder {
    pub fn start() -> Self {
        Self::new(Destination::System)
    }

    #[cfg(test)]
    pub fn for_path(path: PathBuf) -> Self {
        Self::new(Destination::Path(path))
    }

    fn new(destination: Destination) -> Self {
        Self {
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs()),
            started: Instant::now(),
            destination,
        }
    }

    pub fn record_build_failure(
        &self,
        plan: &DeploymentPlan<'_>,
        artifact_hashes: &[String],
        failed_index: usize,
        error: &anyhow::Error,
    ) {
        let record = self
            .at
            .context("system clock is before the Unix epoch")
            .and_then(|at| {
                build_failure_record(
                    at,
                    self.started.elapsed(),
                    plan,
                    artifact_hashes,
                    failed_index,
                    error,
                )
            });
        self.record(record);
    }

    pub fn record_transaction(
        &self,
        plan: &DeploymentPlan<'_>,
        build_elapsed: Duration,
        transaction_elapsed: Duration,
        execution: &Execution,
    ) {
        let record = self
            .at
            .context("system clock is before the Unix epoch")
            .and_then(|at| {
                transaction_record(
                    at,
                    self.started.elapsed(),
                    build_elapsed,
                    transaction_elapsed,
                    plan,
                    execution,
                )
            });
        self.record(record);
    }

    fn record(&self, record: Result<JournalRecord>) {
        let result = record.and_then(|record| match &self.destination {
            Destination::System => log_path().and_then(|path| append_record(&path, &record)),
            #[cfg(test)]
            Destination::Path(path) => append_record(path, &record),
        });
        if let Err(error) = result {
            eprintln!("warning: could not record agent deployment journal: {error:#}");
        }
    }
}

pub(super) fn log_path() -> Result<PathBuf> {
    Ok(crate::local_data::tugboat_dir()?.join("agent-deploys.jsonl"))
}

#[derive(Debug, Serialize)]
struct JournalRecord {
    v: u32,
    at: u64,
    transaction: String,
    name: String,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    build_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction_ms: Option<u64>,
    total_ms: u64,
    targets: Vec<TargetRecord>,
}

#[derive(Debug, Serialize)]
struct TargetRecord {
    name: String,
    location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_sha256: Option<String>,
    build: StepRecord,
    prepare: StepRecord,
    activate: StepRecord,
    verify: StepRecord,
    compensate: StepRecord,
    cleanup: StepRecord,
    recovery_preserved: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum StepRecord {
    NotAttempted,
    NotRequired,
    Succeeded,
    Failed { error: String },
    SkippedPreserved,
}

impl From<&StepOutcome> for StepRecord {
    fn from(outcome: &StepOutcome) -> Self {
        match outcome {
            StepOutcome::NotAttempted => Self::NotAttempted,
            StepOutcome::NotRequired => Self::NotRequired,
            StepOutcome::Succeeded => Self::Succeeded,
            StepOutcome::Failed(error) => Self::Failed {
                error: error.clone(),
            },
            StepOutcome::SkippedPreserved => Self::SkippedPreserved,
        }
    }
}

fn build_failure_record(
    at: u64,
    total_elapsed: Duration,
    plan: &DeploymentPlan<'_>,
    artifact_hashes: &[String],
    failed_index: usize,
    error: &anyhow::Error,
) -> Result<JournalRecord> {
    ensure!(
        failed_index < plan.targets.len(),
        "build failure target index is outside the deployment plan"
    );
    ensure!(
        artifact_hashes.len() == failed_index,
        "built artifact hashes do not match the completed build targets"
    );
    let targets = plan
        .targets
        .iter()
        .enumerate()
        .map(|(index, planned)| TargetRecord {
            name: planned.target.name.clone(),
            location: location_name(&planned.target.location).to_owned(),
            artifact_sha256: artifact_hashes.get(index).cloned(),
            build: if index < failed_index {
                StepRecord::Succeeded
            } else if index == failed_index {
                StepRecord::Failed {
                    error: format!("{error:#}"),
                }
            } else {
                StepRecord::NotAttempted
            },
            prepare: StepRecord::NotAttempted,
            activate: StepRecord::NotAttempted,
            verify: StepRecord::NotAttempted,
            compensate: StepRecord::NotRequired,
            cleanup: StepRecord::NotRequired,
            recovery_preserved: false,
        })
        .collect();
    Ok(JournalRecord {
        v: JOURNAL_VERSION,
        at,
        transaction: plan.transaction.clone(),
        name: plan.name.to_owned(),
        result: "build_failed",
        failed_stage: Some("build"),
        error: Some(format!("{error:#}")),
        build_ms: millis(total_elapsed),
        transaction_ms: None,
        total_ms: millis(total_elapsed),
        targets,
    })
}

fn transaction_record(
    at: u64,
    total_elapsed: Duration,
    build_elapsed: Duration,
    transaction_elapsed: Duration,
    plan: &DeploymentPlan<'_>,
    execution: &Execution,
) -> Result<JournalRecord> {
    ensure!(
        plan.targets.len() == execution.report.targets.len(),
        "transaction report targets do not match the deployment plan"
    );
    ensure!(
        plan.targets.len() == execution.artifact_hashes.len(),
        "transaction artifact hashes do not match the deployment plan"
    );
    let targets = plan
        .targets
        .iter()
        .zip(&execution.artifact_hashes)
        .zip(&execution.report.targets)
        .map(|((planned, artifact_hash), report)| TargetRecord {
            name: report.name.clone(),
            location: location_name(&planned.target.location).to_owned(),
            artifact_sha256: Some(artifact_hash.clone()),
            build: StepRecord::Succeeded,
            prepare: (&report.prepare).into(),
            activate: (&report.activate).into(),
            verify: (&report.verify).into(),
            compensate: (&report.compensate).into(),
            cleanup: (&report.cleanup).into(),
            recovery_preserved: report.recovery_preserved,
        })
        .collect();
    Ok(JournalRecord {
        v: JOURNAL_VERSION,
        at,
        transaction: plan.transaction.clone(),
        name: plan.name.to_owned(),
        result: outcome_name(execution.report.outcome),
        failed_stage: failed_stage(&execution.report),
        error: execution.error.as_ref().map(|error| format!("{error:#}")),
        build_ms: millis(build_elapsed),
        transaction_ms: Some(millis(transaction_elapsed)),
        total_ms: millis(total_elapsed),
        targets,
    })
}

fn failed_stage(report: &super::transaction::Report) -> Option<&'static str> {
    match report.outcome {
        Outcome::Deployed => None,
        Outcome::PreparationFailed => Some("prepare"),
        Outcome::Compensated | Outcome::CompensationIncomplete => report
            .targets
            .iter()
            .any(|target| matches!(target.verify, StepOutcome::Failed(_)))
            .then_some("verify")
            .or(Some("activate")),
        Outcome::DeployedCleanupIncomplete => Some("cleanup"),
    }
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Deployed => "deployed",
        Outcome::PreparationFailed => "preparation_failed",
        Outcome::Compensated => "compensated",
        Outcome::CompensationIncomplete => "compensation_incomplete",
        Outcome::DeployedCleanupIncomplete => "deployed_cleanup_incomplete",
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn append_record(path: &Path, record: &JournalRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut line = serde_json::to_vec(record).context("serializing agent deployment journal")?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    file.write_all(&line)
        .with_context(|| format!("appending to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::agent::transaction::{Report, TargetReport};
    use crate::agent::{
        BuildCommand, Destination, HealthSpec, Location, PlannedTarget, Platform, Target,
    };
    use crate::user_service::UserService;

    fn targets() -> Vec<Target> {
        vec![
            Target {
                name: "mac".to_owned(),
                location: Location::Local,
                platform: Platform {
                    os: "darwin".to_owned(),
                    arch: "arm64".to_owned(),
                },
                destination: Destination::Absolute("/tmp/clipd".to_owned()),
                service: UserService::launchd("com.example.clipd".to_owned()).unwrap(),
            },
            Target {
                name: "desktop".to_owned(),
                location: Location::Ssh {
                    host: "desktop".to_owned(),
                },
                platform: Platform {
                    os: "linux".to_owned(),
                    arch: "amd64".to_owned(),
                },
                destination: Destination::HomeRelative(".local/bin/clipd".to_owned()),
                service: UserService::systemd_user("clipd".to_owned()).unwrap(),
            },
        ]
    }

    fn plan_targets(targets: &[Target]) -> Vec<PlannedTarget<'_>> {
        targets
            .iter()
            .enumerate()
            .map(|(index, target)| PlannedTarget {
                target,
                artifact: PathBuf::from(format!("/tmp/artifact-{index}")),
                build: BuildCommand {
                    program: "unused".to_owned(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                },
            })
            .collect()
    }

    fn report(outcome: Outcome) -> Report {
        Report {
            outcome,
            targets: vec![
                TargetReport {
                    name: "mac".to_owned(),
                    prepare: StepOutcome::Succeeded,
                    activate: StepOutcome::Succeeded,
                    verify: StepOutcome::Succeeded,
                    compensate: StepOutcome::Succeeded,
                    cleanup: StepOutcome::Succeeded,
                    recovery_preserved: false,
                },
                TargetReport {
                    name: "desktop".to_owned(),
                    prepare: StepOutcome::Succeeded,
                    activate: StepOutcome::Failed("ssh disconnected".to_owned()),
                    verify: StepOutcome::NotAttempted,
                    compensate: StepOutcome::Failed("restore failed".to_owned()),
                    cleanup: StepOutcome::SkippedPreserved,
                    recovery_preserved: true,
                },
            ],
        }
    }

    fn health_spec() -> HealthSpec {
        HealthSpec {
            args: vec!["health".to_owned()],
            attempts: 3,
            interval: Duration::from_millis(10),
            timeout: Duration::from_millis(100),
        }
    }

    #[test]
    fn transaction_record_preserves_multi_target_recovery_semantics() {
        let targets = targets();
        let planned = plan_targets(&targets);
        let health = health_spec();
        let plan = DeploymentPlan {
            name: "clipd",
            source_dir: Path::new("/source"),
            health: &health,
            transaction: "tx-1".to_owned(),
            targets: planned,
        };
        let execution = Execution {
            report: report(Outcome::CompensationIncomplete),
            artifact_hashes: vec!["aa".repeat(32), "bb".repeat(32)],
            error: Some(anyhow::anyhow!("deployment failed")),
        };

        let record = transaction_record(
            42,
            Duration::from_millis(300),
            Duration::from_millis(100),
            Duration::from_millis(200),
            &plan,
            &execution,
        )
        .unwrap();
        let value = serde_json::to_value(record).unwrap();

        assert_eq!(value["v"], JOURNAL_VERSION);
        assert_eq!(value["transaction"], "tx-1");
        assert_eq!(value["result"], "compensation_incomplete");
        assert_eq!(value["failed_stage"], "activate");
        assert_eq!(value["targets"][0]["artifact_sha256"], "aa".repeat(32));
        assert_eq!(value["targets"][1]["compensate"]["result"], "failed");
        assert_eq!(
            value["targets"][1]["cleanup"]["result"],
            "skipped_preserved"
        );
        assert_eq!(value["targets"][1]["recovery_preserved"], true);
    }

    #[test]
    fn build_failure_marks_later_targets_unattempted() {
        let targets = targets();
        let planned = plan_targets(&targets);
        let health = health_spec();
        let plan = DeploymentPlan {
            name: "clipd",
            source_dir: Path::new("/source"),
            health: &health,
            transaction: "tx-2".to_owned(),
            targets: planned,
        };
        let record = build_failure_record(
            42,
            Duration::from_millis(50),
            &plan,
            &[],
            0,
            &anyhow::anyhow!("compiler failed"),
        )
        .unwrap();
        let value = serde_json::to_value(record).unwrap();

        assert_eq!(value["result"], "build_failed");
        assert_eq!(value["failed_stage"], "build");
        assert_eq!(value["targets"][0]["build"]["result"], "failed");
        assert_eq!(value["targets"][1]["build"]["result"], "not_attempted");
        assert!(value.get("transaction_ms").is_none());
    }

    #[test]
    fn append_keeps_each_record_on_one_parseable_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-deploys.jsonl");
        let record = JournalRecord {
            v: JOURNAL_VERSION,
            at: 42,
            transaction: "tx".to_owned(),
            name: "clipd".to_owned(),
            result: "build_failed",
            failed_stage: Some("build"),
            error: Some("first line\nsecond line".to_owned()),
            build_ms: 1,
            transaction_ms: None,
            total_ms: 1,
            targets: Vec::new(),
        };

        append_record(&path, &record).unwrap();
        append_record(&path, &record).unwrap();

        let text = std::fs::read_to_string(path).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines
            .iter()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
    }

    #[test]
    fn concurrent_appends_are_serialized_without_lost_records() {
        const WRITERS: usize = 4;
        const RECORDS_PER_WRITER: usize = 25;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-deploys.jsonl");
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = &path;
                scope.spawn(move || {
                    for sequence in 0..RECORDS_PER_WRITER {
                        let record = JournalRecord {
                            v: JOURNAL_VERSION,
                            at: 42,
                            transaction: format!("tx-{writer}-{sequence}"),
                            name: "clipd".to_owned(),
                            result: "deployed",
                            failed_stage: None,
                            error: None,
                            build_ms: 1,
                            transaction_ms: Some(1),
                            total_ms: 2,
                            targets: Vec::new(),
                        };
                        append_record(path, &record).unwrap();
                    }
                });
            }
        });

        let text = std::fs::read_to_string(path).unwrap();
        let transactions = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["transaction"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(transactions.len(), WRITERS * RECORDS_PER_WRITER);
    }
}
