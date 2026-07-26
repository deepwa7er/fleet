//! `harness serve` — the web UI frontend.
//!
//! An axum server bound to this MacBook's Tailscale IPv4 (discovered at
//! startup from the 100.64.0.0/10 CGNAT range, same approach as
//! breakwater/src/tailscale.rs), so the UI is reachable from any tailnet
//! device while the laptop is on. There is no auth: the tailnet is the
//! boundary, same trust model as the rest of the fleet.
//!
//! Each chat session is a driver task owning a library [Session]; browsers
//! talk to it over a small JSON API and an SSE event stream. [WebIo] is the
//! [TurnIo] implementation that bridges the two: agent events go out on a
//! broadcast channel (with a retained history so late-joining tabs replay
//! the whole session), and lines posted mid-turn arrive through the same
//! input channel the driver reads between turns — so typing while the model
//! works steers it, exactly like the terminal REPL. Web sessions always run
//! yolo (commands execute without confirmation).
//!
//! Sessions are durable: both logs go to SQLite through [harness::store], and
//! [restore_sessions] brings them all back at startup as ordinary live
//! sessions you can keep talking to. The retained in-memory history remains
//! the *live* replay buffer — it holds every streaming delta, so a tab that
//! reconnects mid-turn catches up token by token — while the durable log
//! coalesces those deltas into blocks (see [Emit::persist]).

use clap::Args;
use fleet_common::util::default_db_path;
use futures_util::{Stream, StreamExt};
use harness::agent::{MessageSink, Session, SessionConfig, Steer, TurnIo, cancelled};
use harness::store::{Store, Write, Writer, spawn_writer};
use harness::util::{home, now_secs, resolve_path, short_args, truncate_chars};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

#[derive(Args)]
pub struct ServeArgs {
    /// Address to bind. Default: this host's Tailscale IPv4 (100.64.0.0/10),
    /// discovered at startup, so the UI is reachable only from the tailnet.
    /// Override to serve somewhere else — `--bind 127.0.0.1` for local
    /// development against `bun run dev`.
    #[arg(long)]
    pub bind: Option<IpAddr>,
    /// Port to listen on
    #[arg(long, default_value_t = 8098)]
    pub port: u16,
    /// Session database. Default: $HARNESS_DB, else
    /// ~/.local/share/harness/harness.db
    #[arg(long)]
    pub db: Option<PathBuf>,
}

impl ServeArgs {
    fn db_path(&self) -> PathBuf {
        self.db
            .clone()
            .or_else(|| std::env::var("HARNESS_DB").ok().map(PathBuf::from).filter(|p| p.as_os_str() != ""))
            .unwrap_or_else(|| default_db_path("harness", "harness.db"))
    }
}

const PORT_DEFAULT_NOTE: &str = "8098 sits in the fleet's service port range";

/// Events retained per session for replay to late-joining browsers. Past the
/// cap the oldest quarter is dropped (the conversation itself is unbounded in
/// memory anyway; this just keeps replay snappy).
const EVENT_HISTORY_CAP: usize = 5000;

// ------------------------------------------------------------ events

/// One thing that happened in a session, as the browser renders it. `seq`
/// orders events within a session so SSE reconnects can replay-then-resume
/// without gaps or duplicates.
#[derive(Clone, Serialize)]
struct SessionEvent {
    seq: u64,
    #[serde(flatten)]
    kind: EventKind,
}

/// Serialized as-is into the durable transcript, so it round-trips through
/// [Deserialize] on restore — the wire format and the stored format are the
/// same format on purpose, and only `seq` is re-assigned.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventKind {
    /// A user message (turn start, or an ask_user answer).
    User { text: String },
    /// Guidance typed while the model was working.
    Interjection { text: String },
    /// A status line ([auth], [tokens], model changes, …).
    Note { text: String },
    /// Streamed reasoning delta.
    Reasoning { delta: String },
    /// Streamed answer-text delta.
    Content { delta: String },
    /// The SSE stream to the model ended.
    StreamEnd,
    /// A tool call is about to execute (`args` is a one-line summary).
    ToolCall { name: String, args: String },
    /// A tool call finished; `preview` is the result, truncated.
    ToolResult { name: String, ok: bool, preview: String },
    /// The model asked the user a question (ask_user).
    Ask { question: String },
    /// A turn started (busy) / ended (idle; `fatal` on unrecoverable error).
    TurnStart,
    TurnEnd { fatal: Option<String> },
    /// The session could not start (e.g. no credentials).
    Fatal { message: String },
    /// The conversation history was cleared.
    Reset,
}

