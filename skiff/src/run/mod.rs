//! Live run state: one pi per session, and the in-flight reply.
//!
//! This is the only state in skiffd that is **not** derived from a file, and
//! it is deliberately not in the store: it lives exactly as long as the
//! process that owns it. A restart loses the in-flight overlay and nothing
//! else, because the transcript re-derives from the session file (DW-004 §4).
//!
//! ## Why the overlay is a separate topic
//!
//! The transcript changes when pi *persists* a message — once per reply. The
//! overlay changes on every delta — many times a second. Announcing both as
//! `Topic::Session` would make every viewer recompute and re-send the whole
//! transcript ten times a second; a 154-message session is a megabyte, so that
//! is not a small mistake. `Topic::Run` is the frequent, cheap one: it touches
//! no SQLite and carries only the live message.

pub mod overlay;
pub mod pi_rpc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, mpsc};
use ts_rs::TS;

use crate::ingest::{Topic, pi};
use crate::model::{Message, SessionKey};
use crate::store::Store;
use overlay::{Overlay, RunId};
use pi_rpc::{COMMAND_TIMEOUT, PiConfig, PiProcess};

/// How often overlay growth is announced.
///
/// pi emits deltas far faster than a screen can usefully change; coalescing to
/// this interval is the difference between a push per token and a push per
/// frame.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Abort is answered quickly or not at all — a pi that will not stop is a
/// problem to surface, not to keep waiting on.
const ABORT_TIMEOUT: Duration = Duration::from_secs(10);

/// A prompt the user has sent that has not yet appeared in the transcript.
///
/// Server-side, not client-side, because two panes on the same session must
/// agree about it — the "would two clients have to agree?" test for which side
/// of the boundary something belongs on (DW-004 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct PendingPrompt {
    /// Echoed from the client's command, so the pane that sent it can match
    /// its optimistic bubble by identity rather than by guessing.
    pub client_id: String,
    pub text: String,
    #[ts(type = "number")]
    pub sent_ms: i64,
}

/// Everything about a session that is not in the store.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct LiveState {
    /// The harness is working: between `agent_start` and settlement.
    pub working: bool,
    /// The in-flight reply, if one is streaming.
    pub pending: Option<Message>,
    /// A sent prompt not yet visible in the transcript.
    pub pending_prompt: Option<PendingPrompt>,
}

struct SessionRun {
    process: Arc<PiProcess>,
    working: bool,
    overlay: Option<Overlay>,
    /// The overlay has been finished by pi and is waiting for its persisted
    /// entry to appear in the transcript.
    ///
    /// This window is why the overlay is not dropped at `message_end`. pi
    /// writes the entry then, but skiffd only *sees* it after the file
    /// watcher fires and the ingest runs — and in between, dropping the
    /// overlay would make the finished reply vanish from the screen and come
    /// back a moment later. Holding it until the transcript actually contains
    /// it is what makes the handover invisible.
    resolving: bool,
    pending_prompt: Option<PendingPrompt>,
    /// How many user messages the transcript held when the prompt was sent.
    /// The prompt is resolved when that count grows — robust where matching on
    /// text is not, because the same text may be sent twice.
    user_count_at_send: usize,
}

impl SessionRun {
    fn live(&self) -> LiveState {
        LiveState {
            working: self.working,
            pending: self.overlay.as_ref().filter(|o| !o.is_empty()).and_then(Overlay::message),
            pending_prompt: self.pending_prompt.clone(),
        }
    }
}

pub struct Runs {
    sessions: Mutex<HashMap<SessionKey, Arc<Mutex<SessionRun>>>>,
    store: Store,
    topics: broadcast::Sender<Topic>,
    binary: PathBuf,
    pi_dir: PathBuf,
    pi_dir_explicit: bool,
    next_run: AtomicU64,
}

