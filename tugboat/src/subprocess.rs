//! Shared child-process execution and transcript streaming.
//!
//! This sits below the deploy workflows so the VPS deploy engine, agent
//! deployer, documentation shipper, and HTTP daemon can share process plumbing
//! without depending on one another's orchestration modules.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use wait_timeout::ChildExt;

/// A destination for subprocess output and workflow progress. Implementations
/// must be `Sync` because stdout and stderr are drained concurrently.
pub trait LogSink: Send + Sync {
    fn line(&self, line: &str);
}

/// Writes each transcript line to this process's stdout.
pub struct StdoutSink;

impl LogSink for StdoutSink {
    fn line(&self, line: &str) {
        // `println!` locks stdout, so concurrent reader threads cannot
        // interleave within a line.
        println!("{line}");
    }
}

#[derive(Debug)]
pub struct CapturedOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Run a child with a hard deadline while capturing both output streams.
///
/// Regular OS pipes can deadlock when a parent waits before draining a verbose
/// child. Temporary files keep the wait bounded regardless of output volume.
pub fn run_captured_timeout(
    mut command: Command,
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Result<CapturedOutput> {
    let mut stdout = tempfile::tempfile().context("creating stdout capture")?;
    let mut stderr = tempfile::tempfile().context("creating stderr capture")?;
    let stdin = if let Some(data) = stdin_data {
        let mut file = tempfile::tempfile().context("creating stdin buffer")?;
        file.write_all(data).context("buffering child stdin")?;
        file.seek(SeekFrom::Start(0))
            .context("rewinding child stdin")?;
        Stdio::from(file)
    } else {
        Stdio::null()
    };
    command
        .stdout(Stdio::from(
            stdout.try_clone().context("cloning stdout capture")?,
        ))
        .stderr(Stdio::from(
            stderr.try_clone().context("cloning stderr capture")?,
        ))
        .stdin(stdin);

    let mut child = command.spawn().context("spawning command")?;
    let Some(status) = child
        .wait_timeout(timeout)
        .context("waiting on child process")?
    else {
        if let Err(error) = child.kill() {
            if error.kind() != std::io::ErrorKind::InvalidInput {
                return Err(error).context("killing timed-out child process");
            }
        }
        child.wait().context("reaping timed-out child process")?;
        bail!("command timed out after {} ms", timeout.as_millis());
    };

    let read_capture = |file: &mut std::fs::File| -> Result<String> {
        file.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    };
    Ok(CapturedOutput {
        status,
        stdout: read_capture(&mut stdout).context("reading captured stdout")?,
        stderr: read_capture(&mut stderr).context("reading captured stderr")?,
    })
}

/// Run a child process, forwarding stdout and stderr into `log` as they arrive.
/// Optional stdin is written concurrently so a child that produces output while
/// consuming input cannot deadlock on full pipes.
pub fn run_streamed(
    mut command: Command,
    stdin_data: Option<&[u8]>,
    log: &dyn LogSink,
) -> Result<ExitStatus> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = command.spawn().context("spawning command")?;
    let stdout = child.stdout.take().context("child stdout unavailable")?;
    let stderr = child.stderr.take().context("child stderr unavailable")?;
    let stdin = child.stdin.take();

    std::thread::scope(|scope| {
        if let (Some(mut stdin), Some(data)) = (stdin, stdin_data) {
            scope.spawn(move || {
                // A broken pipe means the child exited; its status below is the
                // authoritative outcome.
                let _ = stdin.write_all(data);
            });
        }
        scope.spawn(|| pipe_lines(stdout, log));
        scope.spawn(|| pipe_lines(stderr, log));
    });

    child.wait().context("waiting on child process")
}

fn pipe_lines<R: Read>(reader: R, log: &dyn LogSink) {
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(line) => log.line(&line),
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Collect(Mutex<Vec<String>>);

    impl LogSink for Collect {
        fn line(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_owned());
        }
    }

    #[test]
    fn streamed_process_receives_stdin_and_captures_both_output_streams() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "read -r value; printf 'stdout:%s\\n' \"$value\"; printf 'stderr:%s\\n' \"$value\" >&2",
        ]);
        let log = Collect(Mutex::new(Vec::new()));

        let status = run_streamed(command, Some(b"value\n"), &log).unwrap();
        assert!(status.success());

        let mut lines = log.0.lock().unwrap().clone();
        lines.sort();
        assert_eq!(lines, ["stderr:value", "stdout:value"]);
    }

    #[test]
    fn captured_process_returns_both_output_streams() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "read -r value; printf 'out:%s' \"$value\"; printf 'err:%s' \"$value\" >&2",
        ]);

        let output =
            run_captured_timeout(command, Some(b"value\n"), Duration::from_secs(1)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, "out:value");
        assert_eq!(output.stderr, "err:value");
    }

    #[test]
    fn captured_process_is_killed_at_the_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);

        let error = run_captured_timeout(command, None, Duration::from_millis(10)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