/// A run of same-kind streaming deltas, accumulating until some other event
/// ends it. See [Emit::persist].
struct Pending {
    seq: u64,
    /// true = reasoning, false = answer content.
    reasoning: bool,
    text: String,
}

/// The outbound side of a session: broadcast to live SSE clients, a retained
/// in-memory history for replay, and the durable transcript.
struct Emit {
    session: String,
    events: broadcast::Sender<SessionEvent>,
    history: Mutex<Vec<SessionEvent>>,
    seq: AtomicU64,
    writer: Writer,
    pending: Mutex<Option<Pending>>,
}

impl Emit {
    /// An [Emit] pre-loaded with a restored transcript (empty for a fresh
    /// session). The events keep their order but get fresh, contiguous seqs:
    /// seq only ever orders events within one connection's replay-then-resume,
    /// so it need not — and after coalescing cannot — match what was stored.
    fn seeded(session: String, writer: Writer, restored: Vec<EventKind>) -> Emit {
        let (events, _) = broadcast::channel(256);
        let history: Vec<SessionEvent> = restored
            .into_iter()
            .enumerate()
            .map(|(i, kind)| SessionEvent { seq: i as u64 + 1, kind })
            .collect();
        let next = history.len() as u64;
        Emit {
            session,
            events,
            history: Mutex::new(history),
            seq: AtomicU64::new(next),
            writer,
            pending: Mutex::new(None),
        }
    }

