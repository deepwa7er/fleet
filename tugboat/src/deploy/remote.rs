//! Remote deployment effects behind the shared compensating transaction policy.
//!
//! The policy decides ordering and failure semantics. This module owns the
//! systemd/SSH/rsync implementation for one VPS service and returns a typed
//! outcome instead of asking callers to infer rollback from an SSH exit code.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use super::{ledger_append, DeploymentPlan};
use crate::manifest::{ArtifactKind, Health};
use crate::subprocess::{CapturedOutput, LogSink};
use crate::transaction::{self, Outcome};
use crate::transport::{self, shell_quote as shq, RsyncKind};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HEALTH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
const LOCK_ROOT: &str = "/var/lib/tugboat/transactions";

struct PreparedRemote {
    existed: Vec<bool>,
}

pub(super) fn execute(plan: &DeploymentPlan<'_>, log: &dyn LogSink) -> transaction::Execution {
    let mut runtime = RemoteRuntime {
        plan,
        log,
        verified: false,
    };
    let execution = transaction::execute(&mut runtime);
    match execution.report.outcome {
        Outcome::Deployed | Outcome::DeployedCleanupIncomplete => {
            append_ledger(plan, "deployed", log)
        }
        Outcome::Compensated => append_ledger(plan, "rolled_back", log),
        Outcome::PreparationFailed | Outcome::CompensationIncomplete => {}
    }
    execution
}

struct RemoteRuntime<'plan, 'manifest> {
    plan: &'plan DeploymentPlan<'manifest>,
    log: &'plan dyn LogSink,
    verified: bool,
}

impl transaction::Runtime for RemoteRuntime<'_, '_> {
    type Prepared = PreparedRemote;

    fn target_count(&self) -> usize {
        1
    }

    fn target_name(&self, _index: usize) -> &str {
        &self.plan.manifest.name
    }

    fn prepare(&mut self, _index: usize) -> Result<Self::Prepared> {
        self.log.line(&format!(
            "==> PREPARE: {} transaction {}",
            self.plan.manifest.host(),
            self.plan.id
        ));
        acquire_lock(self.plan)?;

        let result = (|| {
            for artifact in &self.plan.artifacts {
                let staged = artifact.staged(self.plan);
                self.log.line(&format!(
                    "==> SHIP{}: {} → {}:{staged}",
                    if artifact.manifest.kind == ArtifactKind::Dir {
                        " DIR"
                    } else {
                        ""
                    },
                    artifact.src.display(),
                    self.plan.manifest.host(),
                ));
                let kind = match artifact.manifest.kind {
                    ArtifactKind::File => RsyncKind::File,
                    ArtifactKind::Dir => RsyncKind::Directory,
                };
                transport::rsync(
                    &artifact.src,
                    &format!("{}:{staged}", self.plan.manifest.host()),
                    kind,
                    self.log,
                )?;
            }
            backup_live_artifacts(self.plan)
        })();

        match result {
            Ok(existed) => Ok(PreparedRemote { existed }),
            Err(error) => {
                let cleanup_errors = cleanup_state(self.plan);
                if cleanup_errors.is_empty() {
                    Err(error)
                } else {
                    Err(error_report(&format!("{error:#}"), cleanup_errors))
                }
            }
        }
    }

    fn activate(&mut self, _index: usize, _prepared: &Self::Prepared) -> Result<()> {
        self.log.line(&format!(
            "==> ACTIVATE: {} swap artifacts and restart {}",
            self.plan.manifest.host(),
            self.plan.manifest.name
        ));
        require_success(
            transport::ssh_script_capture(
                self.plan.manifest.host(),
                &activation_script(self.plan),
                OPERATION_TIMEOUT,
            )?,
            "remote activation",
        )?;
        Ok(())
    }

    fn verify(&mut self, _index: usize, _prepared: &Self::Prepared) -> Result<()> {
        self.log
            .line("==> VERIFY HOST: waiting for the installed service");
        wait_for_health(self.plan)?;
        self.log.line(&format!(
            "    {} is active and healthy",
            self.plan.manifest.name
        ));
        self.verified = true;
        Ok(())
    }

