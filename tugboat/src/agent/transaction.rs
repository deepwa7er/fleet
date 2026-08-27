use std::fs::Permissions;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    remote_restart_script, with_suffix, DeploymentPlan, HealthSpec, Location, PlannedTarget, Target,
};
use crate::subprocess::{run_captured_timeout, CapturedOutput, StdoutSink};
use crate::transport::{self, RsyncKind};

mod policy;

const HEALTH_PROTOCOL: u32 = 1;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthStatus {
    protocol: u32,
    pid: u32,
    instance: String,
    binary_sha256: String,
    ready: bool,
}

#[derive(Debug)]
struct PreparedTarget {
    artifact_hash: String,
    original_hash: String,
    baseline: Option<HealthStatus>,
}

pub(super) fn execute(plan: &DeploymentPlan<'_>) -> Result<()> {
    let mut runtime = MachineRuntime::new(plan)?;
    policy::execute(&mut runtime)?;
    println!("\n✓ {} deployed to: {}", plan.name, plan.target_names());
    Ok(())
}

/// Production effects behind the transaction policy. The policy knows target
/// indices and prepared state; this adapter owns every local/SSH operation.
struct MachineRuntime<'plan, 'manifest> {
    plan: &'plan DeploymentPlan<'manifest>,
    artifact_hashes: Vec<String>,
}

impl<'plan, 'manifest> MachineRuntime<'plan, 'manifest> {
    fn new(plan: &'plan DeploymentPlan<'manifest>) -> Result<Self> {
        let artifact_hashes = plan
            .targets
            .iter()
            .map(|planned| sha256_file(&planned.artifact))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            plan,
            artifact_hashes,
        })
    }
}

impl policy::Runtime for MachineRuntime<'_, '_> {
    type Prepared = PreparedTarget;

    fn target_count(&self) -> usize {
        self.plan.targets.len()
    }

    fn target_name(&self, index: usize) -> &str {
        &self.plan.targets[index].target.name
    }

    fn prepare(&mut self, index: usize) -> Result<Self::Prepared> {
        let planned = &self.plan.targets[index];
        println!(
            "\n════ PREPARE {} → {} ════",
            self.plan.name, planned.target.name
        );
        let baseline = match probe_once(planned.target, self.plan.health) {
            Ok(status) => {
                println!(
                    "==> BASELINE: pid {}, instance {}, sha256 {}",
                    status.pid, status.instance, status.binary_sha256
                );
                Some(status)
            }
            Err(error) => {
                println!("==> BASELINE unavailable: {error:#}");
                None
            }
        };
        prepare_target(
            planned,
            &self.plan.transaction,
            self.artifact_hashes[index].clone(),
            baseline,
        )
    }

    fn activate(&mut self, index: usize, _prepared: &Self::Prepared) -> Result<()> {
        // An activation attempt is compensatable even when its result is
        // ambiguous, such as an SSH disconnect after the remote rename.
        let planned = &self.plan.targets[index];
        println!(
            "\n════ ACTIVATE {} → {} ════",
            self.plan.name, planned.target.name
        );
        activate_target(planned, &self.plan.transaction)
    }

    fn verify(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
        let target = self.plan.targets[index].target;
        wait_for_health(
            target,
            self.plan.health,
            &prepared.artifact_hash,
            prepared
                .baseline
                .as_ref()
                .map(|status| status.instance.as_str()),
        )?;
        Ok(())
    }

    fn compensate(&mut self, index: usize, prepared: &Self::Prepared) -> Result<()> {
        compensate_target(
            &self.plan.targets[index],
            prepared,
            self.plan.health,
            &self.plan.transaction,
        )
    }

    fn cleanup(&mut self, index: usize, _prepared: &Self::Prepared) -> Result<()> {
        cleanup_target(&self.plan.targets[index], &self.plan.transaction)
    }
}

