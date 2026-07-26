//! The tool set: read_file, write_file, edit_file, run_command, search
//! (ripgrep), ask_user. JSON schemas + local functions, nothing more.
//!
//! Tools are session-scoped through [ToolCtx]: relative paths resolve against
//! the session's working directory (not the process's), commands run with it
//! as their cwd, and cancellation is a per-session flag — so concurrent web
//! sessions never share a process-global cwd or interrupt bit.

use crate::agent::TurnIo;
use crate::util::{arg_str, resolve_path, truncate_chars};
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CMD_TIMEOUT: Duration = Duration::from_secs(120);
const OUT_CAP: usize = 12_000; // chars returned from run_command
const READ_CAP: usize = 2_000; // lines returned from read_file
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-session tool execution context.
#[derive(Clone)]
pub struct ToolCtx {
    /// Working directory relative paths resolve against and commands run in.
    pub cwd: PathBuf,
    /// Yolo mode: run commands without confirmation.
    pub yolo: bool,
    /// Set to abort in-flight commands (Ctrl-C in the REPL, Stop in the web UI).
    pub cancel: Arc<AtomicBool>,
}

fn tool_read_file(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let offset = args["offset"].as_i64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().map(|l| (l as usize).min(READ_CAP)).unwrap_or(READ_CAP);
    let text = std::fs::read_to_string(resolve_path(&ctx.cwd, path))
        .map_err(|e| format!("error: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = (offset - 1).min(lines.len());
    let end = (start + limit).min(lines.len());
    let mut out: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", start + i + 1, l))
        .collect::<Vec<_>>()
        .join("\n");
    let remaining = lines.len() - end;
    if remaining > 0 {
        out.push_str(&format!("\n... ({remaining} more lines)"));
    }
    Ok(if out.is_empty() { "(empty file)".into() } else { out })
}

fn tool_write_file(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let content = arg_str(args, "content")?;
    let p = resolve_path(&ctx.cwd, path);
    if let Some(parent) = p.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("error: {e}"))?;
        }
    std::fs::write(&p, content).map_err(|e| format!("error: {e}"))?;
    Ok(format!("wrote {} bytes to {}", content.len(), p.display()))
}

fn tool_edit_file(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let old = arg_str(args, "old_string")?;
    let new = arg_str(args, "new_string")?;
    if old.is_empty() {
        return Err("error: old_string must not be empty".into());
    }
    let p = resolve_path(&ctx.cwd, path);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("error: {e}"))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err("error: old_string not found in file".into());
    }
    if count > 1 {
        return Err(format!("error: old_string matches {count} times; make it unique"));
    }
    std::fs::write(&p, text.replacen(old, new, 1)).map_err(|e| format!("error: {e}"))?;
    Ok(format!("edited {}", p.display()))
}

fn tool_run_command(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let cmd = arg_str(args, "command")?;
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd).current_dir(&ctx.cwd);
    let (code, stdout, stderr) = run_captured(&mut command, CMD_TIMEOUT, &ctx.cancel)?;
    let mut out = stdout + &stderr;
    if out.chars().count() > OUT_CAP {
        let total = out.chars().count();
        out = truncate_chars(&out, OUT_CAP);
        out.push_str(&format!("\n... (truncated, {total} chars total)"));
    }
    Ok(if code != 0 {
        format!("exit {code}\n{out}")
    } else if out.is_empty() {
        "(no output)".into()
    } else {
        out
    })
}

/// Spawn a command, capture stdout+stderr, and enforce a timeout and the
/// session's cancel flag. Returns (exit code, stdout, stderr). Blocking — call
/// via spawn_blocking.
///
/// Reader threads stream both pipes into shared buffers (so a chatty child
/// can't deadlock on a full pipe, and so a *timeout* can still report what
/// the command produced before it was killed). The child runs in its own
/// process group so a timeout/interrupt kills the whole tree — otherwise a
/// hung grandchild would survive as an orphan.
pub fn run_captured(
    command: &mut Command,
    timeout: Duration,
    cancel: &Arc<AtomicBool>,
) -> Result<(i32, String, String), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("error: failed to spawn `{program}`: {e}"))?;

    let stdout = Arc::new(Mutex::new(String::new()));
    let stderr = Arc::new(Mutex::new(String::new()));
    let t_out = spawn_reader(child.stdout.take().expect("piped stdout"), Arc::clone(&stdout));
    let t_err = spawn_reader(child.stderr.take().expect("piped stderr"), Arc::clone(&stderr));

    let start = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if cancel.load(Ordering::SeqCst) {
                    kill_tree(&mut child);
                    return Err("error: interrupted by user".into());
                }
                if start.elapsed() > timeout {
                    kill_tree(&mut child);
                    return Err(timeout_report(timeout, &lock(&stdout), &lock(&stderr)));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("error: {e}")),
        }
    };
    // Child exited: join the readers so we collect output through EOF.
    // (A detached grandchild that inherited a pipe can still hold it open —
    // same behavior as before this change.)
    t_out.join().ok();
    t_err.join().ok();
    let stdout = std::mem::take(&mut *lock(&stdout));
    let stderr = std::mem::take(&mut *lock(&stderr));
    Ok((code, stdout, stderr))
}