    fn compensate(&mut self, _index: usize, prepared: &Self::Prepared) -> Result<()> {
        self.log.line(&format!(
            "==> COMPENSATE: restoring {} on {}",
            self.plan.manifest.name,
            self.plan.manifest.host()
        ));
        let mut errors = Vec::new();
        match transport::ssh_script_capture(
            self.plan.manifest.host(),
            &compensation_script(self.plan, &prepared.existed),
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
            if let Err(error) = wait_for_health(self.plan) {
                errors.push(format!("verifying restored service: {error:#}"));
            } else {
                self.log.line(&format!(
                    "    restored {} is active and healthy",
                    self.plan.manifest.name
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
        let mut errors = Vec::new();
        if self.verified && self.plan.manifest.lighthouse.enroll {
            self.log.line("==> ENROLL: lighthouse.target");
            let name = shq(&format!("{}.service", self.plan.manifest.name));
            let script = format!(
                "set -euo pipefail\n{}\n$sudo systemctl add-wants lighthouse.target {name}\n$sudo systemctl daemon-reload",
                sudo_setup()
            );
            match transport::ssh_script_capture(
                self.plan.manifest.host(),
                &script,
                OPERATION_TIMEOUT,
            ) {
                Ok(output) => {
                    if let Err(error) = require_success(output, "lighthouse enrollment") {
                        errors.push(format!("enrolling service: {error:#}"));
                    }
                }
                Err(error) => errors.push(format!("enrolling service: {error:#}")),
            }
        }
        errors.extend(cleanup_state(self.plan));
        if errors.is_empty() {
            Ok(())
        } else {
            Err(error_report("remote transaction cleanup failed", errors))
        }
    }
}

fn acquire_lock(plan: &DeploymentPlan<'_>) -> Result<()> {
    let lock = shq(&lock_path(plan));
    let script = format!(
        "set -euo pipefail\n{}\n$sudo mkdir -p {root}\n$sudo mkdir {lock}",
        sudo_setup(),
        root = shq(LOCK_ROOT),
    );
    require_success(
        transport::ssh_script_capture(plan.manifest.host(), &script, OPERATION_TIMEOUT)?,
        "acquiring remote deployment lock",
    )?;
    Ok(())
}

fn backup_live_artifacts(plan: &DeploymentPlan<'_>) -> Result<Vec<bool>> {
    let mut script = format!("set -euo pipefail\n{}\n", sudo_setup());
    for artifact in &plan.artifacts {
        let staged = shq(&artifact.staged(plan));
        let live = shq(&artifact.manifest.dest);
        let backup = shq(&artifact.backup(plan));
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
        transport::ssh_script_capture(plan.manifest.host(), &script, OPERATION_TIMEOUT)?,
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
    if existed.len() != plan.artifacts.len() {
        bail!(
            "remote backup reported {} artifacts, expected {}",
            existed.len(),
            plan.artifacts.len()
        );
    }
    Ok(existed)
}

fn activation_script(plan: &DeploymentPlan<'_>) -> String {
    let mut script = format!("set -euo pipefail\n{}\n", sudo_setup());
    for artifact in &plan.artifacts {
        let staged = shq(&artifact.staged(plan));
        let live = shq(&artifact.manifest.dest);
        script.push_str(&format!("$sudo rm -rf {live}\n$sudo mv {staged} {live}\n"));
    }
    script.push_str(&format!(
        "$sudo systemctl restart {}\n",
        shq(&plan.manifest.name)
    ));
    script
}

fn compensation_script(plan: &DeploymentPlan<'_>, existed: &[bool]) -> String {
    let mut script = format!("set -uo pipefail\n{}\nfailed=0\n", sudo_setup());
    for (artifact, existed) in plan.artifacts.iter().zip(existed).rev() {
        let staged = shq(&artifact.staged(plan));
        let live = shq(&artifact.manifest.dest);
        let backup = shq(&artifact.backup(plan));
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
        shq(&plan.manifest.name)
    ));
    script
}

fn wait_for_health(plan: &DeploymentPlan<'_>) -> Result<()> {
    let (retries, interval_ms, check) = match &plan.manifest.health {
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
        }) => (*retries, *interval_ms, systemctl_healthcheck(plan)),
        None => (10, 500, systemctl_healthcheck(plan)),
    };
    let mut last_error = None;
    for attempt in 1..=retries {
        match transport::ssh_script_capture(plan.manifest.host(), &check, HEALTH_ATTEMPT_TIMEOUT) {
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
        plan.manifest.name,
        last_error.unwrap_or_else(|| "health check made no attempts".to_owned())
    )
}

fn systemctl_healthcheck(plan: &DeploymentPlan<'_>) -> String {
    format!(
        "set -euo pipefail\n{}\n[ \"$($sudo systemctl is-active {})\" = active ]",
        sudo_setup(),
        shq(&plan.manifest.name)
    )
}

fn cleanup_state(plan: &DeploymentPlan<'_>) -> Vec<String> {
    let mut script = format!("set -uo pipefail\n{}\nfailed=0\n", sudo_setup());
    for artifact in &plan.artifacts {
        script.push_str(&format!(
            "if ! $sudo rm -rf {} {}; then echo 'could not remove transaction artifacts' >&2; failed=1; fi\n",
            shq(&artifact.staged(plan)),
            shq(&artifact.backup(plan)),
        ));
    }
    script.push_str(&format!(
        "if ! $sudo rmdir {}; then echo 'could not release deployment lock' >&2; failed=1; fi\nexit \"$failed\"\n",
        shq(&lock_path(plan)),
    ));
    match transport::ssh_script_capture(plan.manifest.host(), &script, OPERATION_TIMEOUT) {
        Ok(output) => match require_success(output, "remote transaction cleanup") {
            Ok(_) => Vec::new(),
            Err(error) => vec![format!("{error:#}")],
        },
        Err(error) => vec![format!("{error:#}")],
    }
}

fn append_ledger(plan: &DeploymentPlan<'_>, result: &str, log: &dyn LogSink) {
    let script = ledger_append(&plan.manifest.name, plan.stamp.as_ref(), &plan.id, result);
    if script.is_empty() {
        return;
    }
    if let Err(error) = transport::ssh_script_capture(
        plan.manifest.host(),
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

fn lock_path(plan: &DeploymentPlan<'_>) -> String {
    format!("{LOCK_ROOT}/{}.lock", plan.manifest.name)
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
