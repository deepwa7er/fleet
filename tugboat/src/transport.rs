//! Reusable SSH and rsync transport primitives.
//!
//! Remote shell text is executed only at this boundary. Higher-level workflows
//! provide typed values and complete scripts; this module owns the child-process
//! shapes used to deliver them.

use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::subprocess::{
    run_captured_timeout, run_captured_timeout_file, run_streamed, CapturedOutput, LogSink,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsyncKind {
    File,
    Directory,
}

/// Ship a file or directory to an rsync remote destination.
pub fn rsync(local: &Path, remote: &str, kind: RsyncKind, log: &dyn LogSink) -> Result<()> {
    let (command, from, to) = rsync_command(local, remote, kind);
    let status = run_streamed(command, None, log).context("spawning rsync")?;
    if !status.success() {
        bail!("rsync failed: {from} → {to}");
    }
    Ok(())
}

fn rsync_command(local: &Path, remote: &str, kind: RsyncKind) -> (Command, String, String) {
    let mut command = Command::new("rsync");
    command.args(["-az", "--no-owner", "--no-group"]);

    let (from, to) = match kind {
        RsyncKind::File => (local.display().to_string(), remote.to_owned()),
        RsyncKind::Directory => {
            command.arg("--delete");
            (format!("{}/", local.display()), format!("{remote}/"))
        }
    };
    command.arg(&from).arg(&to);
    (command, from, to)
}

/// Execute a Bash script on `host`, providing the script through stdin so its
/// content never has to survive a local shell or SSH command-line interpolation.
pub fn ssh_script(host: &str, script: &str, log: &dyn LogSink) -> Result<()> {
    let mut command = Command::new("ssh");
    command.arg(host).arg("bash -s");
    let status = run_streamed(command, Some(script.as_bytes()), log).context("spawning ssh")?;
    if !status.success() {
        bail!("remote script exited with {status}");
    }
    Ok(())
}

/// Execute a remote command and capture its output with a hard deadline.
pub fn ssh_capture(host: &str, remote_command: &str, timeout: Duration) -> Result<CapturedOutput> {
    let mut command = Command::new("ssh");
    command.arg(host).arg(remote_command);
    run_captured_timeout(command, None, timeout).context("spawning ssh")
}

/// Execute a remote Bash script through stdin and capture its bounded output.
pub fn ssh_script_capture(host: &str, script: &str, timeout: Duration) -> Result<CapturedOutput> {
    let mut command = Command::new("ssh");
    command.arg(host).arg("bash -s");
    run_captured_timeout(command, Some(script.as_bytes()), timeout).context("spawning ssh")
}

/// Execute a remote command with a file streamed directly to stdin.
pub fn ssh_pipe_file_quiet(
    host: &str,
    remote_command: &str,
    stdin: File,
    timeout: Duration,
) -> Result<()> {
    let mut command = Command::new("ssh");
    command.arg(host).arg(remote_command);
    let output =
        run_captured_timeout_file(command, stdin, timeout).context("running remote command")?;
    if !output.status.success() {
        bail!("ssh exited {}: {}", output.status, output.stderr.trim());
    }
    Ok(())
}

/// Quote one value as one POSIX shell word for a remote script.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn file_rsync_preserves_the_exact_endpoints() {
        let (command, from, to) = rsync_command(
            Path::new("/tmp/tool"),
            "host:~/.local/bin/tool.tug-new",
            RsyncKind::File,
        );
        assert_eq!(from, "/tmp/tool");
        assert_eq!(to, "host:~/.local/bin/tool.tug-new");
        assert_eq!(
            argv(&command),
            [
                "rsync",
                "-az",
                "--no-owner",
                "--no-group",
                "/tmp/tool",
                "host:~/.local/bin/tool.tug-new"
            ]
        );
    }

    #[test]
    fn directory_rsync_adds_delete_and_trailing_slashes() {
        let (command, from, to) = rsync_command(
            Path::new("/tmp/site"),
            "host:/srv/site.tug-new",
            RsyncKind::Directory,
        );
        assert_eq!(from, "/tmp/site/");
        assert_eq!(to, "host:/srv/site.tug-new/");
        assert_eq!(
            argv(&command),
            [
                "rsync",
                "-az",
                "--no-owner",
                "--no-group",
                "--delete",
                "/tmp/site/",
                "host:/srv/site.tug-new/"
            ]
        );
    }

    #[test]
    fn shell_quote_keeps_a_value_in_one_word() {
        assert_eq!(shell_quote("one'two"), "'one'\\''two'");
    }
}
