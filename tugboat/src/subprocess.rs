//! Shared child-process execution and transcript streaming.
//!
//! This sits below the deploy workflows so the VPS deploy engine, agent
//! deployer, documentation shipper, and HTTP daemon can share process plumbing
//! without depending on one another's orchestration modules.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

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
}
