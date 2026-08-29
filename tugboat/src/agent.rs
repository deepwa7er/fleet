//! `tugboat agent` — deploy a per-user daemon to the dev machines themselves.
//!
//! Distinct from the VPS deploy engine (`deploy.rs`), which targets a
//! root-owned systemd *system* service on the `deepwa7er` host. An agent is a
//! daemon that lives on a dev machine, installed locally or over SSH with an
//! atomic swap, then restarted through launchd (macOS) or `systemd --user`
//! (Linux), like `tidepool-clipd`.
//!
//! A manifest is first converted into a validated domain model, then into a
//! deployment plan shared by dry-run and execution. Execution builds and stages
//! every selected target before activating the first one, then verifies the
//! exact running artifacts or restores every target it already attempted.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::transport;
use crate::user_service::{UserService, UserServiceManager};

mod journal;
mod transaction;

// ── manifest input + validated domain model ─────────────────────────────────

/// The TOML representation. These types never reach execution: [`load`]
/// converts them into the domain types below after validating their structural
/// invariants.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    name: String,
    build: RawBuild,
    health: RawHealth,
    targets: Vec<RawTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHealth {
    args: Vec<String>,
    attempts: u32,
    interval_ms: u64,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    name: String,
    #[serde(default)]
    local: bool,
    #[serde(default)]
    ssh: Option<String>,
    os: String,
    arch: String,
    dest: String,
    #[serde(default)]
    launchd: Option<String>,
    #[serde(default)]
    systemd_user: Option<String>,
}

#[derive(Debug)]
struct Manifest {
    name: String,
    build: BuildSpec,
    health: HealthSpec,
    targets: Vec<Target>,
}

#[derive(Debug)]
struct BuildSpec {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

#[derive(Debug)]
struct HealthSpec {
    args: Vec<String>,
    attempts: u32,
    interval: Duration,
    timeout: Duration,
}

#[derive(Debug)]
struct Target {
    name: String,
    location: Location,
    platform: Platform,
    destination: Destination,
    service: UserService,
}

#[derive(Debug)]
enum Location {
    Local,
    Ssh { host: String },
}

#[derive(Debug)]
struct Platform {
    os: String,
    arch: String,
}

#[derive(Debug)]
enum Destination {
    Absolute(String),
    HomeRelative(String),
}

impl Destination {
    fn parse(value: String, target: &str) -> Result<Self> {
        if value == "~/" || value == "~" {
            bail!("agent target `{target}`: `dest` must name a file below the home directory");
        }
        let path = value.strip_prefix("~/").unwrap_or(&value);
        if !path.chars().all(is_remote_path_char) {
            bail!(
                "agent target `{target}`: `dest` contains characters unsupported by the agent transport"
            );
        }
        if path.ends_with('/')
            || Path::new(path)
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            bail!("agent target `{target}`: `dest` must name a file without `..` components");
        }
        if let Some(relative) = value.strip_prefix("~/") {
            return Ok(Self::HomeRelative(relative.to_owned()));
        }
        if value.starts_with('/') {
            return Ok(Self::Absolute(value));
        }
        bail!("agent target `{target}`: `dest` must be absolute or start with `~/`")
    }

    fn display(&self) -> String {
        match self {
            Self::Absolute(path) => path.clone(),
            Self::HomeRelative(path) => format!("~/{path}"),
        }
    }

    fn local_path(&self) -> Result<PathBuf> {
        match self {
            Self::Absolute(path) => Ok(PathBuf::from(path)),
            Self::HomeRelative(path) => {
                let home = std::env::var_os("HOME")
                    .context("HOME is not set; cannot expand agent destination")?;
                Ok(PathBuf::from(home).join(path))
            }
        }
    }
}

impl TryFrom<RawManifest> for Manifest {
    type Error = anyhow::Error;