/// Stream a pipe into a shared string buffer until EOF. Chunked + lossy so
/// partial output is always visible to the caller (even mid-read) and invalid
/// UTF-8 can't silently drop the whole stream.
fn spawn_reader(mut pipe: impl Read + Send + 'static, buf: Arc<Mutex<String>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => lock(&buf).push_str(&String::from_utf8_lossy(&chunk[..n])),
            }
        }
    })
}

fn lock(buf: &Arc<Mutex<String>>) -> std::sync::MutexGuard<'_, String> {
    buf.lock().unwrap_or_else(|e| e.into_inner())
}

/// Kill the child and, on unix, its whole process group so grandchildren
/// can't survive a timeout/interrupt as orphans.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        // Negative pid = process group. Safe: the group was created by us via
        // process_group(0) at spawn; ESRCH (already dead) is fine.
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    child.kill().ok(); // covers non-unix and the group-less fallback
    child.wait().ok();
}

/// Build the timeout error from whatever the command produced before it was
/// killed, so the model can see *where* it got stuck instead of guessing.
fn timeout_report(timeout: Duration, stdout: &str, stderr: &str) -> String {
    let mut msg =
        format!("error: command timed out after {}s (process tree killed)", timeout.as_secs());
    let mut captured = String::new();
    if !stdout.trim().is_empty() {
        captured.push_str("\n--- captured stdout before kill ---\n");
        captured.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        captured.push_str("\n--- captured stderr before kill ---\n");
        captured.push_str(stderr.trim_end());
    }
    if captured.is_empty() {
        msg.push_str("\n(no output captured before kill — it was likely blocked: waiting on the network, a hung child, or a prompt)");
    } else {
        if captured.chars().count() > OUT_CAP {
            let total = captured.chars().count();
            captured = truncate_chars(&captured, OUT_CAP);
            captured.push_str(&format!("\n... (truncated, {total} chars total)"));
        }
        msg.push_str(&captured);
    }
    msg
}

/// Search file contents via ripgrep. Read-only, so no yolo confirmation.
fn tool_search(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let pattern = arg_str(args, "pattern")?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let mut cmd = Command::new("rg");
    // Machine-friendly output; --max-columns keeps minified files from flooding the result.
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color")
        .arg("never")
        .arg("--max-columns")
        .arg("500");
    if args["ignore_case"].as_bool().unwrap_or(false) {
        cmd.arg("--ignore-case");
    }
    if let Some(glob) = args.get("glob").and_then(|v| v.as_str()) {
        cmd.arg("--glob").arg(glob);
    }
    cmd.arg("--").arg(pattern).arg(resolve_path(&ctx.cwd, path));

    let (code, stdout, stderr) = run_captured(&mut cmd, SEARCH_TIMEOUT, &ctx.cancel)?;
    match code {
        0 => {
            let mut out = stdout;
            if out.chars().count() > OUT_CAP {
                let total = out.chars().count();
                out = truncate_chars(&out, OUT_CAP);
                out.push_str(&format!("\n... (truncated, {total} chars total)"));
            }
            Ok(out)
        }
        // rg exit code 1: no matches — a normal result, not an error.
        1 => Ok("(no matches)".into()),
        _ => Err(format!("error: rg failed: {}", stderr.trim())),
    }
}