impl Runs {
    pub fn new(
        store: Store,
        topics: broadcast::Sender<Topic>,
        binary: PathBuf,
        pi_dir: PathBuf,
        pi_dir_explicit: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::default(),
            store,
            topics,
            binary,
            pi_dir,
            pi_dir_explicit,
            next_run: AtomicU64::new(0),
        })
    }

    /// The live state of a session. A session with no running pi is simply
    /// idle — the absence of a process is not an error.
    pub async fn live(&self, session: &SessionKey) -> LiveState {
        let run = self.sessions.lock().await.get(session).cloned();
        match run {
            Some(run) => run.lock().await.live(),
            None => LiveState::default(),
        }
    }

    /// Send a prompt, spawning pi for this session if it is not already up.
    pub async fn send(&self, session: &SessionKey, text: &str, client_id: &str) -> Result<()> {
        if text.trim().is_empty() {
            bail!("an empty prompt has nothing to ask");
        }
        let run = self.ensure(session).await?;
        let process = {
            let mut state = run.lock().await;
            state.pending_prompt = Some(PendingPrompt {
                client_id: client_id.to_owned(),
                text: text.to_owned(),
                sent_ms: now_ms(),
            });
            state.user_count_at_send = self.user_count(session).await;
            state.process.clone()
        };
        // Announced before the command is answered: the sender should see
        // their message immediately, not after a round trip through pi.
        let _ = self.topics.send(Topic::Run(session.clone()));

        // The prompt travels as a command field, never as an argument — it is
        // user text, and argv is visible to every process on the machine.
        let response = process
            .command("prompt", json!({ "message": text }), COMMAND_TIMEOUT)
            .await
            .context("sending the prompt to pi")?;

        if !response.get("success").and_then(Value::as_bool).unwrap_or(false) {
            let mut state = run.lock().await;
            state.pending_prompt = None;
            drop(state);
            let _ = self.topics.send(Topic::Run(session.clone()));
            // pi's own reason, never an echo of the prompt text.
            bail!(
                "pi rejected the prompt: {}",
                response.get("error").and_then(Value::as_str).unwrap_or("no reason given")
            );
        }
        Ok(())
    }

    /// Stop the run in flight. Aborting an idle session is not an error — it
    /// is what the button does when the run finished a moment ago.
    pub async fn abort(&self, session: &SessionKey) -> Result<()> {
        let run = self.sessions.lock().await.get(session).cloned();
        let Some(run) = run else { return Ok(()) };
        let process = run.lock().await.process.clone();
        let response = process
            .command("abort", json!({}), ABORT_TIMEOUT)
            .await
            .context("asking pi to abort")?;
        if !response.get("success").and_then(Value::as_bool).unwrap_or(false) {
            bail!(
                "pi rejected abort: {}",
                response.get("error").and_then(Value::as_str).unwrap_or("no reason given")
            );
        }
        Ok(())
    }

    /// How many user messages the session's transcript currently holds.
    async fn user_count(&self, session: &SessionKey) -> usize {
        let store = self.store.clone();
        let session = session.clone();
        tokio::task::spawn_blocking(move || {
            let entries = store.entries(&session).unwrap_or_default();
            crate::views::transcript(&entries)
                .iter()
                .filter(|m| m.role == crate::model::Role::User)
                .count()
        })
        .await
        .unwrap_or(0)
    }

    /// The session's file changed: hand a finished reply over to the
    /// transcript, and clear a prompt the transcript has caught up with.
    ///
    /// Both are resolutions against what is actually on disk rather than
    /// against an event, which is what makes them correct when the two
    /// disagree. The prompt compares *counts* rather than text, so sending the
    /// same message twice works.
    pub async fn session_changed(&self, session: &SessionKey) {
        let run = self.sessions.lock().await.get(session).cloned();
        let Some(run) = run else { return };

        let (resolving, watermark) = {
            let state = run.lock().await;
            (state.resolving, state.pending_prompt.as_ref().map(|_| state.user_count_at_send))
        };
        if !resolving && watermark.is_none() {
            return;
        }

        let mut changed = false;
        if resolving {
            // The entry pi wrote at `message_end` is in the transcript now, so
            // the overlay has nothing left to show.
            let mut state = run.lock().await;
            state.overlay = None;
            state.resolving = false;
            changed = true;
        }
        if let Some(watermark) = watermark
            && self.user_count(session).await > watermark
        {
            run.lock().await.pending_prompt = None;
            changed = true;
        }
        if changed {
            let _ = self.topics.send(Topic::Run(session.clone()));
        }
    }

    /// The session's pi, spawned if needed.
    async fn ensure(&self, session: &SessionKey) -> Result<Arc<Mutex<SessionRun>>> {
        if let Some(run) = self.sessions.lock().await.get(session)
            && run.lock().await.process.is_alive()
        {
            return Ok(run.clone());
        }

        let file = self.session_file(session).await?;
        let cwd = self.cwd(session).await;
        let (process, events) = PiProcess::spawn(&PiConfig {
            binary: self.binary.clone(),
            session_file: file,
            cwd,
            session_dir_override: self.pi_dir_explicit.then(|| self.pi_dir.clone()),
        })?;

        let run = Arc::new(Mutex::new(SessionRun {
            process: process.clone(),
            working: false,
            overlay: None,
            resolving: false,
            pending_prompt: None,
            user_count_at_send: 0,
        }));
        self.sessions.lock().await.insert(session.clone(), run.clone());

        tokio::spawn(pump(
            session.clone(),
            run.clone(),
            process,
            events,
            self.topics.clone(),
            self.next_run_id(session),
        ));
        Ok(run)
    }

    fn next_run_id(&self, session: &SessionKey) -> impl Fn() -> RunId + Send + 'static {
        let counter = self.next_run.fetch_add(1, Ordering::SeqCst);
        let session = session.to_string();
        let nth = AtomicU64::new(0);
        move || format!("run:{session}:{counter}:{}", nth.fetch_add(1, Ordering::SeqCst))
    }

    /// pi names a session file by its id; resolution is a name walk, not a
    /// content scan.
    async fn session_file(&self, session: &SessionKey) -> Result<PathBuf> {
        let dir = self.pi_dir.clone();
        let target = format!("{}.jsonl", session.local);
        tokio::task::spawn_blocking(move || {
            pi::session_files(&dir)?
                .into_iter()
                .find(|p| p.file_name().is_some_and(|n| n == target.as_str()))
                .with_context(|| format!("no session file named {target}"))
        })
        .await
        .context("the session-file lookup panicked")?
    }

    /// pi runs where the session runs. A session whose directory is unknown
    /// falls back to the home directory rather than to skiffd's own cwd, which
    /// is wherever the service happened to be started.
    async fn cwd(&self, session: &SessionKey) -> PathBuf {
        let store = self.store.clone();
        let session = session.clone();
        let directory = tokio::task::spawn_blocking(move || {
            store
                .sessions()
                .ok()?
                .into_iter()
                .find(|s| s.id == session)
                .and_then(|s| s.directory)
        })
        .await
        .ok()
        .flatten();
        directory.map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(home)
    }
}