    fn emit(&self, kind: EventKind) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.persist(seq, &kind);
        let event = SessionEvent { seq, kind };
        {
            let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            if history.len() >= EVENT_HISTORY_CAP {
                history.drain(..EVENT_HISTORY_CAP / 4);
            }
            history.push(event.clone());
        }
        self.events.send(event).ok(); // no live receivers is fine
    }

    /// Queue the event for the durable transcript, merging consecutive
    /// streaming deltas of the same kind into one record.
    ///
    /// A turn emits thousands of per-token deltas but only a handful of
    /// blocks, and the transcript only needs the blocks — a replaying browser
    /// appends a whole block exactly as it appends a token. Live viewers are
    /// unaffected: this is the write path, not the broadcast. Every non-delta
    /// event closes the open run, and `stream_end` always arrives, so a block
    /// is never left pending across a turn boundary.
    fn persist(&self, seq: u64, kind: &EventKind) {
        let delta = match kind {
            EventKind::Reasoning { delta } => Some((true, delta)),
            EventKind::Content { delta } => Some((false, delta)),
            _ => None,
        };
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((reasoning, text)) = delta {
            let merged = match pending.as_mut() {
                Some(open) if open.reasoning == reasoning => {
                    open.text.push_str(text);
                    true
                }
                _ => false,
            };
            if merged {
                return;
            }
            if let Some(open) = pending.take() {
                self.write_block(open);
            }
            *pending = Some(Pending { seq, reasoning, text: text.clone() });
            return;
        }
        if let Some(open) = pending.take() {
            self.write_block(open);
        }
        self.write(seq, kind);
    }

    fn write_block(&self, open: Pending) {
        let kind = if open.reasoning {
            EventKind::Reasoning { delta: open.text }
        } else {
            EventKind::Content { delta: open.text }
        };
        self.write(open.seq, &kind);
    }

    fn write(&self, seq: u64, kind: &EventKind) {
        match serde_json::to_string(kind) {
            Ok(body) => {
                self.writer.send(Write::Event { session: self.session.clone(), seq, body })
            }
            Err(e) => harness::log(&format!("[store] could not serialize event: {e}")),
        }
    }

    fn snapshot(&self) -> Vec<SessionEvent> {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The [MessageSink] that records a session's context to SQLite.
struct StoreSink {
    session: String,
    writer: Writer,
}

impl MessageSink for StoreSink {
    fn appended(&self, index: usize, message: &Value) {
        self.writer.send(Write::Message {
            session: self.session.clone(),
            idx: index,
            body: message.to_string(),
        });
    }

    fn cleared(&self) {
        self.writer.send(Write::ClearMessages { session: self.session.clone() });
    }
}

// ------------------------------------------------------------ sessions

/// Input to a session's driver task. Lines are user messages (turn starters
/// when idle, interjections/answers mid-turn); Reset clears the history.
enum UserMsg {
    Line(String),
    Reset,
}

struct SessionHandle {
    id: String,
    cwd: PathBuf,
    created_at: u64,
    busy: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    input: mpsc::Sender<UserMsg>,
    emit: Arc<Emit>,
}

struct AppState {
    sessions: Mutex<HashMap<String, Arc<SessionHandle>>>,
    next_id: AtomicU64,
    writer: Writer,
}

/// The web [TurnIo]: events out on the broadcast channel, mid-turn input
/// from the session's message channel.
struct WebIo {
    emit: Arc<Emit>,
    input: mpsc::Receiver<UserMsg>,
    cancel: Arc<AtomicBool>,
}

impl TurnIo for WebIo {
    fn note(&mut self, text: &str) {
        self.emit.emit(EventKind::Note { text: text.to_string() });
    }

    fn reasoning(&mut self, delta: &str) {
        self.emit.emit(EventKind::Reasoning { delta: delta.to_string() });
    }

    fn content(&mut self, delta: &str) {
        self.emit.emit(EventKind::Content { delta: delta.to_string() });
    }

    fn stream_end(&mut self) {
        self.emit.emit(EventKind::StreamEnd);
    }

    fn tool_call(&mut self, name: &str, args: &Value) {
        self.emit.emit(EventKind::ToolCall { name: name.to_string(), args: short_args(args) });
    }

    fn tool_result(&mut self, name: &str, result: &str) {
        self.emit.emit(EventKind::ToolResult {
            name: name.to_string(),
            ok: !result.starts_with("error:"),
            preview: truncate_chars(result, 1_000),
        });
    }

    fn interjected(&mut self, text: &str) {
        self.emit.emit(EventKind::Interjection { text: text.to_string() });
    }

    async fn steer(&mut self) -> Steer {
        tokio::select! {
            _ = cancelled(&self.cancel) => Steer::Interrupted,
            msg = self.input.recv() => match msg {
                Some(UserMsg::Line(text)) => Steer::Interjected(text),
                // Reset mid-turn makes no sense (the history is being appended
                // to); treat it as no-op steering and keep waiting. Channel
                // closed = session deleted: abort the turn.
                Some(UserMsg::Reset) => std::future::pending().await,
                None => std::future::pending().await,
            },
        }
    }

    async fn pending_line(&mut self) -> Option<String> {
        match self.input.try_recv() {
            Ok(UserMsg::Line(text)) => Some(text),
            _ => None,
        }
    }

    async fn ask(&mut self, question: &str) -> Result<String, String> {
        self.emit.emit(EventKind::Ask { question: question.to_string() });
        loop {
            tokio::select! {
                _ = cancelled(&self.cancel) => return Err("error: interrupted by user".into()),
                msg = self.input.recv() => match msg {
                    Some(UserMsg::Line(text)) if !text.trim().is_empty() => {
                        // Echo the answer as a user message so every tab shows it.
                        self.emit.emit(EventKind::User { text: text.clone() });
                        return Ok(text);
                    }
                    Some(_) => continue, // empty line or Reset: keep waiting
                    None => return Err("error: session closed — no answer available".into()),
                },
            }
        }
    }

    async fn confirm(&mut self, _command: &str) -> bool {
        // Web sessions are always yolo, so this is never called; if that ever
        // changes, auto-approving matches the session's mode rather than
        // hanging the turn on a question the UI can't ask.
        true
    }
}

/// One task per session: owns the library Session and feeds it user lines.
/// Exits when the last input sender is dropped (session deleted).
///
/// `restored` is a persisted history to resume from, appended after
/// [Session::start] composes a fresh system prompt — so a resumed session
/// keeps its memory but picks up any edits to system.md / AGENTS.md.
async fn drive_session(
    emit: Arc<Emit>,
    input: mpsc::Receiver<UserMsg>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    config: SessionConfig,
    restored: Vec<Value>,
    sink: Arc<dyn MessageSink>,
) {
    let mut session = match Session::start(config).await {
        Ok(mut s) => {
            s.cancel = Arc::clone(&cancel);
            // Restore before attaching the sink: these messages are already
            // stored, and re-announcing them would rewrite what they came from.
            if !restored.is_empty() {
                let count = restored.len();
                s.restore(restored);
                emit.emit(EventKind::Note {
                    text: format!("[resumed with {count} messages of context]"),
                });
            }
            s.sink = Some(sink);
            s
        }
        Err(e) => {
            emit.emit(EventKind::Fatal { message: e });
            return;
        }
    };
    let mut io = WebIo { emit: Arc::clone(&emit), input, cancel };
    while let Some(msg) = io.input.recv().await {
        // Between turns: forget interrupts delivered while idle.
        session.cancel.store(false, Ordering::SeqCst);
        match msg {
            UserMsg::Line(text) => {
                emit.emit(EventKind::User { text: text.clone() });
                session.push_user(&text);
                busy.store(true, Ordering::SeqCst);
                emit.emit(EventKind::TurnStart);
                let fatal = session.run_turn(&mut io).await;
                busy.store(false, Ordering::SeqCst);
                emit.emit(EventKind::TurnEnd { fatal });
            }
            UserMsg::Reset => {
                session.reset();
                emit.emit(EventKind::Reset);
            }
        }
    }
}

// ------------------------------------------------------------ http api

#[derive(Deserialize)]
struct CreateSession {
    /// Working directory for the agent: absolute, ~/, or relative to $HOME.
    /// Default: $HOME itself.
    cwd: Option<String>,
    /// Model id override (default: $KIMI_MODEL or k3).
    model: Option<String>,
}

#[derive(Deserialize)]
struct PostMessage {
    text: String,
}

type ApiResult<T> = Result<T, (axum::http::StatusCode, String)>;

fn not_found(id: &str) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::NOT_FOUND, format!("no session {id}"))
}