    fn try_from(raw: RawManifest) -> Result<Self> {
        let name = required(raw.name, "agent: `name` is required")?;
        if raw.targets.is_empty() {
            bail!("agent: at least one `[[targets]]` is required");
        }

        let program = required(raw.build.program, "agent: `build.program` is required")?;
        if !raw
            .build
            .args
            .iter()
            .chain(raw.build.env.values())
            .any(|value| value.contains("{out}"))
        {
            bail!("agent: `build.args` or `build.env` must contain the `{{out}}` placeholder");
        }
        for key in raw.build.env.keys() {
            if key.trim().is_empty() || key.contains('=') {
                bail!("agent: build environment key `{key}` is invalid");
            }
        }
        let build = BuildSpec {
            program,
            args: raw.build.args,
            env: raw.build.env,
        };

        if raw.health.args.is_empty() {
            bail!("agent: `health.args` must contain at least one argument");
        }
        if raw.health.args.iter().any(|arg| arg.contains('\0')) {
            bail!("agent: `health.args` must not contain NUL bytes");
        }
        if raw.health.attempts == 0 {
            bail!("agent: `health.attempts` must be positive");
        }
        if raw.health.interval_ms == 0 {
            bail!("agent: `health.interval_ms` must be positive");
        }
        if raw.health.timeout_ms == 0 {
            bail!("agent: `health.timeout_ms` must be positive");
        }
        let health = HealthSpec {
            args: raw.health.args,
            attempts: raw.health.attempts,
            interval: Duration::from_millis(raw.health.interval_ms),
            timeout: Duration::from_millis(raw.health.timeout_ms),
        };

        let mut seen = HashSet::new();
        let mut targets = Vec::with_capacity(raw.targets.len());
        for raw_target in raw.targets {
            let target = Target::try_from(raw_target)?;
            if !seen.insert(target.name.clone()) {
                bail!("agent: duplicate target name `{}`", target.name);
            }
            targets.push(target);
        }

        Ok(Self {
            name,
            build,
            health,
            targets,
        })
    }
}

impl TryFrom<RawTarget> for Target {
    type Error = anyhow::Error;

    fn try_from(raw: RawTarget) -> Result<Self> {
        let name = required(raw.name, "agent target: `name` is required")?;
        let location = match (raw.local, raw.ssh) {
            (true, None) => Location::Local,
            (false, Some(host)) => {
                let host = required(
                    host,
                    &format!("agent target `{name}`: `ssh` must not be empty"),
                )?;
                if host.starts_with('-') || !host.chars().all(is_ssh_host_char) {
                    bail!("agent target `{name}`: `ssh` is not a valid SSH host");
                }
                Location::Ssh { host }
            }
            (true, Some(_)) => bail!("agent target `{name}`: set `local` or `ssh`, not both"),
            (false, None) => {
                bail!("agent target `{name}`: set `local = true` or `ssh = \"user@host\"`")
            }
        };
        let os = required(raw.os, &format!("agent target `{name}`: `os` is required"))?;
        let platform = Platform {
            os,
            arch: required(
                raw.arch,
                &format!("agent target `{name}`: `arch` is required"),
            )?,
        };
        let destination = Destination::parse(raw.dest, &name)?;
        let service = match (raw.launchd, raw.systemd_user) {
            (Some(label), None) => {
                let label = required(
                    label,
                    &format!("agent target `{name}`: `launchd` must not be empty"),
                )?;
                UserService::launchd(label).with_context(|| format!("agent target `{name}`"))?
            }
            (None, Some(unit)) => {
                let unit = required(
                    unit,
                    &format!("agent target `{name}`: `systemd_user` must not be empty"),
                )?;
                UserService::systemd_user(unit).with_context(|| format!("agent target `{name}`"))?
            }
            (Some(_), Some(_)) => {
                bail!("agent target `{name}`: set `launchd` or `systemd_user`, not both")
            }
            (None, None) => bail!("agent target `{name}`: set `launchd` or `systemd_user`"),
        };
        match (platform.os.as_str(), service.manager()) {
            ("darwin", UserServiceManager::Launchd) | ("linux", UserServiceManager::Systemd) => {}
            ("darwin", UserServiceManager::Systemd) => {
                bail!("agent target `{name}`: `os = \"darwin\"` requires `launchd`")
            }
            ("linux", UserServiceManager::Launchd) => {
                bail!("agent target `{name}`: `os = \"linux\"` requires `systemd_user`")
            }
            (os, _) => bail!("agent target `{name}`: unsupported operating system `{os}`"),
        }

        Ok(Self {
            name,
            location,
            platform,
            destination,
            service,
        })
    }
}

fn required(value: String, message: &str) -> Result<String> {
    if value.trim().is_empty() {
        bail!("{message}");
    }
    Ok(value)
}

fn is_ssh_host_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "._-@".contains(ch)
}

fn is_remote_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "_./:=+-,@%".contains(ch)
}

fn load(path: &Path) -> Result<Manifest> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let raw: RawManifest =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Manifest::try_from(raw)
}

// ── pure deployment planning ────────────────────────────────────────────────

#[derive(Debug)]
struct BuildCommand {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

impl BuildCommand {
    fn plan(spec: &BuildSpec, target: &Target, output: &Path) -> Self {
        let render = |value: &str| {
            value
                .replace("{os}", &target.platform.os)
                .replace("{arch}", &target.platform.arch)
                .replace("{out}", &output.to_string_lossy())
        };
        Self {
            program: spec.program.clone(),
            args: spec.args.iter().map(|arg| render(arg)).collect(),
            env: spec
                .env
                .iter()
                .map(|(key, value)| (key.clone(), render(value)))
                .collect(),
        }
    }