/// Consume one pi's events for the process's lifetime, keeping the run state
/// current and announcing changes at most once per flush interval.
async fn pump(
    session: SessionKey,
    run: Arc<Mutex<SessionRun>>,
    process: Arc<PiProcess>,
    mut events: mpsc::UnboundedReceiver<Value>,
    topics: broadcast::Sender<Topic>,
    next_run_id: impl Fn() -> RunId,
) {
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dirty = false;

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                // Some transitions must be seen immediately rather than at the
                // next flush: a reader watching a run start or stop should not
                // wait out the coalescing window for a state change that is
                // not going to change again.
                match handle(&session, &run, &process, &event, &next_run_id).await {
                    Change::Now => {
                        dirty = false;
                        let _ = topics.send(Topic::Run(session.clone()));
                    }
                    Change::Coalesced => dirty = true,
                    Change::None => {}
                }
            }
            _ = flush.tick() => {
                if dirty {
                    dirty = false;
                    let _ = topics.send(Topic::Run(session.clone()));
                }
            }
        }
    }

    // pi is gone. Whatever it was streaming will never be persisted, so
    // showing it would freeze the transcript on content that can never settle.
    {
        let mut state = run.lock().await;
        state.working = false;
        state.overlay = None;
        state.pending_prompt = None;
    }
    let _ = topics.send(Topic::Run(session));
}

enum Change {
    /// Announce immediately.
    Now,
    /// Announce at the next flush.
    Coalesced,
    None,
}

async fn handle(
    session: &SessionKey,
    run: &Arc<Mutex<SessionRun>>,
    process: &Arc<PiProcess>,
    event: &Value,
    next_run_id: &impl Fn() -> RunId,
) -> Change {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
    let mut state = run.lock().await;

    match kind {
        "agent_start" => {
            state.working = true;
            Change::Now
        }
        "message_start" => {
            // Only assistant messages stream; user and tool-result messages
            // are persisted directly and reach the reader through the file.
            if event.get("message").and_then(|m| m.get("role")).and_then(Value::as_str)
                != Some("assistant")
            {
                return Change::None;
            }
            let model = event
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            state.overlay = Some(Overlay::new(next_run_id(), model, now_ms()));
            Change::None // nothing to show until the first delta
        }
        "message_update" => {
            let Some(delta) = event.get("assistantMessageEvent") else { return Change::None };
            let Some(overlay) = state.overlay.as_mut() else { return Change::None };
            if overlay.apply(delta) { Change::Coalesced } else { Change::None }
        }
        "message_end" => {
            // pi persists the entry at `message_end` (verified against a real
            // pi), so the transcript is about to own this reply — but it does
            // not yet, because skiffd has not seen the file change. The
            // overlay stays up, frozen, until it does. Announce now so the
            // final text is shown even for a reply shorter than one flush
            // interval.
            state.resolving = state.overlay.is_some();
            Change::Now
        }
        "agent_end" => {
            // `willRetry` means the run continues; only a terminal end settles
            // it.
            if !event.get("willRetry").and_then(Value::as_bool).unwrap_or(false) {
                state.working = false;
            }
            drop_unresolved(&mut state);
            Change::Now
        }
        "agent_settled" => {
            state.working = false;
            drop_unresolved(&mut state);
            Change::Now
        }
        "extension_ui_request" => {
            let method = event.get("method").and_then(Value::as_str).unwrap_or_default();
            if !pi_rpc::is_dialog(method) {
                return Change::None; // a display hint
            }
            let Some(id) = event.get("id") else { return Change::None };
            let frame = pi_rpc::cancel_dialog(id);
            let process = process.clone();
            let session = session.clone();
            // Not awaited under the state lock: cancelling must not be able to
            // block the event loop that answers the next dialog.
            drop(state);
            tokio::spawn(async move {
                if let Err(err) = process.notify(&frame).await {
                    tracing::warn!(%session, %err, "could not cancel a pi dialog");
                }
            });
            Change::None
        }
        _ => Change::None,
    }
}

/// Drop a reply that will never be persisted.
///
/// A run that ends without `message_end` was aborted or errored, so its
/// partial reply is never written to the file. Keeping it would freeze the
/// transcript on content that can never settle. A reply that *did* reach
/// `message_end` is left alone: it is waiting for the file, not abandoned.
fn drop_unresolved(state: &mut SessionRun) {
    if !state.resolving {
        state.overlay = None;
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
