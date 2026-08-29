//! Effect-agnostic ordering policy for a compensating multi-target deployment.
//!
//! Machine effects enter only through [`Runtime`], which makes every state
//! transition and failure path deterministic under test.

use anyhow::{anyhow, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Deployed,
    PreparationFailed,
    Compensated,
    CompensationIncomplete,
    DeployedCleanupIncomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StepOutcome {
    NotAttempted,
    NotRequired,
    Succeeded,
    Failed(String),
    SkippedPreserved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetReport {
    pub(crate) name: String,
    pub(crate) prepare: StepOutcome,
    pub(crate) activate: StepOutcome,
    pub(crate) verify: StepOutcome,
    pub(crate) compensate: StepOutcome,
    pub(crate) cleanup: StepOutcome,
    pub(crate) recovery_preserved: bool,
}

impl TargetReport {
    fn new(name: String) -> Self {
        Self {
            name,
            prepare: StepOutcome::NotAttempted,
            activate: StepOutcome::NotAttempted,
            verify: StepOutcome::NotAttempted,
            compensate: StepOutcome::NotRequired,
            cleanup: StepOutcome::NotRequired,
            recovery_preserved: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) outcome: Outcome,
    pub(crate) targets: Vec<TargetReport>,
}

pub(crate) struct Execution {
    pub(crate) report: Report,
    pub(crate) error: Option<anyhow::Error>,
}

/// Machine effects required by the deployment state machine.
///
/// A failed `prepare` owns cleanup of any state it created before returning.
/// Once `prepare` succeeds, the policy calls `cleanup` unless compensation for
/// that target fails; failed compensation deliberately preserves recovery state.
pub(crate) trait Runtime {
    type Prepared;

    fn target_count(&self) -> usize;
    fn target_name(&self, index: usize) -> &str;
    fn prepare(&mut self, index: usize) -> Result<Self::Prepared>;
    fn activate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn verify(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn compensate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn cleanup(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
}

pub(crate) fn execute<R: Runtime>(runtime: &mut R) -> Execution {
    let target_count = runtime.target_count();
    let mut prepared = Vec::with_capacity(target_count);
    let mut report = Report {
        outcome: Outcome::Deployed,
        targets: (0..target_count)
            .map(|index| TargetReport::new(runtime.target_name(index).to_owned()))
            .collect(),
    };

    for index in 0..target_count {
        let target_name = runtime.target_name(index).to_owned();
        match runtime.prepare(index) {
            Ok(state) => {
                report.targets[index].prepare = StepOutcome::Succeeded;
                report.targets[index].cleanup = StepOutcome::NotAttempted;
                prepared.push(state);
            }
            Err(error) => {
                report.targets[index].prepare = StepOutcome::Failed(format!("{error:#}"));
                let mut errors = vec![format!("preparing target `{target_name}`: {error:#}")];
                errors.extend(cleanup_targets(
                    runtime,
                    &prepared,
                    &mut report,
                    0..prepared.len(),
                ));
                report.outcome = Outcome::PreparationFailed;
                return failed(
                    report,
                    error_report("deployment preparation failed", errors),
                );
            }
        }
    }

    for index in 0..target_count {
        let target_name = runtime.target_name(index).to_owned();
        let error = match runtime.activate(index, &prepared[index]) {
            Ok(()) => {
                report.targets[index].activate = StepOutcome::Succeeded;
                match runtime.verify(index, &prepared[index]) {
                    Ok(()) => {
                        report.targets[index].verify = StepOutcome::Succeeded;
                        None
                    }
                    Err(error) => {
                        report.targets[index].verify = StepOutcome::Failed(format!("{error:#}"));
                        Some(error)
                    }
                }
            }
            Err(error) => {
                report.targets[index].activate = StepOutcome::Failed(format!("{error:#}"));
                Some(error)
            }
        };
        if let Some(error) = error {
            let mut errors = vec![format!("activating target `{target_name}`: {error:#}")];
            let mut preserve = vec![false; target_count];
            for rollback_index in (0..=index).rev() {
                match runtime.compensate(rollback_index, &prepared[rollback_index]) {
                    Ok(()) => {
                        report.targets[rollback_index].compensate = StepOutcome::Succeeded;
                    }
                    Err(rollback_error) => {
                        preserve[rollback_index] = true;
                        report.targets[rollback_index].compensate =
                            StepOutcome::Failed(format!("{rollback_error:#}"));
                        report.targets[rollback_index].cleanup = StepOutcome::SkippedPreserved;
                        report.targets[rollback_index].recovery_preserved = true;
                        errors.push(format!(
                            "compensating target `{}`: {rollback_error:#}; remaining transaction state and lock were preserved for recovery",
                            runtime.target_name(rollback_index)
                        ));
                    }
                }
            }
            errors.extend(cleanup_targets(
                runtime,
                &prepared,
                &mut report,
                (0..target_count).filter(|cleanup_index| !preserve[*cleanup_index]),
            ));
            report.outcome = if preserve.iter().any(|preserved| *preserved) {
                Outcome::CompensationIncomplete
            } else {
                Outcome::Compensated
            };
            return failed(
                report,
                error_report("deployment failed; compensation was attempted", errors),
            );
        }
    }

    let cleanup_errors = cleanup_targets(runtime, &prepared, &mut report, 0..target_count);
    if !cleanup_errors.is_empty() {
        report.outcome = Outcome::DeployedCleanupIncomplete;
        return failed(
            report,
            error_report(
                "deployment is healthy, but transaction cleanup failed",
                cleanup_errors,
            ),
        );
    }

    Execution {
        report,
        error: None,
    }
}

fn cleanup_targets<R: Runtime>(
    runtime: &mut R,
    prepared: &[R::Prepared],
    report: &mut Report,
    indices: impl Iterator<Item = usize>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for index in indices {
        match runtime.cleanup(index, &prepared[index]) {
            Ok(()) => report.targets[index].cleanup = StepOutcome::Succeeded,
            Err(error) => {
                report.targets[index].cleanup = StepOutcome::Failed(format!("{error:#}"));
                errors.push(format!(
                    "cleaning target `{}`: {error:#}",
                    runtime.target_name(index)
                ));
            }
        }
    }
    errors
}

fn failed(report: Report, error: anyhow::Error) -> Execution {
    Execution {
        report,
        error: Some(error),
    }
}

fn error_report(summary: &str, errors: Vec<String>) -> anyhow::Error {
    anyhow!("{summary}:\n  - {}", errors.join("\n  - "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Event {
        Prepare(usize),
        Activate(usize),
        Verify(usize),
        Compensate(usize),
        Cleanup(usize),
    }

    #[derive(Default)]
    struct Failures {
        prepare: bool,
        activate: bool,
        verify: bool,
        compensate: bool,
        cleanup: bool,
    }

    struct FakeTarget {
        name: &'static str,
        failures: Failures,
    }

    struct FakeRuntime {
        targets: Vec<FakeTarget>,
        events: Vec<Event>,
    }

    impl FakeRuntime {
        fn healthy(count: usize) -> Self {
            Self {
                targets: (0..count)
                    .map(|index| FakeTarget {
                        name: ["one", "two", "three"][index],
                        failures: Failures::default(),
                    })
                    .collect(),
                events: Vec::new(),
            }
        }

        fn fail(&mut self, index: usize) -> &mut Failures {
            &mut self.targets[index].failures
        }

        fn record(&mut self, event: Event, fails: bool) -> Result<()> {
            self.events.push(event);
            if fails {
                bail!("injected {event:?} failure");
            }
            Ok(())
        }
    }

    impl Runtime for FakeRuntime {
        type Prepared = usize;

        fn target_count(&self) -> usize {
            self.targets.len()
        }

        fn target_name(&self, index: usize) -> &str {
            self.targets[index].name
        }

        fn prepare(&mut self, index: usize) -> Result<Self::Prepared> {
            self.record(Event::Prepare(index), self.targets[index].failures.prepare)?;
            Ok(index)
        }

        fn activate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
            assert_eq!(index, *prepared);
            self.record(
                Event::Activate(index),
                self.targets[index].failures.activate,
            )
        }

        fn verify(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
            assert_eq!(index, *prepared);
            self.record(Event::Verify(index), self.targets[index].failures.verify)
        }

        fn compensate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
            assert_eq!(index, *prepared);
            self.record(
                Event::Compensate(index),
                self.targets[index].failures.compensate,
            )
        }

        fn cleanup(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
            assert_eq!(index, *prepared);
            self.record(Event::Cleanup(index), self.targets[index].failures.cleanup)
        }
    }

    #[test]
    fn success_prepares_every_target_before_activation() {
        let mut runtime = FakeRuntime::healthy(3);

        let execution = execute(&mut runtime);

        assert!(execution.error.is_none());
        assert_eq!(execution.report.outcome, Outcome::Deployed);
        assert!(execution.report.targets.iter().all(|target| {
            target.prepare == StepOutcome::Succeeded
                && target.activate == StepOutcome::Succeeded
                && target.verify == StepOutcome::Succeeded
                && target.compensate == StepOutcome::NotRequired
                && target.cleanup == StepOutcome::Succeeded
                && !target.recovery_preserved
        }));
        assert_eq!(
            runtime.events,
            [
                Event::Prepare(0),
                Event::Prepare(1),
                Event::Prepare(2),
                Event::Activate(0),
                Event::Verify(0),
                Event::Activate(1),
                Event::Verify(1),
                Event::Activate(2),
                Event::Verify(2),
                Event::Cleanup(0),
                Event::Cleanup(1),
                Event::Cleanup(2),
            ]
        );
    }

    #[test]
    fn preparation_failure_cleans_only_completed_preparations() {
        let mut runtime = FakeRuntime::healthy(3);
        runtime.fail(1).prepare = true;

        let execution = execute(&mut runtime);
        let error = execution.error.as_ref().unwrap();

        assert!(error.to_string().contains("deployment preparation failed"));
        assert_eq!(execution.report.outcome, Outcome::PreparationFailed);
        assert!(matches!(
            execution.report.targets[1].prepare,
            StepOutcome::Failed(_)
        ));
        assert_eq!(
            execution.report.targets[2].prepare,
            StepOutcome::NotAttempted
        );
        assert_eq!(
            runtime.events,
            [Event::Prepare(0), Event::Prepare(1), Event::Cleanup(0)]
        );
    }

    #[test]
    fn activation_failure_compensates_attempted_targets_in_reverse() {
        let mut runtime = FakeRuntime::healthy(3);
        runtime.fail(1).activate = true;

        let execution = execute(&mut runtime);
        let error = execution.error.as_ref().unwrap();

        assert!(error.to_string().contains("compensation was attempted"));
        assert_eq!(execution.report.outcome, Outcome::Compensated);
        assert!(matches!(
            execution.report.targets[1].activate,
            StepOutcome::Failed(_)
        ));
        assert_eq!(
            execution.report.targets[1].verify,
            StepOutcome::NotAttempted
        );
        assert_eq!(
            runtime.events,
            [
                Event::Prepare(0),
                Event::Prepare(1),
                Event::Prepare(2),
                Event::Activate(0),
                Event::Verify(0),
                Event::Activate(1),
                Event::Compensate(1),
                Event::Compensate(0),
                Event::Cleanup(0),
                Event::Cleanup(1),
                Event::Cleanup(2),
            ]
        );
    }

    #[test]
    fn verification_failure_uses_the_same_compensation_path() {
        let mut runtime = FakeRuntime::healthy(3);
        runtime.fail(1).verify = true;

        let execution = execute(&mut runtime);

        assert_eq!(execution.report.outcome, Outcome::Compensated);
        assert_eq!(execution.report.targets[1].activate, StepOutcome::Succeeded);
        assert!(matches!(
            execution.report.targets[1].verify,
            StepOutcome::Failed(_)
        ));
        assert_eq!(
            runtime.events,
            [
                Event::Prepare(0),
                Event::Prepare(1),
                Event::Prepare(2),
                Event::Activate(0),
                Event::Verify(0),
                Event::Activate(1),
                Event::Verify(1),
                Event::Compensate(1),
                Event::Compensate(0),
                Event::Cleanup(0),
                Event::Cleanup(1),
                Event::Cleanup(2),
            ]
        );
    }

    #[test]
    fn failed_compensation_preserves_that_targets_recovery_state() {
        let mut runtime = FakeRuntime::healthy(3);
        runtime.fail(1).activate = true;
        runtime.fail(1).compensate = true;

        let execution = execute(&mut runtime);
        let error = execution.error.as_ref().unwrap();

        assert!(error.to_string().contains("preserved for recovery"));
        assert_eq!(execution.report.outcome, Outcome::CompensationIncomplete);
        assert!(matches!(
            execution.report.targets[1].compensate,
            StepOutcome::Failed(_)
        ));
        assert_eq!(
            execution.report.targets[1].cleanup,
            StepOutcome::SkippedPreserved
        );
        assert!(execution.report.targets[1].recovery_preserved);
        assert_eq!(
            runtime.events,
            [
                Event::Prepare(0),
                Event::Prepare(1),
                Event::Prepare(2),
                Event::Activate(0),
                Event::Verify(0),
                Event::Activate(1),
                Event::Compensate(1),
                Event::Compensate(0),
                Event::Cleanup(0),
                Event::Cleanup(2),
            ]
        );
    }

    #[test]
    fn healthy_deployment_reports_cleanup_failure_without_compensation() {
        let mut runtime = FakeRuntime::healthy(2);
        runtime.fail(0).cleanup = true;

        let execution = execute(&mut runtime);
        let error = execution.error.as_ref().unwrap();

        assert!(error.to_string().contains("deployment is healthy"));
        assert_eq!(execution.report.outcome, Outcome::DeployedCleanupIncomplete);
        assert!(matches!(
            execution.report.targets[0].cleanup,
            StepOutcome::Failed(_)
        ));
        assert_eq!(
            runtime.events,
            [
                Event::Prepare(0),
                Event::Prepare(1),
                Event::Activate(0),
                Event::Verify(0),
                Event::Activate(1),
                Event::Verify(1),
                Event::Cleanup(0),
                Event::Cleanup(1),
            ]
        );
    }
}
