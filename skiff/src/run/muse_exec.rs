//! Muse's live process model: one `muse exec --json` child per prompt.
//!
//! Muse has no persistent RPC mode. Its stdout is an event stream for the
//! current run while its session JSONL remains the sole source of committed
//! transcript truth. This module therefore owns only liveness and the overlay;
//! the file ingest owns every finished message.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, broadcast, oneshot};

use super::{LiveState, PendingPrompt, now_ms};
use crate::content::parse;
use crate::ingest::Topic;
use crate::model::{Harness, Message, Part, Role, SessionKey, SourceHealth};
use crate::store::Store;

const STDERR_TAIL_LIMIT: usize = 4_096;
type Ready = Arc<Mutex<Option<oneshot::Sender<Result<(), String>>>>>;

#[derive(Debug, Clone)]
pub struct MuseConfig {
    pub binary: PathBuf,
    pub session_dir: PathBuf,
    pub session_dir_explicit: bool,
}

struct MuseState {
    pid: u32,
    run_id: String,
    started_ms: i64,
    output: String,
    working: bool,
    aborted: bool,
    pending_prompt: Option<PendingPrompt>,
    user_count_at_send: usize,
    assistant_count_at_send: usize,
}

impl MuseState {
    fn live(&self) -> LiveState {
        let pending = (!self.output.is_empty()).then(|| Message {
            id: self.run_id.clone(),
            role: Role::Assistant,
            agent: None,
            created_ms: Some(self.started_ms),
            completed_ms: None,
            parts: vec![Part::Text {
                blocks: parse(&self.output),
            }],
        });
        LiveState {
            working: self.working,
            pending,
            pending_prompt: self.pending_prompt.clone(),
        }
    }
}

pub struct MuseRuns {
    binary: Result<PathBuf, String>,
    data_home: Result<Option<PathBuf>, String>,
    store: Store,
    topics: broadcast::Sender<Topic>,
    sessions: Arc<Mutex<HashMap<SessionKey, Arc<Mutex<MuseState>>>>>,
    next_run: AtomicU64,
}

impl MuseRuns {
    pub fn new(config: MuseConfig, store: Store, topics: broadcast::Sender<Topic>) -> Self {
        let binary = super::resolve::binary(&config.binary);
        match &binary {
            Ok(path) => tracing::info!(muse = %path.display(), "muse resolved"),
            Err(error) => tracing::warn!("{error} — muse sessions can be read but not run"),
        }
        let data_home = muse_data_home(&config.session_dir, config.session_dir_explicit);
        if let Err(error) = &data_home {
            tracing::warn!("{error} — muse sessions can be read but not run");
        }
        let runtime_error = binary
            .as_ref()
            .err()
            .cloned()
            .or_else(|| data_home.as_ref().err().cloned());
        let health = SourceHealth {
            source: "muse runner".to_owned(),
            error: runtime_error,
            checked_ms: now_ms(),
        };
        if let Err(error) = store.set_source_health(&health) {
            tracing::warn!(%error, "could not record Muse runner health");
        } else {
            let _ = topics.send(Topic::SourceHealth);
        }
        Self {
            binary,
            data_home,
            store,
            topics,
            sessions: Arc::default(),
            next_run: AtomicU64::new(0),
        }
    }

    pub async fn live(&self, session: &SessionKey) -> LiveState {
        let state = self.sessions.lock().await.get(session).cloned();
        match state {
            Some(state) => state.lock().await.live(),
            None => LiveState::default(),
        }
    }

    pub async fn send(&self, session: &SessionKey, text: &str, client_id: &str) -> Result<()> {
        if session.harness != Harness::Muse {
            bail!("{} is not a muse session", session);
        }
        if text.trim().is_empty() {
            bail!("an empty prompt has nothing to ask");
        }
        let binary = self.binary.clone().map_err(anyhow::Error::msg)?;
        let data_home = self.data_home.clone().map_err(anyhow::Error::msg)?;
        let (cwd, user_count, assistant_count) = self.session_facts(session).await?;

        let temp = tempfile::Builder::new()
            .prefix("skiff-muse-")
            .tempdir()
            .context("creating a private Muse prompt directory")?;
        let prompt_file = temp.path().join("prompt.txt");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&prompt_file)
            .context("creating Muse's prompt file")?;
        file.write_all(text.as_bytes())
            .context("writing Muse's prompt file")?;
        file.sync_all().context("syncing Muse's prompt file")?;