    fn display(&self) -> String {
        let mut parts: Vec<String> = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_word(value)))
            .collect();
        parts.push(shell_word(&self.program));
        parts.extend(self.args.iter().map(|arg| shell_word(arg)));
        parts.join(" ")
    }

    fn run(&self, dir: &Path) -> Result<()> {
        let mut command = Command::new(&self.program);
        command.args(&self.args).envs(&self.env).current_dir(dir);
        let status = command
            .status()
            .with_context(|| format!("spawning: {}", self.display()))?;
        if !status.success() {
            bail!("command exited with {status}: {}", self.display());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PlannedTarget<'a> {
    target: &'a Target,
    artifact: PathBuf,
    build: BuildCommand,
}

#[derive(Debug)]
struct DeploymentPlan<'a> {
    name: &'a str,
    source_dir: &'a Path,
    health: &'a HealthSpec,
    transaction: String,
    targets: Vec<PlannedTarget<'a>>,
}

impl<'a> DeploymentPlan<'a> {
    fn create(
        manifest: &'a Manifest,
        source_dir: &'a Path,
        only: Option<&str>,
        workdir: &Path,
    ) -> Result<Self> {
        let selected = select(manifest, only)?;
        let targets = selected
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                // Artifact locations are generated rather than derived from
                // manifest names, so configuration cannot escape the temp dir.
                let artifact = workdir.join(format!("target-{index}")).join("agent");
                let build = BuildCommand::plan(&manifest.build, target, &artifact);
                PlannedTarget {
                    target,
                    artifact,
                    build,
                }
            })
            .collect();
        Ok(Self {
            name: &manifest.name,
            source_dir,
            health: &manifest.health,
            transaction: workdir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect(),
            targets,
        })
    }

    fn print(&self) {
        for planned in &self.targets {
            let target = planned.target;
            println!("\n════ {} → {} ════", self.name, target.name);
            println!(
                "  build:   ({}/{}) {}",
                target.platform.os,
                target.platform.arch,
                planned.build.display()
            );
            println!(
                "  install: {} → {}",
                location_name(&target.location),
                target.destination.display()
            );
            println!("  restart: {}", target.service.remote_restart_description());
            println!(
                "  verify:  {} {} ({} attempts, {} ms each)",
                target.destination.display(),
                self.health
                    .args
                    .iter()
                    .map(|arg| shell_word(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
                self.health.attempts,
                self.health.timeout.as_millis()
            );
        }
    }

    fn execute(&self) -> Result<()> {
        let recorder = journal::Recorder::start();
        self.execute_recording(&recorder)
    }

    fn execute_recording(&self, recorder: &journal::Recorder) -> Result<()> {
        let build_started = Instant::now();
        let mut artifact_hashes = Vec::with_capacity(self.targets.len());
        // Preparation phase: prove every selected target builds before changing
        // any machine. Installation begins only after this entire loop succeeds.
        for (index, planned) in self.targets.iter().enumerate() {
            let target = planned.target;
            println!(
                "\n════ BUILD {} → {} ({}/{}) ════",
                self.name, target.name, target.platform.os, target.platform.arch
            );
            let result = (|| {
                let parent = planned
                    .artifact
                    .parent()
                    .context("planned artifact has no parent")?;
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating build output dir {}", parent.display()))?;
                println!("==> {}", planned.build.display());
                planned.build.run(self.source_dir)?;
                if !planned.artifact.is_file() {
                    bail!("build produced no binary at {}", planned.artifact.display());
                }
                transaction::artifact_sha256(&planned.artifact)
            })();
            match result {
                Ok(hash) => artifact_hashes.push(hash),
                Err(error) => {
                    recorder.record_build_failure(self, &artifact_hashes, index, &error);
                    return Err(error);
                }
            }
        }

        let build_elapsed = build_started.elapsed();
        let transaction_started = Instant::now();
        let execution = transaction::execute(self, artifact_hashes);
        let transaction_elapsed = transaction_started.elapsed();
        let deployed = execution.deployed();
        recorder.record_transaction(self, build_elapsed, transaction_elapsed, &execution);
        let result = execution.into_result();
        if deployed {
            println!("\n✓ {} deployed to: {}", self.name, self.target_names());
        }
        result
    }

    fn target_names(&self) -> String {
        self.targets
            .iter()
            .map(|planned| planned.target.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn select<'a>(manifest: &'a Manifest, only: Option<&str>) -> Result<Vec<&'a Target>> {
    let Some(only) = only else {
        return Ok(manifest.targets.iter().collect());
    };
    let wanted: Vec<&str> = only
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if wanted.is_empty() {
        bail!("--only requires at least one target name");
    }
    let mut selected = Vec::with_capacity(wanted.len());
    let mut seen = HashSet::with_capacity(wanted.len());
    for name in wanted {
        if !seen.insert(name) {
            bail!("--only: duplicate target name `{name}`");
        }
        match manifest.targets.iter().find(|target| target.name == name) {
            Some(target) => selected.push(target),
            None => bail!("--only: no agent target named `{name}`"),
        }
    }
    Ok(selected)
}

// ── execution ───────────────────────────────────────────────────────────────

/// `tugboat agent deploy` — plan, build, and install the agent targets.
pub fn deploy(manifest_path: &Path, only: Option<&str>, dry_run: bool) -> Result<()> {
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("agent manifest not found: {}", manifest_path.display()))?;
    let source_dir = manifest_path
        .parent()
        .context("manifest has no parent directory")?;
    let manifest = load(&manifest_path)?;

    if dry_run {
        let display_workdir = std::env::temp_dir().join("tugboat-agent-<workdir>");
        DeploymentPlan::create(&manifest, source_dir, only, &display_workdir)?.print();
        return Ok(());
    }

    let workdir = tempfile::Builder::new()
        .prefix("tugboat-agent-")
        .tempdir()
        .context("creating agent build directory")?;
    DeploymentPlan::create(&manifest, source_dir, only, workdir.path())?.execute()
}

fn remote_restart_script(service: &UserService) -> String {
    let name = transport::shell_quote(service.name());
    match service.manager() {
        UserServiceManager::Launchd => {
            format!("uid=$(id -u)\nlaunchctl kickstart -k \"gui/$uid\"/{name}")
        }
        UserServiceManager::Systemd => format!("systemctl --user restart {name}"),
    }
}

fn location_name(location: &Location) -> &str {
    match location {
        Location::Local => "local",
        Location::Ssh { host } => host,
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_./:=+-,@%".contains(ch))
    {
        value.to_owned()
    } else {
        transport::shell_quote(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_text(text: &str) -> Result<Manifest> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, text)?;
        load(&path)
    }

    const MANIFEST: &str = r#"
name = "clipd"

[build]
program = "go"
args = ["build", "-o", "{out}", "./cmd/clipd"]

[build.env]
GOOS = "{os}"
GOARCH = "{arch}"
CGO_ENABLED = "0"

[health]
args = ["health"]
attempts = 3
interval_ms = 10
timeout_ms = 100

[[targets]]
name = "mac"
local = true
os = "darwin"
arch = "arm64"
dest = "~/.local/bin/clipd"
launchd = "com.example.clipd"

[[targets]]
name = "desktop"
ssh = "desktop"
os = "linux"
arch = "amd64"
dest = "~/.local/bin/clipd"
systemd_user = "clipd"
"#;

    #[test]
    fn raw_manifest_becomes_valid_domain_types() {
        let manifest = load_text(MANIFEST).unwrap();
        assert_eq!(manifest.name, "clipd");
        assert!(matches!(manifest.targets[0].location, Location::Local));
        assert!(matches!(
            &manifest.targets[0].service,
            UserService::Launchd { label } if label == "com.example.clipd"
        ));
        assert!(matches!(
            &manifest.targets[1].location,
            Location::Ssh { host } if host == "desktop"
        ));
        assert!(matches!(
            &manifest.targets[1].service,
            UserService::SystemdUser { unit } if unit == "clipd"
        ));
    }

    #[test]
    fn deployment_plan_resolves_a_structured_build_command() {
        let manifest = load_text(MANIFEST).unwrap();
        let plan = DeploymentPlan::create(
            &manifest,
            Path::new("/source"),
            Some("desktop"),
            Path::new("/tmp/agent-plan"),
        )
        .unwrap();
        let planned = &plan.targets[0];

        assert_eq!(planned.build.program, "go");
        assert_eq!(
            planned.build.args,
            [
                "build",
                "-o",
                "/tmp/agent-plan/target-0/agent",
                "./cmd/clipd"
            ]
        );
        assert_eq!(
            planned.build.env.get("GOOS").map(String::as_str),
            Some("linux")
        );
        assert_eq!(
            planned.build.env.get("GOARCH").map(String::as_str),
            Some("amd64")
        );
        assert_eq!(
            planned.build.env.get("CGO_ENABLED").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn rejects_a_target_without_a_restart_manager() {
        let error = load_text(
            r#"
name = "tool"
[build]
program = "go"
args = ["build", "-o", "{out}"]
[health]
args = ["health"]
attempts = 3
interval_ms = 10
timeout_ms = 100
[[targets]]
name = "mac"
local = true
os = "darwin"
arch = "arm64"
dest = "~/.local/bin/tool"
"#,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("set `launchd` or `systemd_user`"));
    }

    #[test]
    fn rejects_a_build_without_an_output_placeholder() {
        let error = load_text(
            r#"
name = "tool"
[build]
program = "go"
args = ["build", "./cmd/tool"]
[health]
args = ["health"]
attempts = 3
interval_ms = 10
timeout_ms = 100
[[targets]]
name = "mac"
local = true
os = "darwin"
arch = "arm64"
dest = "~/.local/bin/tool"
launchd = "com.example.tool"
"#,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("must contain the `{out}` placeholder"));
    }

    #[test]
    fn rejects_an_unbounded_health_policy() {
        let error = load_text(
            r#"
name = "tool"
[build]
program = "go"
args = ["build", "-o", "{out}"]
[health]
args = ["health"]
attempts = 0
interval_ms = 10
timeout_ms = 100
[[targets]]
name = "mac"
local = true
os = "darwin"
arch = "arm64"
dest = "~/.local/bin/tool"
launchd = "com.example.tool"
"#,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("`health.attempts` must be positive"));
    }

    #[test]
    fn rejects_a_service_manager_that_does_not_match_the_platform() {
        let error = load_text(
            r#"
name = "tool"
[build]
program = "go"
args = ["build", "-o", "{out}"]
[health]
args = ["health"]
attempts = 3
interval_ms = 10
timeout_ms = 100
[[targets]]
name = "mac"
local = true
os = "darwin"
arch = "arm64"
dest = "~/.local/bin/tool"
systemd_user = "tool"
"#,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("`os = \"darwin\"` requires `launchd`"));
    }

    #[test]
    fn rejects_destination_shell_metacharacters() {
        let error = Destination::parse("~/.local/bin/tool;rm".to_owned(), "mac").unwrap_err();
        assert!(error
            .to_string()
            .contains("characters unsupported by the agent transport"));
    }

    #[test]
    fn rejects_duplicate_only_targets() {
        let manifest = load_text(MANIFEST).unwrap();
        let error = select(&manifest, Some("mac,mac")).unwrap_err();
        assert!(error.to_string().contains("duplicate target name `mac`"));
    }

    #[test]
    fn remote_restart_scripts_keep_service_values_typed_until_rendering() {
        let launchd = UserService::launchd("com.example.clipd".to_owned()).unwrap();
        assert_eq!(
            remote_restart_script(&launchd),
            "uid=$(id -u)\nlaunchctl kickstart -k \"gui/$uid\"/'com.example.clipd'"
        );

        let systemd = UserService::systemd_user("clipd".to_owned()).unwrap();
        assert_eq!(
            remote_restart_script(&systemd),
            "systemctl --user restart 'clipd'"
        );
    }

    #[test]
    fn every_build_finishes_before_any_install_begins() {
        let dir = tempfile::tempdir().unwrap();
        let first_dest = dir.path().join("first");
        let second_dest = dir.path().join("second");
        std::fs::write(&first_dest, "old-first").unwrap();
        std::fs::write(&second_dest, "old-second").unwrap();
        let manifest = load_text(&format!(
            r#"
name = "clipd"
[build]
program = "sh"
args = ["-c", 'if [ "$1" = fail ]; then exit 1; else printf built > "$2"; fi', "_", "{{arch}}", "{{out}}"]

[health]
args = ["health"]
attempts = 3
interval_ms = 10
timeout_ms = 100

[[targets]]
name = "first"
local = true
os = "linux"
arch = "ok"
dest = "{}"
systemd_user = "never-restarted"

[[targets]]
name = "second"
local = true
os = "linux"
arch = "fail"
dest = "{}"
systemd_user = "never-restarted"
"#,
            first_dest.display(),
            second_dest.display()
        ))
        .unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let plan = DeploymentPlan::create(&manifest, dir.path(), None, workdir.path()).unwrap();
        let journal_path = workdir.path().join("agent-deploys.jsonl");
        let recorder = journal::Recorder::for_path(journal_path);

        assert!(plan.execute_recording(&recorder).is_err());
        assert_eq!(std::fs::read_to_string(first_dest).unwrap(), "old-first");
        assert_eq!(std::fs::read_to_string(second_dest).unwrap(), "old-second");
    }
}
