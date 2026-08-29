//! Remote deployment effects behind the shared compensating transaction policy.
//!
//! The policy decides ordering and failure semantics. This module owns the
//! systemd/SSH/rsync implementation for one VPS service and returns a typed
//! outcome instead of asking callers to infer rollback from an SSH exit code.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use tugboat_ledger::LedgerResult;

use super::{ledger_append, RemoteExecutionSpec};
use crate::manifest::{ArtifactKind, Health};
use crate::subprocess::{CapturedOutput, LogSink};
use crate::transaction::{self, PreparationFailure};
use crate::transport::{self, shell_quote as shq, RsyncKind};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HEALTH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
const LOCK_ROOT: &str = "/var/lib/tugboat/transactions";

struct PreparedRemote {
    existed: Vec<bool>,
}

pub(super) fn execute(
    spec: &RemoteExecutionSpec<'_, '_>,
    log: &dyn LogSink,
) -> transaction::Execution {
    transaction::execute(&mut RemoteRuntime { spec, log })
}

pub(super) fn enroll_lighthouse(
    spec: &RemoteExecutionSpec<'_, '_>,
    log: &dyn LogSink,
) -> Result<()> {
    if !spec.manifest.lighthouse.enroll {
        return Ok(());
    }
    log.line("==> ENROLL: lighthouse.target");
    let name = shq(&format!("{}.service", spec.manifest.name));
    let script = format!(
        "set -euo pipefail\n{}\n$sudo systemctl add-wants lighthouse.target {name}\n$sudo systemctl daemon-reload",
        sudo_setup()
    );
    let output =
        transport::ssh_script_capture(spec.manifest.host(), &script, OPERATION_TIMEOUT)?;
    require_success(output, "lighthouse enrollment")?;
    Ok(())
}

struct RemoteRuntime<'plan, 'manifest> {
    spec: &'plan RemoteExecutionSpec<'plan, 'manifest>,
    log: &'plan dyn LogSink,
}

impl transaction::Runtime for RemoteRuntime<'_, '_> {
    type Prepared = PreparedRemote;

    fn target_count(&self) -> usize {
        1
    }

    fn target_name(&self, _index: usize) -> &str {
        &self.spec.manifest.name
    }

    fn prepare(
        &mut self,
        _index: usize,
    ) -> std::result::Result<Self::Prepared, PreparationFailure> {
        self.log.line(&format!(
            "==> PREPARE: {} transaction {}",
            self.spec.manifest.host(),
            self.spec.transaction_id
        ));
        acquire_lock(self.spec).map_err(PreparationFailure::cleanup_uncertain)?;

        let result = (|| {
            for artifact in self.spec.artifacts {
                let staged = artifact.staged(self.spec.transaction_id);
                self.log.line(&format!(
                    "==> SHIP{}: {} → {}:{staged}",
                    if artifact.manifest.kind == ArtifactKind::Dir {
                        " DIR"
                    } else {
                        ""
                    },
                    artifact.src.display(),
                    self.spec.manifest.host(),
                ));
                let kind = match artifact.manifest.kind {
                    ArtifactKind::File => RsyncKind::File,
                    ArtifactKind::Dir => RsyncKind::Directory,
                };
                transport::rsync(
                    &artifact.src,
                    &format!("{}:{staged}", self.spec.manifest.host()),
                    kind,
                    self.log,
                )?;
            }
            backup_live_artifacts(self.spec)
        })();

        match result {
            Ok(existed) => Ok(PreparedRemote { existed }),
            Err(error) => {
                let cleanup_errors = cleanup_state(self.spec);
                if cleanup_errors.is_empty() {
                    Err(PreparationFailure::cleaned(error))
                } else {
                    Err(PreparationFailure::cleanup_failed(
                        error,
                        error_report("remote transaction cleanup failed", cleanup_errors),
                    ))
                }
            }
        }
    }

    fn activate(&mut self, _index: usize, _prepared: &Self::Prepared) -> Result<()> {
        self.log.line(&format!(
            "==> ACTIVATE: {} swap artifacts and restart {}",
            self.spec.manifest.host(),
            self.spec.manifest.name
        ));
        require_success(
            transport::ssh_script_capture(
                self.spec.manifest.host(),
                &activation_script(self.spec),
                OPERATION_TIMEOUT,
            )?,
            "remote activation",
        )?;
        Ok(())
    }

    fn verify(&mut self, _index: usize, _prepared: &Self::Prepared) -> Result<()> {
        self.log
            .line("==> VERIFY HOST: waiting for the installed service");
        wait_for_host_health(self.spec)?;
        self.log.line(&format!(
            "    {} is active and healthy",
            self.spec.manifest.name
        ));
        Ok(())
    }

