//! Effect-agnostic ordering policy for a compensating multi-target deployment.
//!
//! Machine effects enter only through [`Runtime`], which makes every state
//! transition and failure path deterministic under test.

use anyhow::{anyhow, Result};

/// Machine effects required by the agent deployment state machine.
///
/// A failed `prepare` owns cleanup of any state it created before returning.
/// Once `prepare` succeeds, the policy calls `cleanup` unless compensation for
/// that target fails; failed compensation deliberately preserves recovery state.
pub(super) trait Runtime {
    type Prepared;

    fn target_count(&self) -> usize;
    fn target_name(&self, index: usize) -> &str;
    fn prepare(&mut self, index: usize) -> Result<Self::Prepared>;
    fn activate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn verify(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn compensate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
    fn cleanup(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()>;
}

pub(super) fn execute<R: Runtime>(runtime: &mut R) -> Result<()> {
    let target_count = runtime.target_count();
    let mut prepared = Vec::with_capacity(target_count);

    for index in 0..target_count {
        let target_name = runtime.target_name(index).to_owned();
        match runtime.prepare(index) {
            Ok(state) => prepared.push(state),
            Err(error) => {
                let mut errors = vec![format!("preparing target `{target_name}`: {error:#}")];
                errors.extend(cleanup_targets(runtime, &prepared, 0..prepared.len()));
                return Err(error_report("agent deployment preparation failed", errors));
            }
        }
    }

    for index in 0..target_count {
        let target_name = runtime.target_name(index).to_owned();
        let result = runtime
            .activate(index, &prepared[index])
            .and_then(|()| runtime.verify(index, &prepared[index]));
        if let Err(error) = result {
            let mut errors = vec![format!("activating target `{target_name}`: {error:#}")];
            let mut preserve = vec![false; target_count];
            for rollback_index in (0..=index).rev() {
                if let Err(rollback_error) =
                    runtime.compensate(rollback_index, &prepared[rollback_index])
                {
                    preserve[rollback_index] = true;
                    errors.push(format!(
                        "compensating target `{}`: {rollback_error:#}; remaining transaction state and lock were preserved for recovery",
                        runtime.target_name(rollback_index)
                    ));
                }
            }
            errors.extend(cleanup_targets(
                runtime,
                &prepared,
                (0..target_count).filter(|cleanup_index| !preserve[*cleanup_index]),
            ));
            return Err(error_report(
                "agent deployment failed; compensation was attempted",
                errors,
            ));
        }
    }

    let cleanup_errors = cleanup_targets(runtime, &prepared, 0..target_count);
    if !cleanup_errors.is_empty() {
        return Err(error_report(
            "agent deployment is healthy, but transaction cleanup failed",
            cleanup_errors,
        ));
    }

    Ok(())
}

fn cleanup_targets<R: Runtime>(
    runtime: &mut R,
    prepared: &[R::Prepared],
    indices: impl Iterator<Item = usize>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for index in indices {
        if let Err(error) = runtime.cleanup(index, &prepared[index]) {
            errors.push(format!(
                "cleaning target `{}`: {error:#}",
                runtime.target_name(index)
            ));
        }
    }
    errors
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

        execute(&mut runtime).unwrap();

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

        let error = execute(&mut runtime).unwrap_err();

        assert!(error
            .to_string()
            .contains("agent deployment preparation failed"));
        assert_eq!(
            runtime.events,
            [Event::Prepare(0), Event::Prepare(1), Event::Cleanup(0)]
        );
    }

    #[test]
    fn activation_failure_compensates_attempted_targets_in_reverse() {
        let mut runtime = FakeRuntime::healthy(3);
        runtime.fail(1).activate = true;

        let error = execute(&mut runtime).unwrap_err();

        assert!(error.to_string().contains("compensation was attempted"));
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

        execute(&mut runtime).unwrap_err();

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

        let error = execute(&mut runtime).unwrap_err();

        assert!(error.to_string().contains("preserved for recovery"));
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

        let error = execute(&mut runtime).unwrap_err();

        assert!(error.to_string().contains("deployment is healthy"));
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