fn lookup(state: &AppState, id: &str) -> Option<Arc<SessionHandle>> {
    state.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../web/index.html"))
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn list_sessions(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::Json<Value> {
    let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let mut list: Vec<Value> = sessions
        .values()
        .map(|h| {
            json!({
                "id": h.id,
                "cwd": h.cwd.display().to_string(),
                "created_at": h.created_at,
                "busy": h.busy.load(Ordering::SeqCst),
            })
        })
        .collect();
    list.sort_by_key(|s| s["created_at"].as_u64().unwrap_or(0));
    axum::Json(json!({ "sessions": list }))
}

async fn create_session(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::Json(body): axum::Json<CreateSession>,
) -> ApiResult<axum::Json<Value>> {
    let cwd = match &body.cwd {
        Some(raw) if !raw.trim().is_empty() => resolve_path(&home(), raw.trim()),
        _ => home(),
    };
    if !cwd.is_dir() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("not a directory: {}", cwd.display()),
        ));
    }
    let id = format!("s{}", state.next_id.fetch_add(1, Ordering::Relaxed) + 1);
    start_session(&state, Restore { id: id.clone(), cwd, model: body.model, created_at: now_secs(), messages: Vec::new(), events: Vec::new() });
    harness::log(&format!("[serve] session {id} created"));
    Ok(axum::Json(json!({ "id": id })))
}

/// Everything needed to bring a session up, whether it is brand new (empty
/// `messages`/`events`) or being resumed from the store.
struct Restore {
    id: String,
    cwd: PathBuf,
    model: Option<String>,
    created_at: u64,
    messages: Vec<Value>,
    events: Vec<EventKind>,
}