    fn compensate(&mut self, _index: usize, prepared: &Self::Prepared) -> Result<()> {
        self.log.line(&format!(
            "==> COMPENSATE: restoring {} on {}",
            self.spec.manifest.name,
            self.spec.manifest.host()
        ));
        let mut errors = Vec::new();
        match transport::ssh_script_capture(
            self.spec.manifest.host(),
            &compensation_script(self.spec, &prepared.existed),
            OPERATION_TIMEOUT,
        ) {
            Ok(output) => {
                if let Err(error) = require_success(output, "remote artifact restoration") {
                    errors.push(format!("restoring artifacts: {error:#}"));
                }
            }
            Err(error) => errors.push(format!("restoring artifacts: {error:#}")),
        }
        if errors.is_empty() {
            if let Err(error) = wait_for_host_health(self.spec) {
                errors.push(format!("verifying restored service: {error:#}"));
            } else {
                self.log.line(&format!(
                    "    restored {} is active and healthy",
                    self.spec.manifest.name
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(error_report("compensation incomplete", errors))
        }
    }

    fn cleanup(&mut self, _index: usize, _prepared: &Self::Prepared) -> Result<()> {
        let errors = cleanup_state(self.spec);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(error_report("remote transaction cleanup failed", errors))
        }
    }
}

fn acquire_lock(spec: &RemoteExecutionSpec<'_, '_>) -> Result<()> {
    let lock = shq(&lock_path(spec));
    let script = format!(
        "set -euo pipefail\n{}\n$sudo mkdir -p {root}\n$sudo mkdir {lock}",
        sudo_setup(),
        root = shq(LOCK_ROOT),
    );
    require_success(
        transport::ssh_script_capture(spec.manifest.host(), &script, OPERATION_TIMEOUT)?,
        "acquiring remote deployment lock",
    )?;
    Ok(())
}

fn backup_live_artifacts(spec: &RemoteExecutionSpec<'_, '_>) -> Result<Vec<bool>> {
    let mut script = format!("set -euo pipefail\n{}\n", sudo_setup());
    for artifact in spec.artifacts {
        let staged = shq(&artifact.staged(spec.transaction_id));
        let live = shq(&artifact.manifest.dest);
        let backup = shq(&artifact.backup(spec.transaction_id));
        let expected = match artifact.manifest.kind {
            ArtifactKind::File => "-f",
            ArtifactKind::Dir => "-d",
        };
        script.push_str(&format!(
            "$sudo test {expected} {staged}\n! $sudo test -e {backup}\nif $sudo test -e {live}; then $sudo cp -a {live} {backup}; printf '1\\n'; else printf '0\\n'; fi\n"
        ));
        if artifact.manifest.kind == ArtifactKind::File {
            script.push_str(&format!(
                "$sudo chmod {} {staged}\n",
                shq(&artifact.manifest.mode)
            ));
        }
    }
    let output = require_success(
        transport::ssh_script_capture(spec.manifest.host(), &script, OPERATION_TIMEOUT)?,
        "backing up installed artifacts",
    )?;
    let existed: Vec<bool> = output
        .stdout
        .lines()
        .map(|line| match line.trim() {
            "1" => Ok(true),
            "0" => Ok(false),
            other => bail!("unexpected remote backup response `{other}`"),
        })
        .collect::<Result<_>>()?;
    if existed.len() != spec.artifacts.len() {
        bail!(
            "remote backup reported {} artifacts, expected {}",
            existed.len(),
            spec.artifacts.len()
        );
    }
    Ok(existed)
}

fn activation_script(spec: &RemoteExecutionSpec<'_, '_>) -> String {
    let mut script = format!("set -euo pipefail\n{}\n", sudo_setup());
    for artifact in spec.artifacts {
        let staged = shq(&artifact.staged(spec.transaction_id));
        let live = shq(&artifact.manifest.dest);
        script.push_str(&format!("$sudo rm -rf {live}\n$sudo mv {staged} {live}\n"));
    }
    script.push_str(&format!(
        "$sudo systemctl restart {}\n",
        shq(&spec.manifest.name)
    ));
    script
}

fn compensation_script(spec: &RemoteExecutionSpec<'_, '_>, existed: &[bool]) -> String {
    let mut script = format!("set -uo pipefail\n{}\nfailed=0\n", sudo_setup());
    for (artifact, existed) in spec.artifacts.iter().zip(existed).rev() {
        let staged = shq(&artifact.staged(spec.transaction_id));
        let live = shq(&artifact.manifest.dest);
        let backup = shq(&artifact.backup(spec.transaction_id));
        script.push_str(&format!(
            "if ! $sudo rm -rf {staged}; then echo 'could not remove staged artifact' >&2; failed=1; fi\n"
        ));
        if *existed {
            script.push_str(&format!(
                "if $sudo test -e {backup}; then\n  if ! $sudo rm -rf {live} || ! $sudo mv {backup} {live}; then echo 'could not restore artifact' >&2; failed=1; fi\nelse\n  echo 'artifact backup is missing; leaving the live path untouched' >&2\n  failed=1\nfi\n"
            ));
        } else {
            script.push_str(&format!(
                "if ! $sudo rm -rf {live}; then echo 'could not remove newly introduced artifact' >&2; failed=1; fi\n"
            ));
        }
    }
    script.push_str(&format!(
        "if ! $sudo systemctl restart {}; then echo 'could not restart restored service' >&2; failed=1; fi\nexit \"$failed\"\n",
        shq(&spec.manifest.name)
    ));
    script
}

fn wait_for_host_health(spec: &RemoteExecutionSpec<'_, '_>) -> Result<()> {
    let (retries, interval_ms, check) = match &spec.manifest.health {
        Some(Health {
            url: Some(url),
            retries,
            interval_ms,
        }) => (
            *retries,
            *interval_ms,
            format!("curl -fs -o /dev/null --max-time 12 {}", shq(url)),
        ),
        Some(Health {
            url: None,
            retries,
            interval_ms,
        }) => (*retries, *interval_ms, systemctl_healthcheck(spec)),
        None => (10, 500, systemctl_healthcheck(spec)),
    };
    let mut last_error = None;
    for attempt in 1..=retries {
        match transport::ssh_script_capture(spec.manifest.host(), &check, HEALTH_ATTEMPT_TIMEOUT) {
            Ok(output) if output.status.success() => {
                return Ok(());
            }
            Ok(output) => {
                last_error = Some(format!(
                    "health command exited {}: {}",
                    output.status,
                    output.stderr.trim()
                ));
            }
            Err(error) => last_error = Some(format!("{error:#}")),
        }
        if attempt < retries {
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
    }
    bail!(
        "{} did not become healthy after {retries} attempts: {}",
        spec.manifest.name,
        last_error.unwrap_or_else(|| "health check made no attempts".to_owned())
    )
}

fn systemctl_healthcheck(spec: &RemoteExecutionSpec<'_, '_>) -> String {
    format!(
        "set -euo pipefail\n{}\n[ \"$($sudo systemctl is-active {})\" = active ]",
        sudo_setup(),
        shq(&spec.manifest.name)
    )
}

fn cleanup_state(spec: &RemoteExecutionSpec<'_, '_>) -> Vec<String> {
    let mut script = format!("set -uo pipefail\n{}\nfailed=0\n", sudo_setup());
    for artifact in spec.artifacts {
        script.push_str(&format!(
            "if ! $sudo rm -rf {} {}; then echo 'could not remove transaction artifacts' >&2; failed=1; fi\n",
            shq(&artifact.staged(spec.transaction_id)),
            shq(&artifact.backup(spec.transaction_id)),
        ));
    }
    script.push_str(&format!(
        "if ! $sudo rmdir {}; then echo 'could not release deployment lock' >&2; failed=1; fi\nexit \"$failed\"\n",
        shq(&lock_path(spec)),
    ));
    match transport::ssh_script_capture(spec.manifest.host(), &script, OPERATION_TIMEOUT) {
        Ok(output) => match require_success(output, "remote transaction cleanup") {
            Ok(_) => Vec::new(),
            Err(error) => vec![format!("{error:#}")],
        },
        Err(error) => vec![format!("{error:#}")],
    }
}

pub(super) fn append_ledger(
    spec: &RemoteExecutionSpec<'_, '_>,
    result: LedgerResult,
    log: &dyn LogSink,
) {
    let script = ledger_append(
        &spec.manifest.name,
        spec.stamp,
        spec.transaction_id,
        result,
    );
    if script.is_empty() {
        return;
    }
    if let Err(error) = transport::ssh_script_capture(
        spec.manifest.host(),
        &format!("set -euo pipefail\n{}\n{script}", sudo_setup()),
        OPERATION_TIMEOUT,
    )
    .and_then(|output| require_success(output, "recording deploy ledger").map(drop))
    {
        log.line(&format!(
            "    warning: could not record deploy ledger: {error:#}"
        ));
    }
}

fn lock_path(spec: &RemoteExecutionSpec<'_, '_>) -> String {
    format!("{LOCK_ROOT}/{}.lock", spec.manifest.name)
}

fn sudo_setup() -> &'static str {
    "sudo=\"\"; [ \"$(id -u)\" -eq 0 ] || sudo=\"sudo\""
}

fn require_success(output: CapturedOutput, operation: &str) -> Result<CapturedOutput> {
    if output.status.success() {
        return Ok(output);
    }
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        bail!("{operation} exited with {}", output.status);
    }
    bail!("{operation} exited with {}: {stderr}", output.status)
}

fn error_report(summary: &str, errors: Vec<String>) -> anyhow::Error {
    anyhow!("{summary}:\n  - {}", errors.join("\n  - "))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::*;
    use crate::deploy::{PlannedArtifact, Source};
    use crate::manifest::{Artifact, Build, Lighthouse, Manifest};

    fn manifest(destinations: &[PathBuf]) -> Manifest {
        Manifest {
            name: "service".to_owned(),
            description: None,
            host: Some("deepwa7er".to_owned()),
            port: None,
            state: None,
            build: Build {
                cmd: "true".to_owned(),
                requirements: Vec::new(),
            },
            artifacts: destinations
                .iter()
                .enumerate()
                .map(|(index, destination)| Artifact {
                    src: format!("artifact-{index}"),
                    dest: destination.to_string_lossy().into_owned(),
                    kind: ArtifactKind::File,
                    mode: "0755".to_owned(),
                })
                .collect(),
            health: None,
            verify: None,
            lighthouse: Lighthouse::default(),
        }
    }

    fn plan<'a>(manifest: &'a Manifest, root: &Path) -> DeploymentPlan<'a> {
        DeploymentPlan {
            manifest,
            source: Source::WorkingTree { skip_build: false },
            build_dir: root.to_path_buf(),
            build_cmd: "true".to_owned(),
            artifacts: manifest
                .artifacts
                .iter()
                .map(|artifact| PlannedArtifact {
                    src: root.join(&artifact.src),
                    manifest: artifact,
                })
                .collect(),
            skip_build: false,
            stamp: None,
            id: "42-deadbeef".to_owned(),
            _worktree: None,
        }
    }

    fn run_locally(script: &str, bin: &Path) -> std::process::ExitStatus {
        Command::new("bash")
            .args(["-c", &script.replacen(sudo_setup(), "sudo=\"\"", 1)])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stderr(Stdio::null())
            .status()
            .unwrap()
    }

    #[test]
    fn compensation_restores_every_artifact_after_a_mid_activation_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let systemctl = bin.join("systemctl");
        std::fs::write(&systemctl, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();

        let destinations = [
            dir.path().join("one"),
            dir.path().join("two"),
            dir.path().join("new"),
        ];
        let manifest = manifest(&destinations);
        let plan = plan(&manifest, dir.path());

        for (index, destination) in destinations[..2].iter().enumerate() {
            std::fs::write(destination, format!("old-{index}")).unwrap();
            std::fs::copy(destination, plan.artifacts[index].backup(&plan)).unwrap();
        }
        std::fs::write(plan.artifacts[0].staged(&plan), "new-0").unwrap();
        // Artifact two is deliberately not staged. Activation replaces one,
        // removes two, then fails before touching the newly introduced third.
        std::fs::write(plan.artifacts[2].staged(&plan), "new-2").unwrap();

        assert!(!run_locally(&activation_script(&plan), &bin).success());
        assert_eq!(std::fs::read_to_string(&destinations[0]).unwrap(), "new-0");
        assert!(!destinations[1].exists());

        assert!(run_locally(&compensation_script(&plan, &[true, true, false]), &bin,).success());
        assert_eq!(std::fs::read_to_string(&destinations[0]).unwrap(), "old-0");
        assert_eq!(std::fs::read_to_string(&destinations[1]).unwrap(), "old-1");
        assert!(!destinations[2].exists());
        assert!(!Path::new(&plan.artifacts[2].staged(&plan)).exists());
    }

    #[test]
    fn compensation_continues_after_one_artifact_cannot_be_restored() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let systemctl = bin.join("systemctl");
        std::fs::write(&systemctl, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();

        let destinations = [
            dir.path().join("one"),
            dir.path().join("two"),
            dir.path().join("three"),
        ];
        let manifest = manifest(&destinations);
        let plan = plan(&manifest, dir.path());
        for destination in &destinations {
            std::fs::write(destination, "new").unwrap();
        }
        std::fs::write(plan.artifacts[0].backup(&plan), "old-0").unwrap();
        // Artifact two deliberately has no backup. Compensation must leave its
        // live path intact and continue on to artifact one instead of exiting.
        std::fs::write(plan.artifacts[2].backup(&plan), "old-2").unwrap();

        assert!(!run_locally(&compensation_script(&plan, &[true, true, true]), &bin,).success());
        assert_eq!(std::fs::read_to_string(&destinations[0]).unwrap(), "old-0");
        assert_eq!(std::fs::read_to_string(&destinations[1]).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&destinations[2]).unwrap(), "old-2");
    }
}