/// Run one tool call. `io` supplies the user interaction some tools need:
/// ask_user answers and (outside yolo mode) run_command confirmation.
pub async fn dispatch<T: TurnIo>(ctx: &ToolCtx, io: &mut T, name: &str, args: Value) -> String {
    let result = match name {
        "read_file" => tool_read_file(ctx, &args),
        "write_file" => tool_write_file(ctx, &args),
        "edit_file" => tool_edit_file(ctx, &args),
        "ask_user" => match arg_str(&args, "question") {
            Ok(question) => io.ask(question).await,
            Err(e) => Err(e),
        },
        "run_command" => match arg_str(&args, "command") {
            Ok(cmd) => {
                if !ctx.yolo && !io.confirm(cmd).await {
                    Err("error: user denied this command".to_string())
                } else {
                    let ctx = ctx.clone();
                    let args = args.clone();
                    // Long-running child processes: keep them off the async
                    // runtime threads so signals and streaming stay responsive.
                    tokio::task::spawn_blocking(move || tool_run_command(&ctx, &args))
                        .await
                        .unwrap_or_else(|e| Err(format!("error: tool task failed: {e}")))
                }
            }
            Err(e) => Err(e),
        },
        "search" => {
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || tool_search(&ctx, &args))
                .await
                .unwrap_or_else(|e| Err(format!("error: tool task failed: {e}")))
        }
        _ => Err(format!("error: unknown tool {name}")),
    };
    match result {
        Ok(s) => s,
        Err(e) => e,
    }
}

pub fn tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file. Returns numbered lines.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "absolute, ~/, or relative to the session working directory" },
                        "offset": { "type": "integer", "description": "1-based start line" },
                        "limit": { "type": "integer", "description": "max lines (default 2000)" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write (create or overwrite) a file with the given content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "absolute, ~/, or relative to the session working directory" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Replace a unique exact string in a file with a new string.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "absolute, ~/, or relative to the session working directory" },
                        "old_string": { "type": "string", "description": "must match exactly once" },
                        "new_string": { "type": "string" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search file contents with ripgrep (regex). Returns matching lines as path:line:text. Respects .gitignore and skips hidden files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "regex pattern (ripgrep syntax)" },
                        "path": { "type": "string", "description": "file or directory to search (default: session working directory)" },
                        "glob": { "type": "string", "description": "limit search to files matching a glob, e.g. \"*.rs\"" },
                        "ignore_case": { "type": "boolean", "description": "case-insensitive search (default false)" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a shell command in the session working directory and return stdout+stderr and exit code.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": "Ask the user a clarifying question and return their typed answer. Use when the request is ambiguous or a decision is needed; prefer asking over guessing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "the question to ask the user" }
                    },
                    "required": ["question"]
                }
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn timeout_reports_partial_output() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo out-line; echo err-line 1>&2; sleep 30");
        let err = run_captured(&mut cmd, Duration::from_secs(1), &no_cancel()).unwrap_err();
        assert!(err.contains("timed out after 1s"), "{err}");
        assert!(err.contains("captured stdout"), "{err}");
        assert!(err.contains("out-line"), "{err}");
        assert!(err.contains("captured stderr"), "{err}");
        assert!(err.contains("err-line"), "{err}");
    }

    #[test]
    fn timeout_with_no_output_says_so() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let err = run_captured(&mut cmd, Duration::from_secs(1), &no_cancel()).unwrap_err();
        assert!(err.contains("timed out after 1s"), "{err}");
        assert!(err.contains("no output captured"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_grandchildren() {
        // A backgrounded grandchild writes a file after 2s — but only if it
        // survives the parent's 1s timeout kill.
        let marker = std::env::temp_dir().join(format!("harness-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("(sleep 2; touch {}) & sleep 30", marker.display()));
        let err = run_captured(&mut cmd, Duration::from_secs(1), &no_cancel()).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "grandchild survived the timeout kill");
        let _ = std::fs::remove_file(&marker);
    }

    #[test]
    fn completed_command_captures_both_streams() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo hi; echo oops 1>&2; exit 3");
        let (code, stdout, stderr) =
            run_captured(&mut cmd, Duration::from_secs(10), &no_cancel()).unwrap();
        assert_eq!(code, 3);
        assert_eq!(stdout.trim(), "hi");
        assert_eq!(stderr.trim(), "oops");
    }

    #[test]
    fn cancel_flag_aborts_command() {
        let cancel = Arc::new(AtomicBool::new(true));
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let err = run_captured(&mut cmd, Duration::from_secs(30), &cancel).unwrap_err();
        assert!(err.contains("interrupted"), "{err}");
    }
}