        let mut command = Command::new(&binary);
        command
            .arg("exec")
            .arg("--json")
            .arg("--yolo")
            .arg("--session-id")
            .arg(&session.local)
            .arg("--prompt-file")
            .arg(&prompt_file)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(data_home) = data_home {
            command.env("XDG_DATA_HOME", data_home);
        }

        // The map lock makes the "one run per session" check and child spawn
        // one atomic operation. Two simultaneous sends can never both escape.
        let mut sessions = self.sessions.lock().await;
        if sessions.get(session).is_some_and(|state| {
            state.try_lock().map_or(true, |state| {
                state.working || state.pending_prompt.is_some() || !state.output.is_empty()
            })
        }) {
            bail!("a run is already active for this session");
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawning {}", binary.display()))?;
        let pid = child
            .id()
            .context("Muse exited before its process id was available")?;
        let stdout = child.stdout.take().context("Muse stdout was not piped")?;
        let stderr = child.stderr.take().context("Muse stderr was not piped")?;
        let run_number = self.next_run.fetch_add(1, Ordering::SeqCst);
        let state = Arc::new(Mutex::new(MuseState {
            pid,
            run_id: format!("run:{session}:muse:{run_number}"),
            started_ms: now_ms(),
            output: String::new(),
            working: false,
            aborted: false,
            pending_prompt: Some(PendingPrompt {
                client_id: client_id.to_owned(),
                text: text.to_owned(),
                sent_ms: now_ms(),
            }),
            user_count_at_send: user_count,
            assistant_count_at_send: assistant_count,
        }));
        sessions.insert(session.clone(), state.clone());
        drop(sessions);
        let _ = self.topics.send(Topic::Run(session.clone()));

        let stderr_tail: Arc<Mutex<String>> = Arc::default();
        tokio::spawn(read_stderr(stderr, stderr_tail.clone()));
        let (ready_tx, ready_rx) = oneshot::channel();
        let ready = Arc::new(Mutex::new(Some(ready_tx)));
        tokio::spawn(read_stdout(
            session.clone(),
            stdout,
            state.clone(),
            ready.clone(),
            self.topics.clone(),
        ));
        tokio::spawn(wait_for_exit(
            child,
            ExitContext {
                session: session.clone(),
                state,
                ready,
                stderr_tail,
                topics: self.topics.clone(),
                sessions: self.sessions.clone(),
                _prompt_dir: temp,
            },
        ));

        ready_rx
            .await
            .context("Muse exited before acknowledging the run")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn abort(&self, session: &SessionKey) -> Result<()> {
        if session.harness != Harness::Muse {
            bail!("{} is not a muse session", session);
        }
        let state = self.sessions.lock().await.get(session).cloned();
        let Some(state) = state else { return Ok(()) };
        let mut state = state.lock().await;
        if !state.working {
            return Ok(());
        }
        state.aborted = true;
        // SAFETY: `pid` came from the child still owned by this registry. A
        // failed kill is surfaced; no raw or user-selected process id enters.
        let result = unsafe { libc::kill(state.pid as i32, libc::SIGINT) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("interrupting Muse");
        }
        Ok(())
    }

    pub async fn session_changed(&self, session: &SessionKey) {
        let state = self.sessions.lock().await.get(session).cloned();
        let Some(state) = state else { return };
        let Ok((_, users, assistants)) = self.session_facts(session).await else {
            return;
        };
        let removable = {
            let mut state = state.lock().await;
            if users > state.user_count_at_send {
                state.pending_prompt = None;
            }
            if assistants > state.assistant_count_at_send {
                state.output.clear();
            }
            !state.working && state.pending_prompt.is_none() && state.output.is_empty()
        };
        let _ = self.topics.send(Topic::Run(session.clone()));
        if removable {
            let mut sessions = self.sessions.lock().await;
            if sessions
                .get(session)
                .is_some_and(|current| Arc::ptr_eq(current, &state))
            {
                sessions.remove(session);
            }
        }
    }

    async fn session_facts(&self, session: &SessionKey) -> Result<(PathBuf, usize, usize)> {
        let store = self.store.clone();
        let session = session.clone();
        tokio::task::spawn_blocking(move || {
            let summary = store
                .sessions()?
                .into_iter()
                .find(|summary| summary.id == session)
                .with_context(|| format!("session {session} not found"))?;
            let cwd = summary
                .directory
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .unwrap_or_else(super::home);
            let transcript = crate::views::transcript(&store.entries(&session)?);
            let users = transcript
                .iter()
                .filter(|message| message.role == Role::User)
                .count();
            let assistants = transcript
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .count();
            Ok((cwd, users, assistants))
        })
        .await
        .context("the Muse session lookup panicked")?
    }
}

async fn read_stdout(
    session: SessionKey,
    stdout: tokio::process::ChildStdout,
    state: Arc<Mutex<MuseState>>,
    ready: Ready,
    topics: broadcast::Sender<Topic>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut last_flush = tokio::time::Instant::now() - super::FLUSH_INTERVAL;
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(sender) = ready.lock().await.take() {
            state.lock().await.working = true;
            let _ = sender.send(Ok(()));
            let _ = topics.send(Topic::Run(session.clone()));
        }
        if record.get("payload_type").and_then(Value::as_str) == Some("run.output.delta")
            && let Some(text) = record
                .get("payload")
                .and_then(|payload| payload.get("text"))
                .and_then(Value::as_str)
        {
            state.lock().await.output.push_str(text);
            if last_flush.elapsed() >= super::FLUSH_INTERVAL {
                last_flush = tokio::time::Instant::now();
                let _ = topics.send(Topic::Run(session.clone()));
            }
        }
    }
}