fn prepare_target(
    planned: &PlannedTarget<'_>,
    transaction: &str,
    artifact_hash: String,
    baseline: Option<HealthStatus>,
) -> Result<PreparedTarget> {
    let original_hash = match &planned.target.location {
        Location::Local => prepare_local(planned, transaction),
        Location::Ssh { host } => prepare_remote(host, planned, transaction),
    }?;
    Ok(PreparedTarget {
        artifact_hash,
        original_hash,
        baseline,
    })
}

fn prepare_local(planned: &PlannedTarget<'_>, transaction: &str) -> Result<String> {
    let paths = LocalPaths::new(planned.target, transaction)?;
    if let Some(parent) = paths.live.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating install dir {}", parent.display()))?;
    }
    std::fs::create_dir(&paths.lock).with_context(|| {
        format!(
            "acquiring deployment lock {}; another deployment or an interrupted transaction may own it",
            paths.lock.display()
        )
    })?;

    let result = (|| {
        if !paths.live.is_file() {
            bail!(
                "{} is not an installed binary; bootstrap the service before using transactional deploy",
                paths.live.display()
            );
        }
        if paths.staged.exists() || paths.backup.exists() {
            bail!("transaction staging paths already exist");
        }
        std::fs::copy(&planned.artifact, &paths.staged)
            .with_context(|| format!("copying to {}", paths.staged.display()))?;
        std::fs::set_permissions(&paths.staged, Permissions::from_mode(0o755))
            .with_context(|| format!("chmod +x {}", paths.staged.display()))?;
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(&paths.staged)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        std::fs::copy(&paths.live, &paths.backup)
            .with_context(|| format!("backing up to {}", paths.backup.display()))?;
        sha256_file(&paths.backup)
    })();

    match result {
        Ok(original_hash) => {
            println!("==> STAGED: {}", paths.staged.display());
            Ok(original_hash)
        }
        Err(error) => {
            let cleanup_errors = cleanup_local(&paths);
            if cleanup_errors.is_empty() {
                Err(error)
            } else {
                Err(error_report(&format!("{error:#}"), cleanup_errors))
            }
        }
    }
}

fn prepare_remote(host: &str, planned: &PlannedTarget<'_>, transaction: &str) -> Result<String> {
    let paths = RemotePaths::new(planned.target, transaction);
    let preflight = format!(
        "set -euo pipefail\nmkdir -p \"$(dirname {live})\"\nmkdir {lock}\ntrap 'rm -f {staged} {backup}; rmdir {lock}' ERR\ntest -f {live}\ntest ! -e {staged}\ntest ! -e {backup}",
        live = paths.live,
        lock = paths.lock,
        staged = paths.staged,
        backup = paths.backup,
    );
    let output = transport::ssh_script_capture(host, &preflight, OPERATION_TIMEOUT)
        .with_context(|| format!("preflighting {host}:{}", paths.live))?;
    require_success(output, "remote deployment preflight")?;

    let result = (|| {
        println!(
            "==> SHIP: {} → {host}:{}",
            planned.artifact.display(),
            paths.staged
        );
        transport::rsync(
            &planned.artifact,
            &format!("{host}:{}", paths.staged),
            RsyncKind::File,
            &StdoutSink,
        )?;
        let hash = remote_hash_command(&planned.target.platform.os, &paths.backup)?;
        let finalize = format!(
            "set -euo pipefail\nchmod 755 {staged}\ncp -p {live} {backup}\n{hash}",
            staged = paths.staged,
            live = paths.live,
            backup = paths.backup,
        );
        let output = transport::ssh_script_capture(host, &finalize, OPERATION_TIMEOUT)?;
        let output = require_success(output, "remote staging")?;
        parse_hash(&output.stdout).context("parsing remote backup hash")
    })();

    match result {
        Ok(original_hash) => {
            println!("==> STAGED: {host}:{}", paths.staged);
            Ok(original_hash)
        }
        Err(error) => {
            let cleanup_errors = cleanup_remote(host, &paths);
            if cleanup_errors.is_empty() {
                Err(error)
            } else {
                Err(error_report(&format!("{error:#}"), cleanup_errors))
            }
        }
    }
}

