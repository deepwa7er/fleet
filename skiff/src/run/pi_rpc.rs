//! A live `pi --mode rpc` child, and the protocol it speaks.
//!
//! Protocol facts this module depends on, all of them load-bearing:
//!
//! - **Framing is strict JSONL, LF only.** Rust's line reader splits on `\n`
//!   and nothing else, which is what the protocol requires. (The Node bridge
//!   had to hand-roll a splitter because `readline` also breaks on U+2028 and
//!   U+2029 — legal characters *inside* a JSON string, so a reply containing
//!   one would corrupt the frame.)
//! - **Responses and events share stdout**, and responses can arrive **out of
//!   order** — a real pi has answered a later `get_state` before an earlier
//!   `new_session`. Correlation is by `id`, never by arrival order.
//! - **Dialog requests block the agent** until answered. There is no human on
//!   the other end of this socket, so they are auto-cancelled by the caller;
//!   a deadlocked prompt would wedge every viewer of that session forever.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// A command's response may legitimately trail its events, so this is a bound
/// on waiting rather than a claim that the command failed.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// stderr is kept only for diagnostics on a pi that dies. Prompt text never
/// appears there — prompts travel stdin.
const STDERR_TAIL_LIMIT: usize = 4_096;

pub struct PiConfig {
    pub binary: PathBuf,
    pub session_file: PathBuf,
    pub cwd: PathBuf,
    /// Passed as `--session-dir` **only** when skiff's session directory was
    /// explicitly overridden.
    ///
    /// pi's native default is the same directory skiff scans, but organised
    /// into per-cwd buckets — the layout pi's own CLI reads and writes.
    /// Forcing `--session-dir` unconditionally would make pi write flat into
    /// the directory root, invisible to the CLI's project-scoped listings, and
    /// silently split sessions started here from sessions started at a
    /// terminal.
    pub session_dir_override: Option<PathBuf>,
}

pub struct PiProcess {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    exited: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<String>>,
}

impl PiProcess {
    /// Spawn pi and start reading its stdout.
    ///
    /// Every event (everything that is not a command response) is forwarded on
    /// the returned receiver. The channel closes when pi exits, which is how
    /// the caller learns the process is gone.
    pub fn spawn(config: &PiConfig) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Value>)> {
        let mut command = Command::new(&config.binary);
        command.arg("--mode").arg("rpc");
        if let Some(dir) = &config.session_dir_override {
            command.arg("--session-dir").arg(dir);
        }
        command.arg("--session").arg(&config.session_file);
        command
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this a killed skiffd leaves pi children running, holding
            // their session files open.
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("spawning {}", config.binary.display()))?;

        let stdin = child.stdin.take().context("pi stdin was not piped")?;
        let stdout = child.stdout.take().context("pi stdout was not piped")?;
        let stderr = child.stderr.take().context("pi stderr was not piped")?;

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> = Arc::default();
        let exited = Arc::new(AtomicBool::new(false));
        let stderr_tail: Arc<Mutex<String>> = Arc::default();
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        tokio::spawn({
            let pending = pending.clone();
            let exited = exited.clone();
            async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // An unparseable line from pi is not worth surfacing; the
                    // next frame is independent.
                    let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                    if value.get("type").and_then(Value::as_str) == Some("response") {
                        let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
                        if let Some(waiter) = pending.lock().await.remove(id) {
                            // A dropped receiver means the command timed out;
                            // the late response is simply discarded.
                            let _ = waiter.send(value);
                        }
                        continue;
                    }
                    if events_tx.send(value).is_err() {
                        break; // nobody is listening any more
                    }
                }
                // stdout closed: pi is gone. Fail every waiter rather than
                // leaving callers to time out one by one.
                exited.store(true, Ordering::SeqCst);
                pending.lock().await.clear();
            }
        });

        tokio::spawn({
            let stderr_tail = stderr_tail.clone();
            async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut tail = stderr_tail.lock().await;
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > STDERR_TAIL_LIMIT {
                        let cut = tail.len() - STDERR_TAIL_LIMIT;
                        // Keep the tail on a character boundary.
                        let cut = (cut..tail.len())
                            .find(|i| tail.is_char_boundary(*i))
                            .unwrap_or(tail.len());
                        *tail = tail[cut..].to_owned();
                    }
                }
            }
        });

        Ok((
            Arc::new(Self {
                stdin: Mutex::new(stdin),
                child: Mutex::new(child),
                pending,
                next_id: AtomicU64::new(0),
                exited,
                stderr_tail,
            }),
            events_rx,
        ))
    }

    pub fn is_alive(&self) -> bool {
        !self.exited.load(Ordering::SeqCst)
    }

    pub async fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().await.clone()
    }

    /// Send one command and wait for the response with the matching id.
    pub async fn command(&self, kind: &str, fields: Value, timeout: Duration) -> Result<Value> {
        if !self.is_alive() {
            bail!("pi is not running");
        }
        let id = (self.next_id.fetch_add(1, Ordering::SeqCst) + 1).to_string();
        let mut frame = json!({ "type": kind, "id": id });
        if let (Some(frame), Value::Object(fields)) = (frame.as_object_mut(), fields) {
            frame.extend(fields);
        }

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        if let Err(err) = self.write(&frame).await {
            self.pending.lock().await.remove(&id);
            return Err(err);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            // The waiter was dropped: pi exited while the command was in
            // flight.
            Ok(Err(_)) => bail!("pi exited before answering {kind}: {}", self.stderr_tail().await),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("pi did not answer {kind} within {timeout:?}")
            }
        }
    }

    /// Send a frame with no response — the dialog cancellations.
    pub async fn notify(&self, frame: &Value) -> Result<()> {
        self.write(frame).await
    }

    async fn write(&self, frame: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(frame).context("serialising a pi command")?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await.context("writing to pi")?;
        stdin.flush().await.context("flushing to pi")?;
        Ok(())
    }

    /// Ask pi to exit. The child is also killed on drop, so this is for the
    /// deliberate case rather than for cleanup.
    pub async fn kill(&self) {
        let _ = self.child.lock().await.start_kill();
    }
}