/// Spawn a session's driver task, record it durably, and register its handle.
/// Fresh creation and startup restore share this so the two cannot drift.
fn start_session(state: &AppState, restore: Restore) {
    let Restore { id, cwd, model, created_at, messages, events } = restore;
    let (input_tx, input_rx) = mpsc::channel(64);
    let emit = Arc::new(Emit::seeded(id.clone(), state.writer.clone(), events));
    let cancel = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(SessionHandle {
        id: id.clone(),
        cwd: cwd.clone(),
        created_at,
        busy: Arc::clone(&busy),
        cancel: Arc::clone(&cancel),
        input: input_tx,
        emit: Arc::clone(&emit),
    });
    let mut config = SessionConfig::from_env(cwd.clone(), true); // web sessions are yolo
    if let Some(model) = model
        && !model.trim().is_empty() {
            config.model = model.trim().to_string();
        }
    state.writer.send(Write::Session {
        id: id.clone(),
        cwd: cwd.display().to_string(),
        model: config.model.clone(),
        created_at,
    });
    let sink = Arc::new(StoreSink { session: id.clone(), writer: state.writer.clone() });
    tokio::spawn(drive_session(emit, input_rx, cancel, busy, config, messages, sink));
    state.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, handle);
}

async fn delete_session(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let Some(handle) =
        state.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
    else {
        return Err(not_found(&id));
    };
    // Interrupt any in-flight turn; dropping the handle closes the input
    // channel, so the driver task exits once the turn unwinds.
    handle.cancel.store(true, Ordering::SeqCst);
    // Delete means delete: the row goes, and both logs cascade with it.
    state.writer.send(Write::Delete { session: id.clone() });
    harness::log(&format!("[serve] session {id} deleted"));
    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn post_message(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<PostMessage>,
) -> ApiResult<axum::http::StatusCode> {
    let Some(handle) = lookup(&state, &id) else {
        return Err(not_found(&id));
    };
    if body.text.trim().is_empty() {
        return Err((axum::http::StatusCode::BAD_REQUEST, "empty message".to_string()));
    }
    handle
        .input
        .send(UserMsg::Line(body.text))
        .await
        .map_err(|_| (axum::http::StatusCode::GONE, "session closed".to_string()))?;
    Ok(axum::http::StatusCode::ACCEPTED)
}

async fn interrupt(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let Some(handle) = lookup(&state, &id) else {
        return Err(not_found(&id));
    };
    handle.cancel.store(true, Ordering::SeqCst);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn reset(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let Some(handle) = lookup(&state, &id) else {
        return Err(not_found(&id));
    };
    if handle.busy.load(Ordering::SeqCst) {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "session is busy — interrupt it first".to_string(),
        ));
    }
    handle
        .input
        .send(UserMsg::Reset)
        .await
        .map_err(|_| (axum::http::StatusCode::GONE, "session closed".to_string()))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// The session event stream: replay the retained history, then live events.
/// Subscribing before snapshotting means an event can land in both; the
/// `seq > max_replayed` filter drops those duplicates.
async fn events(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<
    axum::response::sse::Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>,
> {
    let Some(handle) = lookup(&state, &id) else {
        return Err(not_found(&id));
    };
    let receiver = handle.emit.events.subscribe();
    let replay = handle.emit.snapshot();
    let max_replayed = replay.last().map(|e| e.seq).unwrap_or(0);

    fn to_sse(event: SessionEvent) -> Result<axum::response::sse::Event, Infallible> {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok(axum::response::sse::Event::default().data(data))
    }

    let replayed = futures_util::stream::iter(replay.into_iter().map(to_sse));
    let live = tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(
        move |item| async move {
            match item {
                Ok(event) if event.seq > max_replayed => Some(to_sse(event)),
                _ => None, // duplicate, out of order, or lagged past
            }
        },
    );
    Ok(axum::response::sse::Sse::new(replayed.chain(live)).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

// ------------------------------------------------------------ bind + serve

/// Tailscale hands each node an IPv4 in the CGNAT block `100.64.0.0/10`
/// (`100.64.0.0` – `100.127.255.255`). On the fleet's machines that block is
/// only ever the Tailscale interface, so membership identifies it reliably —
/// same approach as `breakwater/src/tailscale.rs`.
fn is_tailscale_cgnat(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// One enumeration pass: the host's tailnet IPv4, or `None` if it hasn't been
/// assigned yet.
fn find_tailscale_ip() -> Option<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .find(|&v4| is_tailscale_cgnat(v4))
}

/// Resolve the MacBook's Tailscale IPv4, polling until it appears or the
/// timeout elapses (tailscaled can still be coming up when launchd starts us
/// at login; KeepAlive restarts us if we give up).
async fn resolve_tailscale_ip(timeout: Duration) -> Result<Ipv4Addr, String> {
    const POLL: Duration = Duration::from_millis(500);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(ip) = find_tailscale_ip() {
            return Ok(ip);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no Tailscale IPv4 (100.64.0.0/10) on any interface after {}s — \
                 is Tailscale running and logged in? (or pass --bind)",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Bring every stored session back as an ordinary live session, and advance
/// the id counter past the ids already in use.
///
/// A session whose working directory has gone away is still restored: the
/// transcript is worth keeping and the user may want to read or delete it, so
/// it comes back with a note rather than silently vanishing.
fn restore_sessions(state: &AppState, store: &Store) -> Result<usize, String> {
    let stored = store.sessions()?;
    let mut highest = 0u64;
    for row in &stored {
        if let Some(n) = row.id.strip_prefix('s').and_then(|n| n.parse::<u64>().ok()) {
            highest = highest.max(n);
        }
        let mut events: Vec<EventKind> = store
            .events(&row.id)?
            .iter()
            .filter_map(|body| match serde_json::from_str(body) {
                Ok(kind) => Some(kind),
                Err(e) => {
                    harness::log(&format!("[store] dropping unreadable event in {}: {e}", row.id));
                    None
                }
            })
            .collect();
        let cwd = PathBuf::from(&row.cwd);
        if !cwd.is_dir() {
            events.push(EventKind::Note {
                text: format!("[working directory {} no longer exists]", row.cwd),
            });
        }
        start_session(
            state,
            Restore {
                id: row.id.clone(),
                cwd,
                model: Some(row.model.clone()),
                created_at: row.created_at,
                messages: store.messages(&row.id)?,
                events,
            },
        );
    }
    // Fresh ids continue past every restored one, so a new session can never
    // collide with (and overwrite) a resumed one.
    state.next_id.store(highest, Ordering::Relaxed);
    Ok(stored.len())
}

pub async fn serve(args: ServeArgs) -> Result<(), String> {
    let ip = match args.bind {
        Some(ip) => ip,
        None => IpAddr::V4(resolve_tailscale_ip(Duration::from_secs(60)).await?),
    };
    let db_path = args.db_path();
    let store = Arc::new(Store::open(&db_path)?);
    harness::log(&format!("[serve] sessions at {}", db_path.display()));
    let state = Arc::new(AppState {
        sessions: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(0),
        writer: spawn_writer(Arc::clone(&store)),
    });
    match restore_sessions(&state, &store) {
        Ok(0) => {}
        Ok(n) => harness::log(&format!("[serve] resumed {n} session(s)")),
        // A restore failure must not cost the user a working server; they can
        // still start fresh sessions, and the stored ones are untouched.
        Err(e) => harness::log(&format!("[serve] could not restore sessions: {e}")),
    }
    let app = axum::Router::new()
        .route("/", axum::routing::get(index))
        .route("/api/healthz", axum::routing::get(healthz))
        .route("/api/sessions", axum::routing::get(list_sessions).post(create_session))
        .route("/api/sessions/{id}", axum::routing::delete(delete_session))
        .route("/api/sessions/{id}/messages", axum::routing::post(post_message))
        .route("/api/sessions/{id}/interrupt", axum::routing::post(interrupt))
        .route("/api/sessions/{id}/reset", axum::routing::post(reset))
        .route("/api/sessions/{id}/events", axum::routing::get(events))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind((ip, args.port))
        .await
        .map_err(|e| format!("bind {ip}:{}: {e}", args.port))?;
    harness::log(&format!(
        "harness serve — http://{ip}:{} (tailnet only, no auth; {PORT_DEFAULT_NOTE})",
        args.port
    ));
    axum::serve(listener, app).await.map_err(|e| format!("serve: {e}"))
}