fn activate_target(planned: &PlannedTarget<'_>, transaction: &str) -> Result<()> {
    match &planned.target.location {
        Location::Local => {
            let paths = LocalPaths::new(planned.target, transaction)?;
            std::fs::rename(&paths.staged, &paths.live)
                .with_context(|| format!("installing {}", paths.live.display()))?;
            let restart = planned.target.service.restart_command()?;
            println!("==> RESTART: {}", restart.display());
            restart.run_timeout(OPERATION_TIMEOUT)
        }
        Location::Ssh { host } => {
            let paths = RemotePaths::new(planned.target, transaction);
            let restart = remote_restart_script(&planned.target.service);
            let script = format!(
                "set -euo pipefail\nmv {staged} {live}\n{restart}",
                staged = paths.staged,
                live = paths.live,
            );
            println!("==> INSTALL + RESTART on {host}");
            let output = transport::ssh_script_capture(host, &script, OPERATION_TIMEOUT)?;
            require_success(output, "remote activation")?;
            Ok(())
        }
    }
}

fn compensate_target(
    planned: &PlannedTarget<'_>,
    state: &PreparedTarget,
    health: &HealthSpec,
    transaction: &str,
) -> Result<()> {
    println!("==> COMPENSATE: {}", planned.target.name);
    let mut errors = Vec::new();
    match &planned.target.location {
        Location::Local => match LocalPaths::new(planned.target, transaction) {
            Ok(paths) => {
                if let Err(error) = std::fs::rename(&paths.backup, &paths.live) {
                    errors.push(format!("restoring {}: {error}", paths.live.display()));
                }
            }
            Err(error) => errors.push(format!("resolving local paths: {error:#}")),
        },
        Location::Ssh { host } => {
            let paths = RemotePaths::new(planned.target, transaction);
            let script = format!(
                "set -euo pipefail\ntest -f {backup}\nmv {backup} {live}",
                backup = paths.backup,
                live = paths.live,
            );
            match transport::ssh_script_capture(host, &script, OPERATION_TIMEOUT) {
                Ok(output) => {
                    if let Err(error) = require_success(output, "remote binary restore") {
                        errors.push(format!("restoring remote binary: {error:#}"));
                    }
                }
                Err(error) => errors.push(format!("restoring remote binary: {error:#}")),
            }
        }
    }

    if let Err(error) = restart_target(planned.target) {
        errors.push(format!("restarting restored service: {error:#}"));
    }
    if let Err(error) = wait_for_health(
        planned.target,
        health,
        &state.original_hash,
        state
            .baseline
            .as_ref()
            .map(|status| status.instance.as_str()),
    ) {
        errors.push(format!("verifying restored service: {error:#}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(error_report("compensation incomplete", errors))
    }
}

fn restart_target(target: &Target) -> Result<()> {
    match &target.location {
        Location::Local => target
            .service
            .restart_command()?
            .run_timeout(OPERATION_TIMEOUT),
        Location::Ssh { host } => {
            let output = transport::ssh_script_capture(
                host,
                &remote_restart_script(&target.service),
                OPERATION_TIMEOUT,
            )?;
            require_success(output, "remote service restart")?;
            Ok(())
        }
    }
}

fn wait_for_health(
    target: &Target,
    health: &HealthSpec,
    expected_hash: &str,
    previous_instance: Option<&str>,
) -> Result<HealthStatus> {
    let mut last_error = None;
    for attempt in 1..=health.attempts {
        match probe_once(target, health).and_then(|status| {
            if status.binary_sha256 != expected_hash {
                bail!(
                    "running binary sha256 is {}, expected {}",
                    status.binary_sha256,
                    expected_hash
                );
            }
            if previous_instance == Some(status.instance.as_str()) {
                bail!("service still reports the previous process instance");
            }
            Ok(status)
        }) {
            Ok(status) => {
                println!(
                    "==> HEALTHY: pid {}, instance {}, sha256 {}",
                    status.pid, status.instance, status.binary_sha256
                );
                return Ok(status);
            }
            Err(error) => last_error = Some(error),
        }
        if attempt < health.attempts {
            thread::sleep(health.interval);
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("health probe made no attempts"))).with_context(|| {
        format!(
            "target `{}` did not become healthy after {} attempts",
            target.name, health.attempts
        )
    })
}

fn probe_once(target: &Target, health: &HealthSpec) -> Result<HealthStatus> {
    let output = match &target.location {
        Location::Local => {
            let mut command = Command::new(target.destination.local_path()?);
            command.args(&health.args);
            run_captured_timeout(command, None, health.timeout).context("running health probe")?
        }
        Location::Ssh { host } => {
            let remote_command = std::iter::once(target.destination.display())
                .chain(health.args.iter().map(|arg| transport::shell_quote(arg)))
                .collect::<Vec<_>>()
                .join(" ");
            transport::ssh_capture(host, &remote_command, health.timeout)
                .context("running remote health probe")?
        }
    };
    let output = require_success(output, "health probe")?;
    let status: HealthStatus =
        serde_json::from_str(output.stdout.trim()).context("parsing health probe JSON")?;
    validate_health(&status)?;
    Ok(status)
}

fn validate_health(status: &HealthStatus) -> Result<()> {
    if status.protocol != HEALTH_PROTOCOL {
        bail!("unsupported health protocol {}", status.protocol);
    }
    if status.pid == 0 {
        bail!("health response has an invalid PID");
    }
    if status.instance.len() != 32 || !status.instance.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("health response has an invalid instance token");
    }
    if status.binary_sha256.len() != 64
        || !status
            .binary_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("health response has an invalid binary SHA-256");
    }
    if !status.ready {
        bail!("service reports that it is not ready");
    }
    Ok(())
}