/// Whether a `extension_ui_request` blocks the agent until answered.
///
/// The fire-and-forget methods are display hints and are ignored; these four
/// wait for a human, and there is none here.
pub fn is_dialog(method: &str) -> bool {
    matches!(method, "select" | "confirm" | "input" | "editor")
}

/// The frame that declines a dialog without answering it.
pub fn cancel_dialog(id: &Value) -> Value {
    json!({ "type": "extension_ui_response", "id": id, "cancelled": true })
}

/// A path to a fake pi for tests: a script that speaks the protocol.
#[cfg(test)]
pub fn fake_pi(dir: &std::path::Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-pi");
    std::fs::write(&path, format!("#!/usr/bin/env python3\n{script}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads commands on stdin and answers per a lookup table, so a test can
    /// assert on correlation rather than on timing.
    const ECHO: &str = r#"
import sys, json
for line in sys.stdin:
    cmd = json.loads(line)
    if cmd["type"] == "slow":
        continue                       # never answers
    print(json.dumps({"type": "response", "id": cmd["id"],
                      "success": True, "echo": cmd["type"]}), flush=True)
"#;

    fn config(binary: PathBuf, dir: &std::path::Path) -> PiConfig {
        PiConfig {
            binary,
            session_file: dir.join("s.jsonl"),
            cwd: dir.to_path_buf(),
            session_dir_override: None,
        }
    }

    #[tokio::test]
    async fn a_command_is_answered_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let (pi, _events) = PiProcess::spawn(&config(fake_pi(dir.path(), ECHO), dir.path())).unwrap();
        let response = pi.command("prompt", json!({ "message": "hi" }), COMMAND_TIMEOUT).await.unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["echo"], "prompt");
    }

    #[tokio::test]
    async fn responses_are_correlated_by_id_not_by_arrival_order() {
        // A real pi has answered a later command before an earlier one.
        const REORDER: &str = r#"
import sys, json
seen = []
for line in sys.stdin:
    seen.append(json.loads(line))
    if len(seen) == 2:
        for cmd in reversed(seen):
            print(json.dumps({"type": "response", "id": cmd["id"], "echo": cmd["type"]}), flush=True)
"#;
        let dir = tempfile::tempdir().unwrap();
        let (pi, _events) =
            PiProcess::spawn(&config(fake_pi(dir.path(), REORDER), dir.path())).unwrap();
        let first = pi.command("first", json!({}), COMMAND_TIMEOUT);
        let second = pi.command("second", json!({}), COMMAND_TIMEOUT);
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap()["echo"], "first", "each caller got its own answer");
        assert_eq!(second.unwrap()["echo"], "second");
    }

    #[tokio::test]
    async fn events_reach_the_listener_and_responses_do_not() {
        const MIXED: &str = r#"
import sys, json
print(json.dumps({"type": "agent_start"}), flush=True)
for line in sys.stdin:
    cmd = json.loads(line)
    print(json.dumps({"type": "message_start"}), flush=True)
    print(json.dumps({"type": "response", "id": cmd["id"], "success": True}), flush=True)
"#;
        let dir = tempfile::tempdir().unwrap();
        let (pi, mut events) =
            PiProcess::spawn(&config(fake_pi(dir.path(), MIXED), dir.path())).unwrap();
        assert_eq!(events.recv().await.unwrap()["type"], "agent_start");
        pi.command("prompt", json!({}), COMMAND_TIMEOUT).await.unwrap();
        assert_eq!(events.recv().await.unwrap()["type"], "message_start");
    }

    #[tokio::test]
    async fn a_command_that_is_never_answered_times_out_without_wedging_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let (pi, _events) = PiProcess::spawn(&config(fake_pi(dir.path(), ECHO), dir.path())).unwrap();
        let err = pi.command("slow", json!({}), Duration::from_millis(150)).await.unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err}");
        // The timed-out waiter must not block later commands.
        let ok = pi.command("prompt", json!({}), COMMAND_TIMEOUT).await.unwrap();
        assert_eq!(ok["echo"], "prompt");
    }

    #[tokio::test]
    async fn a_dying_pi_fails_its_waiters_rather_than_making_them_wait() {
        const DIE: &str = "import sys\nsys.exit(3)\n";
        let dir = tempfile::tempdir().unwrap();
        let (pi, _events) = PiProcess::spawn(&config(fake_pi(dir.path(), DIE), dir.path())).unwrap();
        // Long timeout: the point is that the failure arrives immediately.
        let err = pi.command("prompt", json!({}), Duration::from_secs(30)).await.unwrap_err();
        assert!(err.to_string().contains("exited"), "{err}");
        assert!(!pi.is_alive());
    }

    #[tokio::test]
    async fn an_unparseable_line_does_not_break_the_frames_around_it() {
        const NOISE: &str = r#"
import sys, json
print("not json at all", flush=True)
print(json.dumps({"type": "agent_start"}), flush=True)
for line in sys.stdin:
    cmd = json.loads(line)
    print(json.dumps({"type": "response", "id": cmd["id"], "ok": 1}), flush=True)
"#;
        let dir = tempfile::tempdir().unwrap();
        let (pi, mut events) =
            PiProcess::spawn(&config(fake_pi(dir.path(), NOISE), dir.path())).unwrap();
        assert_eq!(events.recv().await.unwrap()["type"], "agent_start");
        assert_eq!(pi.command("x", json!({}), COMMAND_TIMEOUT).await.unwrap()["ok"], 1);
    }

    #[tokio::test]
    async fn a_line_separator_inside_a_string_does_not_split_the_frame() {
        // U+2028 is legal inside a JSON string. Node's readline splits on it,
        // which is why the bridge had to hand-roll a splitter; the Rust line
        // reader breaks on \n alone, so this is correct for free.
        const SEP: &str = r#"
import sys, json
print(json.dumps({"type": "agent_start", "note": "a b"}), flush=True)
sys.stdin.readline()
"#;
        let dir = tempfile::tempdir().unwrap();
        let (_pi, mut events) =
            PiProcess::spawn(&config(fake_pi(dir.path(), SEP), dir.path())).unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event["note"], "a\u{2028}b", "the frame survived intact");
    }

    #[test]
    fn dialog_methods_are_the_ones_that_block_the_agent() {
        for method in ["select", "confirm", "input", "editor"] {
            assert!(is_dialog(method));
        }
        for method in ["setWidget", "setStatus", "notify"] {
            assert!(!is_dialog(method), "{method} is fire-and-forget");
        }
    }
}