async fn read_stderr(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<String>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut tail = tail.lock().await;
        tail.push_str(&line);
        tail.push('\n');
        if tail.len() > STDERR_TAIL_LIMIT {
            let target = tail.len() - STDERR_TAIL_LIMIT;
            let cut = (target..tail.len())
                .find(|index| tail.is_char_boundary(*index))
                .unwrap_or(tail.len());
            *tail = tail[cut..].to_owned();
        }
    }
}

struct ExitContext {
    session: SessionKey,
    state: Arc<Mutex<MuseState>>,
    ready: Ready,
    stderr_tail: Arc<Mutex<String>>,
    topics: broadcast::Sender<Topic>,
    sessions: Arc<Mutex<HashMap<SessionKey, Arc<Mutex<MuseState>>>>>,
    _prompt_dir: tempfile::TempDir,
}

async fn wait_for_exit(mut child: tokio::process::Child, context: ExitContext) {
    let ExitContext {
        session,
        state,
        ready,
        stderr_tail,
        topics,
        sessions,
        _prompt_dir,
    } = context;
    let status = child.wait().await;
    if let Some(sender) = ready.lock().await.take() {
        let detail = stderr_tail
            .lock()
            .await
            .trim()
            .lines()
            .last()
            .unwrap_or("Muse exited before emitting a record")
            .to_owned();
        let _ = sender.send(Err(detail));
    }
    let mut state_guard = state.lock().await;
    state_guard.working = false;
    let successful = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    if state_guard.aborted || !successful {
        state_guard.output.clear();
        state_guard.pending_prompt = None;
    }
    let removable = state_guard.pending_prompt.is_none() && state_guard.output.is_empty();
    drop(state_guard);
    let _ = topics.send(Topic::Run(session.clone()));
    if removable {
        let mut sessions = sessions.lock().await;
        if sessions
            .get(&session)
            .is_some_and(|current| Arc::ptr_eq(current, &state))
        {
            sessions.remove(&session);
        }
    }
}

/// Translate an explicit `<root>/muse/sessions` scan path back into the XDG
/// root Muse itself must receive. Any other override would make the reader and
/// writer disagree, so it is rejected before a child can write elsewhere.
fn muse_data_home(session_dir: &Path, explicit: bool) -> Result<Option<PathBuf>, String> {
    if !explicit {
        return Ok(None);
    }
    let muse = session_dir
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "muse"));
    if session_dir
        .file_name()
        .is_none_or(|name| name != "sessions")
        || muse.is_none()
    {
        return Err(format!(
            "an explicit muse session dir must end in /muse/sessions (got {})",
            session_dir.display()
        ));
    }
    Ok(muse.and_then(Path::parent).map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_session_root_maps_to_xdg_data_home() {
        assert_eq!(
            muse_data_home(Path::new("/tmp/data/muse/sessions"), true).unwrap(),
            Some(PathBuf::from("/tmp/data"))
        );
    }

    #[test]
    fn an_inconsistent_explicit_root_is_rejected() {
        let error = muse_data_home(Path::new("/tmp/sessions"), true).unwrap_err();
        assert!(error.contains("/muse/sessions"));
    }
}