fn cleanup_target(planned: &PlannedTarget<'_>, transaction: &str) -> Result<()> {
    match &planned.target.location {
        Location::Local => {
            let paths = LocalPaths::new(planned.target, transaction)?;
            cleanup_result(cleanup_local(&paths))
        }
        Location::Ssh { host } => {
            let paths = RemotePaths::new(planned.target, transaction);
            cleanup_result(cleanup_remote(host, &paths))
        }
    }
}

fn cleanup_local(paths: &LocalPaths) -> Vec<String> {
    let mut errors = Vec::new();
    for path in [&paths.staged, &paths.backup] {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("removing {}: {error}", path.display()));
            }
        }
    }
    if let Err(error) = std::fs::remove_dir(&paths.lock) {
        if error.kind() != std::io::ErrorKind::NotFound {
            errors.push(format!("releasing {}: {error}", paths.lock.display()));
        }
    }
    errors
}

fn cleanup_remote(host: &str, paths: &RemotePaths) -> Vec<String> {
    let script = format!(
        "set -euo pipefail\nrm -f {staged} {backup}\nrmdir {lock}",
        staged = paths.staged,
        backup = paths.backup,
        lock = paths.lock,
    );
    match transport::ssh_script_capture(host, &script, OPERATION_TIMEOUT) {
        Ok(output) => match require_success(output, "remote transaction cleanup") {
            Ok(_) => Vec::new(),
            Err(error) => vec![format!("{error:#}")],
        },
        Err(error) => vec![format!("{error:#}")],
    }
}

fn cleanup_result(errors: Vec<String>) -> Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(error_report("transaction cleanup failed", errors))
    }
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

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("reading {} for hashing", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn parse_hash(stdout: &str) -> Result<String> {
    let hash = stdout
        .split_whitespace()
        .next()
        .context("hash command returned no output")?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("hash command returned an invalid SHA-256: {hash}");
    }
    Ok(hash.to_ascii_lowercase())
}

