//! Typed launchd and systemd-user service operations.
//!
//! Both agent deploys and Tugboat self-deploy restart per-user daemons. This
//! module is the single authority for their command shapes and executes local
//! restarts directly, without an intermediary shell.

use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::subprocess::run_captured_timeout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserServiceManager {
    Launchd,
    Systemd,
}

impl UserServiceManager {
    pub fn service(self, name: String) -> Result<UserService> {
        match self {
            Self::Launchd => UserService::launchd(name),
            Self::Systemd => UserService::systemd_user(name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserService {
    Launchd { label: String },
    SystemdUser { unit: String },
}

impl UserService {
    pub fn launchd(label: String) -> Result<Self> {
        validate_name(&label, "launchd label")?;
        Ok(Self::Launchd { label })
    }

    pub fn systemd_user(unit: String) -> Result<Self> {
        validate_name(&unit, "systemd user unit")?;
        Ok(Self::SystemdUser { unit })
    }

    pub fn manager(&self) -> UserServiceManager {
        match self {
            Self::Launchd { .. } => UserServiceManager::Launchd,
            Self::SystemdUser { .. } => UserServiceManager::Systemd,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Launchd { label } => label,
            Self::SystemdUser { unit } => unit,
        }
    }

    /// Build the local restart command. Resolving the numeric uid is necessary
    /// only for launchd's GUI domain.
    pub fn restart_command(&self) -> Result<ServiceCommand> {
        match self {
            Self::Launchd { .. } => Ok(self.restart_command_for_uid(current_uid()?)),
            Self::SystemdUser { .. } => Ok(self.restart_command_for_uid(0)),
        }
    }

    fn restart_command_for_uid(&self, uid: u32) -> ServiceCommand {
        match self {
            Self::Launchd { label } => ServiceCommand {
                program: "launchctl",
                args: vec![
                    "kickstart".to_owned(),
                    "-k".to_owned(),
                    format!("gui/{uid}/{label}"),
                ],
            },
            Self::SystemdUser { unit } => ServiceCommand {
                program: "systemctl",
                args: vec!["--user".to_owned(), "restart".to_owned(), unit.clone()],
            },
        }
    }

    /// Human-readable remote command for plans. The `$(id -u)` expression is
    /// descriptive only; actual remote rendering stays in the SSH boundary.
    pub fn remote_restart_description(&self) -> String {
        match self {
            Self::Launchd { label } => {
                format!("launchctl kickstart -k gui/$(id -u)/'{label}'")
            }
            Self::SystemdUser { unit } => {
                format!("systemctl --user restart '{unit}'")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceCommand {
    program: &'static str,
    args: Vec<String>,
}

impl ServiceCommand {
    pub fn display(&self) -> String {
        std::iter::once(self.program.to_owned())
            .chain(self.args.iter().map(|arg| quote_for_display(arg)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn run(&self) -> Result<()> {
        let status = Command::new(self.program)
            .args(&self.args)
            .status()
            .with_context(|| format!("spawning `{}`", self.program))?;
        if !status.success() {
            bail!("command exited with {status}: {}", self.display());
        }
        Ok(())
    }

    pub fn run_timeout(&self, timeout: Duration) -> Result<()> {
        let mut command = Command::new(self.program);
        command.args(&self.args);
        let output = run_captured_timeout(command, None, timeout)
            .with_context(|| format!("running {}", self.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = output.stderr.trim();
        if stderr.is_empty() {
            bail!("command exited with {}: {}", output.status, self.display());
        }
        bail!(
            "command exited with {}: {}: {stderr}",
            output.status,
            self.display()
        )
    }
}

fn validate_name(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "._-@".contains(ch))
    {
        bail!("{field} `{value}` contains unsupported characters");
    }
    Ok(())
}

fn current_uid() -> Result<u32> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("spawning `id -u`")?;
    if !output.status.success() {
        bail!("`id -u` failed");
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("parsing uid from `id -u`")
}

fn quote_for_display(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_./:=+-,@%".contains(ch))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_restart_is_structured_argv() {
        let service = UserService::systemd_user("tugboat-serve".to_owned()).unwrap();
        let command = service.restart_command_for_uid(501);
        assert_eq!(command.program, "systemctl");
        assert_eq!(command.args, ["--user", "restart", "tugboat-serve"]);
        assert_eq!(command.display(), "systemctl --user restart tugboat-serve");
    }

    #[test]
    fn launchd_restart_uses_the_supplied_gui_uid() {
        let service = UserService::launchd("com.deepwa7er.tugboat-serve".to_owned()).unwrap();
        let command = service.restart_command_for_uid(501);
        assert_eq!(command.program, "launchctl");
        assert_eq!(
            command.args,
            ["kickstart", "-k", "gui/501/com.deepwa7er.tugboat-serve"]
        );
    }

    #[test]
    fn service_names_reject_shell_syntax() {
        assert!(UserService::systemd_user("unit; reboot".to_owned()).is_err());
        assert!(UserService::launchd("$(touch bad)".to_owned()).is_err());
    }
}