fn remote_hash_command(os: &str, path: &str) -> Result<String> {
    match os {
        "darwin" => Ok(format!("shasum -a 256 {path}")),
        "linux" => Ok(format!("sha256sum {path}")),
        _ => bail!("unsupported target operating system `{os}`"),
    }
}

fn error_report(summary: &str, errors: Vec<String>) -> anyhow::Error {
    anyhow!("{summary}:\n  - {}", errors.join("\n  - "))
}

struct LocalPaths {
    live: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    lock: PathBuf,
}

impl LocalPaths {
    fn new(target: &Target, transaction: &str) -> Result<Self> {
        let live = target.destination.local_path()?;
        Ok(Self {
            staged: with_suffix(&live, &format!(".tug-new-{transaction}")),
            backup: with_suffix(&live, &format!(".tug-backup-{transaction}")),
            lock: with_suffix(&live, ".tug-lock"),
            live,
        })
    }
}

struct RemotePaths {
    live: String,
    staged: String,
    backup: String,
    lock: String,
}

impl RemotePaths {
    fn new(target: &Target, transaction: &str) -> Self {
        let live = target.destination.display();
        Self {
            staged: format!("{live}.tug-new-{transaction}"),
            backup: format!("{live}.tug-backup-{transaction}"),
            lock: format!("{live}.tug-lock"),
            live,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{BuildCommand, Destination, Platform};
    use crate::user_service::UserService;
    use std::collections::BTreeMap;

    #[test]
    fn hashes_files_as_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"hello").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn validates_health_identity_fields() {
        let valid = HealthStatus {
            protocol: HEALTH_PROTOCOL,
            pid: 42,
            instance: "1a".repeat(16),
            binary_sha256: "2b".repeat(32),
            ready: true,
        };
        assert!(validate_health(&valid).is_ok());

        let mut invalid = valid.clone();
        invalid.ready = false;
        assert!(validate_health(&invalid).is_err());
        invalid = valid.clone();
        invalid.binary_sha256 = "not-a-hash".to_owned();
        assert!(validate_health(&invalid).is_err());
    }

    #[test]
    fn remote_hash_commands_match_each_supported_platform() {
        assert_eq!(
            remote_hash_command("darwin", "~/.local/bin/clipd").unwrap(),
            "shasum -a 256 ~/.local/bin/clipd"
        );
        assert_eq!(
            remote_hash_command("linux", "~/.local/bin/clipd").unwrap(),
            "sha256sum ~/.local/bin/clipd"
        );
    }

    #[test]
    fn local_preparation_preserves_the_live_binary_and_holds_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("clipd");
        let artifact = dir.path().join("new-clipd");
        std::fs::write(&live, b"old").unwrap();
        std::fs::write(&artifact, b"new").unwrap();
        let target = Target {
            name: "local".to_owned(),
            location: Location::Local,
            platform: Platform {
                os: "darwin".to_owned(),
                arch: "arm64".to_owned(),
            },
            destination: Destination::Absolute(live.to_string_lossy().into_owned()),
            service: UserService::launchd("com.example.clipd".to_owned()).unwrap(),
        };
        let planned = PlannedTarget {
            target: &target,
            artifact,
            build: BuildCommand {
                program: "unused".to_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
        };

        let original_hash = prepare_local(&planned, "test").unwrap();
        let paths = LocalPaths::new(&target, "test").unwrap();
        assert_eq!(std::fs::read(&paths.live).unwrap(), b"old");
        assert_eq!(std::fs::read(&paths.staged).unwrap(), b"new");
        assert_eq!(std::fs::read(&paths.backup).unwrap(), b"old");
        assert!(paths.lock.is_dir());
        assert_eq!(original_hash, sha256_file(&paths.backup).unwrap());

        let second_error = prepare_local(&planned, "other").unwrap_err();
        assert!(second_error.to_string().contains("deployment lock"));
        assert_eq!(std::fs::read(&paths.backup).unwrap(), b"old");

        assert!(cleanup_local(&paths).is_empty());
        assert!(!paths.lock.exists());
    }
}
