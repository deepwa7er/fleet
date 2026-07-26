//! The agent loop and its frontend contract.
//!
//! [Session::run_turn] drives the model: one API round trip over SSE, tool
//! calls executed and appended, repeat until the model answers with plain
//! text. Everything a frontend needs flows through [TurnIo] — streamed
//! deltas, notes, tool activity, and the three kinds of mid-turn user input
//! (steering interjections, ask_user answers, run_command confirmation). The
//! terminal REPL and the web server each implement the trait; the loop knows
//! nothing about either.
//!
//! Cancellation and interjection are ordinary `select!` branches on the
//! session's `cancel` flag and the frontend's input source. Fatal API/network
//! errors are returned (as `ChatOutcome::Fatal` / the run_turn return value),
//! never exit() — a web session must not be able to kill the server.

use crate::auth::Auth;
use crate::prompt;
use crate::tools::{self, ToolCtx};
use crate::usage::usage_int;
use crate::util::{est_tokens, message_chars, short_args, truncate_chars};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// pub(crate) so the loop tests assert against the real bound rather than a
// copy of it that could drift.
pub(crate) const MAX_TURNS: usize = 40; // tool-call round trips per user message
const REQ_TIMEOUT: Duration = Duration::from_secs(600); // whole API round trip

/// Context window assumed when `KIMI_CONTEXT_WINDOW` is unset: K3's, which is
/// the default model.
///
/// The API does not report its window, so this tracks the *default model* and
/// nothing else. Point `--model` / `KIMI_MODEL` at anything with a smaller
/// window (K2.7 Code, say) without also setting `KIMI_CONTEXT_WINDOW`, and
/// compaction will trigger too late to save the request — the one direction
/// this gets expensive rather than merely wasteful.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;

/// Compact once a *measured* prompt passes this fraction of the window. The
/// gap to 1.0 is the headroom the next request needs while compaction runs.
const COMPACT_AT: f64 = 0.75;

/// Fraction of the window the retained tail should fit in, so a compaction
/// buys back a useful amount of room rather than triggering again immediately.
const KEEP_TAIL: f64 = 0.30;

/// Per-tool-result cap when rendering dropped messages for the summarizer.
/// Tool output is the bulk of a long history and the least worth summarizing
/// verbatim — what it *showed* matters, not every line of it.
const SUMMARY_RESULT_CAP: usize = 500;

const SUMMARY_SYSTEM: &str = "You summarize a software engineering conversation so that it can \
continue in a smaller context window. Be dense and factual. Preserve: what the user asked for, \
decisions taken and the reasons for them, files and paths touched, commands run and how they \
turned out, what was tried and failed, and anything still outstanding. Omit pleasantries and \
any reasoning that can be re-derived. Write prose, not a replay of every step. Never invent \
anything: if something is unclear from the transcript, leave it out.";

const SUMMARY_ASK: &str = "Summarize the conversation below. Your summary replaces these \
messages verbatim in an ongoing session, so it must carry everything needed to continue the \
work without them.";

const SUMMARY_PREFIX: &str = "[The earlier part of this conversation was compacted to fit the \
context window. What happened so far:]";

/// The mid-turn user actions the loop can receive.
pub enum Steer {
    /// Cancel the turn (Ctrl-C in the REPL, Stop in the web UI).
    Interrupted,
    /// The user typed guidance mid-turn: discard partial output, append the
    /// guidance to history, and keep the turn going.
    Interjected(String),
}

/// The frontend contract. Default methods produce no output and no input, so
/// a minimal implementation (tests, one-shot runs) can override nothing.
///
/// `waiting()` marks the start of one API round trip (no SSE data yet);
/// `first_token()` and `stream_end()` both end that wait — frontends should
/// treat both as idempotent.
///
// async fn in a public trait warns because auto-trait bounds can't be
// specified for implementors; harness uses TurnIo only statically (generic
// `T: TurnIo`) and every implementation is Send, so the lint doesn't apply.
#[allow(async_fn_in_trait)]
pub trait TurnIo: Send {
    /// A request is in flight and no SSE data has arrived yet.
    fn waiting(&mut self) {}
    /// The first SSE data of the round trip arrived.
    fn first_token(&mut self) {}
    /// A streamed reasoning delta (dimmed in the terminal, collapsible on the web).
    fn reasoning(&mut self, _delta: &str) {}
    /// A streamed answer-text delta.
    fn content(&mut self, _delta: &str) {}
    /// The SSE stream ended: flush/close any half-open output.
    fn stream_end(&mut self) {}
    /// A status line ([auth] retries, [tokens], model changes, …).
    fn note(&mut self, _text: &str) {}
    /// A tool call is about to execute.
    fn tool_call(&mut self, name: &str, args: &Value) {
        self.note(&format!("[tool] {name} {}", short_args(args)));
    }
    /// A tool call finished; `result` is the exact string appended to history
    /// (errors are prefixed "error:").
    fn tool_result(&mut self, _name: &str, _result: &str) {}
    /// The user steered mid-turn: their line was appended to history and the
    /// turn continues with that guidance.
    fn interjected(&mut self, text: &str) {
        self.note(&format!("[interjection — steering the model: {}]", truncate_chars(text, 80)));
    }
    /// Wait for a mid-turn user action. The default never resolves, which
    /// disables steering for that frontend.
    async fn steer(&mut self) -> Steer {
        std::future::pending().await
    }
    /// A queued input line if one is already available (checked at tool-call
    /// boundaries and to drain follow-up lines after an interjection).
    async fn pending_line(&mut self) -> Option<String> {
        None
    }
    /// ask_user: the user's typed answer.
    async fn ask(&mut self, _question: &str) -> Result<String, String> {
        Err("error: this frontend cannot answer questions".into())
    }
    /// run_command confirmation outside yolo mode. True = run it.
    async fn confirm(&mut self, _command: &str) -> bool {
        false
    }
}

/// Where a session's message history is durably recorded.
///
/// The loop appends; whether that goes anywhere is the frontend's business —
/// the terminal REPL records nothing, `harness serve` records to SQLite. Only
/// the *history* travels this way; the streamed transcript a UI renders is a
/// [TurnIo] concern, and the two are stored separately for that reason.
///
/// The system prompt at index 0 is never announced: [Session::start] composes
/// it before a sink can be attached, so a restored session gets a freshly
/// composed prompt rather than a frozen copy.
pub trait MessageSink: Send + Sync {
    /// `message` was appended to the history at `index`.
    fn appended(&self, index: usize, message: &Value);
    /// The history was cleared back to the system prompt alone.
    fn cleared(&self);
}

/// Everything needed to start a session. See [Session::start].
pub struct SessionConfig {
    pub model: String,
    pub base_url: String,
    /// Working directory: the system prompt's {cwd}, AGENTS.md lookup, and
    /// the root for relative tool paths.
    pub cwd: PathBuf,
    /// Yolo mode: run commands without confirmation.
    pub yolo: bool,
    /// Extra system-prompt instructions appended last (--system).
    pub system_extra: Option<String>,
    /// File of extra system-prompt instructions (--system-file).
    pub system_file: Option<PathBuf>,
    /// Context window in tokens, used to decide when to compact.
    pub context_window: u64,
}

impl SessionConfig {
    /// Model/base_url from the environment (KIMI_MODEL → "k3", KIMI_BASE_URL →
    /// the Kimi Code endpoint), everything else defaulted.
    pub fn from_env(cwd: PathBuf, yolo: bool) -> SessionConfig {
        SessionConfig {
            model: std::env::var("KIMI_MODEL").unwrap_or_else(|_| "k3".to_string()),
            base_url: std::env::var("KIMI_BASE_URL")
                .unwrap_or_else(|_| "https://api.kimi.com/coding/v1".to_string()),
            cwd,
            yolo,
            system_extra: None,
            system_file: None,
            context_window: std::env::var("KIMI_CONTEXT_WINDOW")
                .ok()
                .and_then(|w| w.parse::<u64>().ok())
                .filter(|w| *w > 0)
                .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        }
    }
}

/// One conversation: message history, credentials, token stats, and the
/// per-session knobs tools need.
pub struct Session {
    pub client: reqwest::Client,
    pub auth: Auth,
    pub model: String,
    pub base_url: String,
    pub cwd: PathBuf,
    pub yolo: bool,
    /// Set to abort the current turn (blocking tool code polls it; the async
    /// side selects on it via [cancelled]).
    pub cancel: Arc<AtomicBool>,
    pub messages: Vec<Value>,
    pub stats: SessionStats,
    /// Durable record of [Session::messages], if the frontend keeps one.
    pub sink: Option<Arc<dyn MessageSink>>,
    /// Context window this session compacts against.
    pub context_window: u64,
    /// `stats.round_trips` as of the last compaction. Compaction waits for a
    /// *newer* measurement than that, so it can never fire twice on the same
    /// (now stale) prompt size.
    ///
    /// pub(crate) only so the loop tests can build a [Session] directly — that
    /// path skips credential loading and system-prompt composition, neither of
    /// which a test should touch.
    pub(crate) compacted_after: u64,
}

impl Session {
    /// Load credentials, compose the system prompt, and open the history.
    pub async fn start(config: SessionConfig) -> Result<Session, String> {
        let client = reqwest::Client::new();
        let auth = Auth::load(&client).await?;
        let system = prompt::build_system_prompt(&config)?;
        Ok(Session {
            client,
            auth,
            model: config.model,
            base_url: config.base_url,
            cwd: config.cwd,
            yolo: config.yolo,
            context_window: config.context_window,
            compacted_after: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            messages: vec![json!({ "role": "system", "content": system })],
            stats: SessionStats::default(),
            sink: None,
        })
    }

    /// Re-append a persisted history on top of the freshly composed system
    /// prompt. Not reported to the sink — these messages came *from* it.
    /// Call before attaching the sink, so the ordering can't be misread.
    ///
    /// The history is repaired on the way in (see [answer_orphaned_tool_calls])
    /// so a session cut short mid-turn resumes cleanly.
    pub fn restore(&mut self, messages: Vec<Value>) {
        self.messages.extend(answer_orphaned_tool_calls(messages));
    }

    /// Clear the conversation history (the system prompt stays).
    pub fn reset(&mut self) {
        self.messages.truncate(1);
        if let Some(sink) = &self.sink {
            sink.cleared();
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push(json!({ "role": "user", "content": text }));
    }

    /// The one place the history grows, so the sink can never drift from it.
    fn push(&mut self, message: Value) {
        self.messages.push(message);
        if let Some(sink) = &self.sink {
            let index = self.messages.len() - 1;
            sink.appended(index, &self.messages[index]);
        }
    }

    /// Swap the history wholesale (compaction). The sink is an append-only
    /// log, so a rewrite is expressed as the clear-and-re-append it is; the
    /// store applies writes in order, which makes that safe.
    fn replace_history(&mut self, messages: Vec<Value>) {
        self.messages = messages;
        if let Some(sink) = &self.sink {
            sink.cleared();
            for (index, message) in self.messages.iter().enumerate().skip(1) {
                sink.appended(index, message);
            }
        }
    }

    /// Compact if the last *measured* prompt crossed [COMPACT_AT].
    ///
    /// Called at the top of each loop iteration, where the history is always
    /// at a settled boundary — never between an assistant's tool calls and
    /// the results that answer them.
    async fn maybe_compact<T: TurnIo>(&mut self, io: &mut T) {
        let Some(last) = self.stats.last else { return };
        // Wait for a measurement newer than the last compaction, so one large
        // prompt cannot trigger compaction repeatedly.
        if self.stats.round_trips <= self.compacted_after {
            return;
        }
        if (last.prompt as f64) < self.context_window as f64 * COMPACT_AT {
            return;
        }
        io.note(&format!(
            "[compacting: last prompt was {} tokens, over {:.0}% of the {} window]",
            last.prompt,
            COMPACT_AT * 100.0,
            self.context_window
        ));
        match self.compact(io).await {
            Ok(report) => io.note(&format!("[{report}]")),
            Err(e) => io.note(&format!("[compaction skipped: {e} — history unchanged]")),
        }
        // Either way, wait for a fresh measurement before trying again: on
        // success the old number is stale, and on failure retrying against the
        // same number would just repeat the same expensive request.
        self.compacted_after = self.stats.round_trips;
    }

    /// Replace the older half of the history with a model-written summary,
    /// keeping the system prompt and the newest messages verbatim.
    ///
    /// On any failure the history is left **untouched** and the error is
    /// returned: running out of context is bad, but quietly destroying the
    /// conversation because a network call failed is worse.
    pub async fn compact<T: TurnIo>(&mut self, io: &mut T) -> Result<String, String> {
        let budget = (self.context_window as f64 * KEEP_TAIL) as u64;
        let Some(cut) = compaction_cut(&self.messages, budget) else {
            return Err("nothing old enough to compact".to_string());
        };
        let transcript = transcript_for_summary(&self.messages[1..cut]);
        let summary = self.summarize(io, &transcript).await?;

        let mut rebuilt = Vec::with_capacity(self.messages.len() - cut + 2);
        rebuilt.push(self.messages[0].clone()); // the system prompt survives
        rebuilt.push(json!({
            "role": "user",
            "content": format!("{SUMMARY_PREFIX}\n\n{summary}"),
        }));
        rebuilt.extend_from_slice(&self.messages[cut..]);
        let dropped = cut - 1;
        let kept = self.messages.len() - cut;
        self.replace_history(rebuilt);
        Ok(format!("compacted {dropped} older messages into a summary; kept the newest {kept}"))
    }

    /// One side request asking the model to summarize `transcript`.
    ///
    /// Sent without tools — this is not an agent turn, and offering tools
    /// invites a tool call where a summary was asked for.
    async fn summarize<T: TurnIo>(
        &mut self,
        io: &mut T,
        transcript: &str,
    ) -> Result<String, String> {
        let messages = vec![
            json!({ "role": "system", "content": SUMMARY_SYSTEM }),
            json!({ "role": "user", "content": format!("{SUMMARY_ASK}\n\n{transcript}") }),
        ];
        let mut quiet = Quiet { inner: io, cancel: Arc::clone(&self.cancel) };
        let outcome = chat_stream(
            &self.client,
            &messages,
            &mut self.auth,
            &self.base_url,
            &self.model,
            None,
            &mut quiet,
        )
        .await;
        match outcome {
            ChatOutcome::Message(reply) => {
                if let Some(t) = reply.tokens {
                    self.stats.record_side(t);
                }
                reply.message["content"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "the model returned an empty summary".to_string())
            }
            ChatOutcome::Interrupted | ChatOutcome::Interjected(_) => {
                Err("interrupted".to_string())
            }
            ChatOutcome::Fatal(e) => Err(e),
        }
    }

    fn tool_ctx(&self) -> ToolCtx {
        ToolCtx {
            cwd: self.cwd.clone(),
            yolo: self.yolo,
            cancel: Arc::clone(&self.cancel),
        }
    }

    /// Tool-call rounds until the model answers with plain text (already
    /// streamed). Returns Some(message) on a fatal API/network error — the
    /// turn is over but the session stays usable.
    pub async fn run_turn<T: TurnIo>(&mut self, io: &mut T) -> Option<String> {
        for _ in 0..MAX_TURNS {
            // Top of the loop is the only settled boundary: every tool call
            // issued so far has its result appended.
            self.maybe_compact(io).await;
            let msg = match self.chat_stream(io).await {
                ChatOutcome::Message(reply) => {
                    if let Some(t) = reply.tokens {
                        self.stats.record(t);
                    }
                    if let Some(served) = &reply.served_model
                        && self.stats.served_model.as_deref() != Some(served.as_str()) {
                            let previous = self.stats.served_model.replace(served.clone());
                            match previous {
                                None => io.note(&format!("[model] API reports model: {served}")),
                                Some(previous) => io.note(&format!(
                                    "[model] API-reported model changed: {previous} -> {served}"
                                )),
                            }
                        }
                    reply.message
                }
                ChatOutcome::Interrupted => {
                    io.note("[interrupted]");
                    return None;
                }
                ChatOutcome::Interjected(text) => {
                    io.interjected(&text);
                    self.push_user(&text);
                    continue;
                }
                ChatOutcome::Fatal(e) => return Some(e),
            };
            self.push(msg.clone());
            if let Some(tcs) = msg["tool_calls"].as_array()
                && !tcs.is_empty() {
                    let mut guidance: Option<String> = None;
                    for tc in tcs {
                        // After an interrupt or an interjection, skip execution
                        // but still answer every tool call so the history stays
                        // valid for the next request.
                        let result = if self.cancel.load(Ordering::SeqCst) || guidance.is_some() {
                            "error: interrupted by user".to_string()
                        } else if let Some(text) = io.pending_line().await {
                            guidance = Some(text);
                            "error: interrupted by user".to_string()
                        } else {
                            let name = tc["function"]["name"].as_str().unwrap_or("");
                            let args = tc["function"]["arguments"]
                                .as_str()
                                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                .unwrap_or_else(|| json!({}));
                            io.tool_call(name, &args);
                            let result = tools::dispatch(&self.tool_ctx(), io, name, args).await;
                            io.tool_result(name, &result);
                            result
                        };
                        self.push(json!({
                            "role": "tool",
                            "tool_call_id": tc["id"],
                            "content": result,
                        }));
                    }
                    if self.cancel.load(Ordering::SeqCst) {
                        io.note("[interrupted]");
                        return None;
                    }
                    if let Some(text) = guidance {
                        io.interjected(&text);
                        self.push_user(&text);
                    }
                    continue;
                }
            return None;
        }
        io.note("(stopped: hit MAX_TURNS without a final answer)");
        None
    }

    /// One API round trip over SSE. Cancellation and interjection are ordinary
    /// `select!` branches. Partial output is discarded from history either way.
    async fn chat_stream<T: TurnIo>(&mut self, io: &mut T) -> ChatOutcome {
        chat_stream(
            &self.client,
            &self.messages,
            &mut self.auth,
            &self.base_url,
            &self.model,
            Some(tools::tools()),
            io,
        )
        .await
    }
}

/// Where the retained tail should start: the newest messages that fit in
/// `budget` estimated tokens, then moved forward so the tail never begins in
/// the middle of a tool run — a `role: "tool"` message whose call sits in the
/// dropped prefix would leave the history unusable.
///
/// The newest message is always kept, however large. Returns `None` when
/// there is nothing worth dropping (index 0 is the system prompt, which always
/// survives) or nothing left to keep.
fn compaction_cut(messages: &[Value], budget: u64) -> Option<usize> {
    let mut kept = 0u64;
    let mut cut = messages.len();
    for i in (1..messages.len()).rev() {
        let size = est_tokens(message_chars(&messages[i]));
        if cut < messages.len() && kept + size > budget {
            break;
        }
        kept += size;
        cut = i;
    }
    while cut < messages.len() && messages[cut]["role"] == "tool" {
        cut += 1;
    }
    (cut > 1 && cut < messages.len()).then_some(cut)
}

/// Render messages for the summarizer as plain text rather than replaying
/// them as a conversation: a flat transcript carries no `tool_call_id`
/// pairing constraints, so the slice can be cut anywhere and still be a valid
/// request. Streamed reasoning is left out — it is the bulkiest part of a
/// history and the least useful thing to summarize.
fn transcript_for_summary(messages: &[Value]) -> String {
    let mut out = String::new();
    for message in messages {
        let content = message["content"].as_str().unwrap_or("").trim();
        match message["role"].as_str().unwrap_or("") {
            "user" => out.push_str(&format!("User: {content}\n")),
            "assistant" => {
                if !content.is_empty() {
                    out.push_str(&format!("Assistant: {content}\n"));
                }
                for call in message["tool_calls"].as_array().unwrap_or(&Vec::new()) {
                    let name = call["function"]["name"].as_str().unwrap_or("?");
                    let args = call["function"]["arguments"].as_str().unwrap_or("");
                    out.push_str(&format!("Assistant ran {name}({})\n", truncate_chars(args, 200)));
                }
            }
            "tool" => {
                // Say *why* it is cut off. Without this the summarizer reads a
                // clipped result as a failed command and writes the truncation
                // into the summary as an outstanding problem to go re-run.
                let total = content.chars().count();
                if total > SUMMARY_RESULT_CAP {
                    out.push_str(&format!(
                        "Result ({total} chars, clipped to {SUMMARY_RESULT_CAP} for this \
                         summary — the tool itself succeeded): {}…\n",
                        truncate_chars(content, SUMMARY_RESULT_CAP)
                    ));
                } else {
                    out.push_str(&format!("Result: {content}\n"));
                }
            }
            _ => {}
        }
    }
    out
}

/// Wraps a frontend for a side request the user did not ask for (the
/// compaction summary): the summary must not stream into the transcript as if
/// it were an answer, but Ctrl-C / Stop must still abort it.
///
/// `steer` resolves only on the cancel flag, never on typed input. A line
/// typed during compaction therefore stays queued in the frontend and is
/// picked up as an ordinary interjection when the turn resumes, instead of
/// being consumed here and dropped.
struct Quiet<'a, T: TurnIo> {
    inner: &'a mut T,
    cancel: Arc<AtomicBool>,
}

impl<T: TurnIo> TurnIo for Quiet<'_, T> {
    // Forwarded so the spinner still covers the wait; content, reasoning and
    // notes fall through to the no-op defaults.
    fn waiting(&mut self) {
        self.inner.waiting();
    }
    fn first_token(&mut self) {
        self.inner.first_token();
    }
    fn stream_end(&mut self) {
        self.inner.stream_end();
    }
    async fn steer(&mut self) -> Steer {
        cancelled(&self.cancel).await;
        Steer::Interrupted
    }
}

/// Give every `tool_calls` entry a matching `role: "tool"` message, inventing
/// results for any left unanswered.
///
/// A persisted history can stop anywhere — the process is killed between the
/// assistant's tool-call message and the results it was about to append. The
/// API requires the pairing to be complete, so a history restored with a hole
/// in it would fail *every* subsequent request, permanently. Filling the hole
/// costs one synthetic result and matches what the loop already does when a
/// turn is interrupted: answer every call so the history stays valid.
fn answer_orphaned_tool_calls(messages: Vec<Value>) -> Vec<Value> {
    /// Emit results for calls still owed, immediately after the tool run they
    /// belong to.
    fn settle(out: &mut Vec<Value>, owed: &mut Vec<String>) {
        for id in owed.drain(..) {
            out.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": "error: harness restarted before this tool call ran",
            }));
        }
    }

    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let mut owed: Vec<String> = Vec::new();
    for message in messages {
        let is_tool = message["role"] == "tool";
        // The tool run for the previous assistant message is over.
        if !is_tool {
            settle(&mut out, &mut owed);
        }
        if is_tool && let Some(id) = message["tool_call_id"].as_str() {
            owed.retain(|owed_id| owed_id != id);
        }
        if message["role"] == "assistant"
            && let Some(calls) = message["tool_calls"].as_array() {
                owed = calls
                    .iter()
                    .filter_map(|call| call["id"].as_str().map(str::to_string))
                    .collect();
            }
        out.push(message);
    }
    settle(&mut out, &mut owed);
    out
}

/// Resolves once `flag` is set. All cancellable waits select on this rather
/// than registering their own listeners: a listener only sees events delivered
/// while registered, but the flag catches every interrupt, whenever it arrived.
pub async fn cancelled(flag: &Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Token usage of one API round trip, from the stream's final usage chunk.
/// `prompt` is the exact measured size of the context sent to the model.
#[derive(Clone, Copy)]
pub struct TokenCount {
    pub prompt: u64,
    pub completion: u64,
}

impl TokenCount {
    pub fn from_value(v: &Value) -> Option<TokenCount> {
        Some(TokenCount {
            prompt: usage_int(&v["prompt_tokens"])?.max(0) as u64,
            completion: usage_int(&v["completion_tokens"])?.max(0) as u64,
        })
    }
}

/// Cumulative token accounting for the session (the REPL's /context).
#[derive(Default)]
pub struct SessionStats {
    pub round_trips: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub last: Option<TokenCount>,
    /// Model id reported by the API response, if it includes one.
    pub served_model: Option<String>,
}

impl SessionStats {
    pub fn record(&mut self, t: TokenCount) {
        self.round_trips += 1;
        self.prompt_tokens += t.prompt;
        self.completion_tokens += t.completion;
        self.last = Some(t);
    }

    /// A round trip that is not part of the conversation (the compaction
    /// summary). It costs real tokens, so it counts toward the totals — but
    /// not toward `last`, which answers "how big is the context now" and
    /// would otherwise report the summarizer's own prompt.
    pub fn record_side(&mut self, t: TokenCount) {
        self.round_trips += 1;
        self.prompt_tokens += t.prompt;
        self.completion_tokens += t.completion;
    }
}

/// A completed assistant reply plus response metadata.
struct AssistantReply {
    message: Value,
    tokens: Option<TokenCount>,
    served_model: Option<String>,
}

/// How a round trip ended.
enum ChatOutcome {
    /// The model produced a complete assistant message.
    Message(AssistantReply),
    /// Interrupted: discard partial output and abort the turn.
    Interrupted,
    /// The user typed guidance mid-turn: discard partial output, append the
    /// guidance to history, and keep the turn going.
    Interjected(String),
    /// Unrecoverable API/network/auth error. The turn ends; the session lives.
    Fatal(String),
}

#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulated state of one SSE stream. Deltas are forwarded to the frontend
/// as they arrive; the raw text accumulates here for the history message.
#[derive(Default)]
struct SseState {
    content: String,
    reasoning: String,
    calls: BTreeMap<u64, ToolCallAcc>,
    usage: Value,
    served_model: Option<String>,
    raw_fallback: String,
    saw_data: bool,
}

impl SseState {
    /// Process one SSE line. Ok(true) on "[DONE]", Err on a fatal API error.
    fn line<T: TurnIo>(&mut self, raw: &str, io: &mut T) -> Result<bool, String> {
        let trimmed = raw.trim_end();
        if trimmed.is_empty() || trimmed.starts_with(':') {
            return Ok(false); // event separator / keep-alive comment
        }
        let Some(data) = trimmed.strip_prefix("data:") else {
            self.raw_fallback.push_str(raw);
            self.raw_fallback.push('\n');
            return Ok(false);
        };
        self.saw_data = true;
        io.first_token();
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        if let Some(err) = chunk.get("error") {
            return Err(format!("API error: {}", truncate_chars(&err.to_string(), 500)));
        }
        if let Some(model) = chunk["model"].as_str()
            && !model.is_empty() {
                self.served_model = Some(model.to_string());
            }
        if !chunk["usage"].is_null() {
            self.usage = chunk["usage"].clone();
        }
        let delta = &chunk["choices"][0]["delta"];
        if delta.is_null() {
            return Ok(false);
        }
        if let Some(r) = delta["reasoning_content"].as_str() {
            self.reasoning.push_str(r);
            io.reasoning(r);
        }
        if let Some(c) = delta["content"].as_str() {
            self.content.push_str(c);
            io.content(c);
        }
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0);
                let acc = self.calls.entry(idx).or_default();
                if let Some(id) = tc["id"].as_str() {
                    acc.id = id.to_string();
                }
                if let Some(name) = tc["function"]["name"].as_str() {
                    acc.name = name.to_string();
                }
                if let Some(args) = tc["function"]["arguments"].as_str() {
                    acc.arguments.push_str(args);
                }
            }
        }
        Ok(false)
    }

    /// Stream ended: build the assistant message (or parse the non-SSE fallback).
    /// Returns the message and the model id reported by the API, if any.
    fn finish<T: TurnIo>(mut self, io: &mut T) -> Result<(Value, Option<String>), String> {
        if !self.saw_data && !self.raw_fallback.trim().is_empty() {
            // Server answered with a plain (non-SSE) JSON response; accept it.
            let resp: Value = serde_json::from_str(&self.raw_fallback)
                .map_err(|e| format!("bad API response: {e}"))?;
            if let Some(model) = resp["model"].as_str()
                && !model.is_empty() {
                    self.served_model = Some(model.to_string());
                }
            let msg = resp["choices"][0]["message"].clone();
            if msg.is_null() {
                return Err("no message in API response".into());
            }
            if let Some(r) = msg["reasoning_content"].as_str()
                && !r.is_empty() {
                    io.reasoning(r);
                }
            if let Some(c) = msg["content"].as_str()
                && !c.is_empty() {
                    io.content(c);
                }
            if !resp["usage"].is_null() {
                self.usage = resp["usage"].clone();
            }
            io.stream_end();
            return Ok((msg, self.served_model));
        }

        io.stream_end();
        let mut msg = json!({ "role": "assistant" });
        msg["content"] = if self.content.is_empty() && !self.calls.is_empty() {
            Value::Null
        } else {
            Value::String(self.content)
        };
        if !self.reasoning.is_empty() {
            msg["reasoning_content"] = Value::String(self.reasoning);
        }
        if !self.calls.is_empty() {
            msg["tool_calls"] = self
                .calls
                .into_values()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "type": "function",
                        "function": { "name": a.name, "arguments": a.arguments },
                    })
                })
                .collect::<Vec<_>>()
                .into();
        }
        Ok((msg, self.served_model))
    }
}

/// One API round trip over SSE. Field-level borrows of the session, so auth
/// (token refresh) can be mutable while messages are shared. One retry after
/// a 401: refresh (or re-read) credentials, then try again. Cancellation
/// rides the frontend's steer() branch, not this function.
async fn chat_stream<T: TurnIo>(
    client: &reqwest::Client,
    messages: &[Value],
    auth: &mut Auth,
    base_url: &str,
    model: &str,
    // None for a request that is not an agent turn (the compaction summary):
    // offering tools there invites a tool call where prose was asked for.
    tools: Option<Value>,
    io: &mut T,
) -> ChatOutcome {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(tools) = tools {
        body["tools"] = tools;
    }
    let url = format!("{base_url}/chat/completions");
    let mut retried = false;
    loop {
        let token = match auth.token(client).await {
            Ok(token) => token,
            Err(e) => return ChatOutcome::Fatal(e),
        };
        io.waiting();
        let request = client
            .post(&url)
            .bearer_auth(&token)
            .header("User-Agent", "kimi-harness/0.2")
            .timeout(REQ_TIMEOUT)
            .json(&body);
        // send() resolves when response headers arrive, which for this API is
        // typically the first token — so the wait for headers must be just as
        // cancellable as the stream itself.
        let resp = tokio::select! {
            result = request.send() => match result {
                Ok(resp) => resp,
                Err(e) => {
                    io.stream_end();
                    return ChatOutcome::Fatal(format!("network error: {e}"));
                }
            },
            outcome = io.steer() => {
                io.stream_end();
                return match outcome {
                    Steer::Interrupted => ChatOutcome::Interrupted,
                    Steer::Interjected(text) => ChatOutcome::Interjected(text),
                };
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            io.stream_end();
            if status == 401 && !retried && auth.handle_401(client).await {
                io.note("[auth] retrying request with fresh credentials");
                retried = true;
                continue;
            }
            if status == 401 && !retried {
                return ChatOutcome::Fatal(format!(
                    "401 unauthorized (and token refresh failed). Run `kimi` to log in again, or set KIMI_API_KEY.\n{detail}"
                ));
            }
            if status == 401 {
                return ChatOutcome::Fatal(format!(
                    "401 unauthorized — token expired or invalid. Run `kimi` to refresh the CLI login, or set KIMI_API_KEY.\n{detail}"
                ));
            }
            return ChatOutcome::Fatal(format!("API error {status}: {}", truncate_chars(&detail, 500)));
        }

        /// Why the SSE consume loop stopped.
        enum StreamEnd {
            Done, // [DONE] or end of stream
            Interrupted,
            Interjected(String),
        }

        let mut state = SseState::default();
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let end = loop {
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(Ok(bytes)) => {
                        buf.extend_from_slice(&bytes);
                        // Split at '\n': UTF-8 continuation bytes are >= 0x80,
                        // so a codepoint never straddles two lines.
                        let mut stop: Option<Result<(), String>> = None;
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            match state.line(&String::from_utf8_lossy(&line), io) {
                                Ok(true) => { stop = Some(Ok(())); break; }
                                Ok(false) => {}
                                Err(msg) => { stop = Some(Err(msg)); break; }
                            }
                        }
                        match stop {
                            Some(Ok(())) => break StreamEnd::Done,
                            Some(Err(msg)) => {
                                io.stream_end();
                                return ChatOutcome::Fatal(msg);
                            }
                            None => {}
                        }
                    }
                    Some(Err(e)) => {
                        io.stream_end();
                        if e.is_timeout() {
                            return ChatOutcome::Fatal(format!(
                                "network error: request timed out after {}s", REQ_TIMEOUT.as_secs()
                            ));
                        }
                        return ChatOutcome::Fatal(format!("stream read error: {e}"));
                    }
                    None => break StreamEnd::Done,
                },
                outcome = io.steer() => break match outcome {
                    Steer::Interrupted => StreamEnd::Interrupted,
                    Steer::Interjected(text) => StreamEnd::Interjected(text),
                },
            }
        };
        match end {
            StreamEnd::Done => {
                let tokens = TokenCount::from_value(&state.usage);
                let (message, served_model) = match state.finish(io) {
                    Ok(done) => done,
                    Err(e) => return ChatOutcome::Fatal(e),
                };
                if let Some(t) = tokens {
                    io.note(&format!("[tokens] prompt={} completion={}", t.prompt, t.completion));
                }
                return ChatOutcome::Message(AssistantReply { message, tokens, served_model });
            }
            StreamEnd::Interrupted => {
                io.stream_end();
                return ChatOutcome::Interrupted;
            }
            StreamEnd::Interjected(text) => {
                io.stream_end();
                return ChatOutcome::Interjected(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No output, no input: the default TurnIo behavior under test.
    struct NullIo;
    impl TurnIo for NullIo {}

    #[test]
    fn token_count_from_usage_payload() {
        let t = TokenCount::from_value(&json!({
            "prompt_tokens": 45231, "completion_tokens": 612, "total_tokens": 45843
        }))
        .unwrap();
        assert_eq!((t.prompt, t.completion), (45231, 612));
        // Numbers as strings are accepted, like the usages endpoint sends.
        let t = TokenCount::from_value(&json!({
            "prompt_tokens": "100", "completion_tokens": "5"
        }))
        .unwrap();
        assert_eq!((t.prompt, t.completion), (100, 5));
        assert!(TokenCount::from_value(&json!({})).is_none());
        assert!(TokenCount::from_value(&Value::Null).is_none());
    }

    #[test]
    fn sse_stream_captures_api_reported_model() {
        let mut state = SseState::default();
        let mut io = NullIo;
        let chunk = r#"data: {"model":"k3-2026-07","choices":[{"delta":{}}]}"#;
        assert_eq!(state.line(chunk, &mut io), Ok(false));
        assert_eq!(state.line("data: [DONE]", &mut io), Ok(true));
        let (msg, served_model) = state.finish(&mut io).unwrap();
        assert_eq!(msg["role"], "assistant");
        assert_eq!(served_model.as_deref(), Some("k3-2026-07"));
    }

    #[test]
    fn non_sse_fallback_captures_api_reported_model() {
        let mut state = SseState::default();
        let mut io = NullIo;
        let resp = r#"{"model":"kimi-for-coding","choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        assert_eq!(state.line(resp, &mut io), Ok(false));
        let (msg, served_model) = state.finish(&mut io).unwrap();
        assert_eq!(msg["content"], "hi");
        assert_eq!(served_model.as_deref(), Some("kimi-for-coding"));
    }

    /// Helpers mirroring the two message shapes the repair cares about.
    fn calls(ids: &[&str]) -> Value {
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": ids.iter().map(|id| json!({
                "id": id, "type": "function",
                "function": { "name": "read_file", "arguments": "{}" }
            })).collect::<Vec<_>>(),
        })
    }
    fn answer(id: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": "ok" })
    }

    #[test]
    fn a_history_cut_off_mid_tool_run_is_repaired() {
        // Killed after the assistant asked for two tools and only one answered.
        let repaired = answer_orphaned_tool_calls(vec![
            json!({ "role": "user", "content": "go" }),
            calls(&["a", "b"]),
            answer("a"),
        ]);
        assert_eq!(repaired.len(), 4);
        assert_eq!(repaired[3]["role"], "tool");
        assert_eq!(repaired[3]["tool_call_id"], "b");
        assert!(repaired[3]["content"].as_str().unwrap().contains("restarted"));
    }

    #[test]
    fn a_history_cut_off_before_any_result_is_repaired() {
        let repaired = answer_orphaned_tool_calls(vec![json!({"role":"user"}), calls(&["a"])]);
        assert_eq!(repaired.len(), 3);
        assert_eq!(repaired[2]["tool_call_id"], "a");
    }

    #[test]
    fn a_complete_history_is_left_alone() {
        let original = vec![
            json!({ "role": "user", "content": "go" }),
            calls(&["a", "b"]),
            answer("a"),
            answer("b"),
            json!({ "role": "assistant", "content": "done" }),
        ];
        assert_eq!(answer_orphaned_tool_calls(original.clone()), original);
    }

    /// The gap can also be interior — an older turn that was interrupted, with
    /// ordinary turns after it. Results must land in their own turn, not at the
    /// end of the history.
    #[test]
    fn an_interior_gap_is_filled_in_place() {
        let repaired = answer_orphaned_tool_calls(vec![
            calls(&["a"]),
            json!({ "role": "user", "content": "never mind, do this instead" }),
            json!({ "role": "assistant", "content": "sure" }),
        ]);
        assert_eq!(repaired.len(), 4);
        assert_eq!(repaired[1]["role"], "tool", "the result belongs right after its call");
        assert_eq!(repaired[1]["tool_call_id"], "a");
        assert_eq!(repaired[2]["role"], "user");
    }

    /// A message of roughly `tokens` estimated size.
    fn bulk(role: &str, tokens: usize) -> Value {
        json!({ "role": role, "content": "x".repeat(tokens * 4) })
    }

    #[test]
    fn compaction_keeps_the_newest_messages_within_budget() {
        let messages = vec![
            bulk("system", 10),
            bulk("user", 100),
            bulk("assistant", 100),
            bulk("user", 100),
            bulk("assistant", 100),
        ];
        // Budget for ~two of the 100-token messages.
        let cut = compaction_cut(&messages, 250).unwrap();
        assert_eq!(cut, 3, "keeps the last two, drops indexes 1..3");
        assert!(cut > 0, "the system prompt is never dropped");
    }

    /// The constraint that makes this more than a slice: a retained tail may
    /// not begin with a tool result, because its call would be in the dropped
    /// prefix and the API requires the pair.
    #[test]
    fn compaction_never_cuts_inside_a_tool_run() {
        let messages = vec![
            bulk("system", 10),
            bulk("user", 100),
            calls(&["a", "b"]),
            answer("a"),
            answer("b"),
            bulk("assistant", 10),
        ];
        // A budget that would naturally cut between the two tool results.
        let cut = compaction_cut(&messages, 15).unwrap();
        assert_ne!(messages[cut]["role"], "tool", "tail must not start on a tool result");
        assert_eq!(cut, 5, "advanced past the whole tool run");
    }

    #[test]
    fn compaction_declines_when_there_is_nothing_to_drop() {
        // Only a system prompt and one message: dropping it would leave
        // nothing to keep.
        assert!(compaction_cut(&[bulk("system", 10), bulk("user", 5)], 1_000).is_none());
        assert!(compaction_cut(&[bulk("system", 10)], 1_000).is_none());
        // A generous budget keeps everything, so there is nothing older to
        // summarize.
        let messages = vec![bulk("system", 10), bulk("user", 5), bulk("assistant", 5)];
        assert!(compaction_cut(&messages, 1_000).is_none());
    }

    #[test]
    fn compaction_always_keeps_the_newest_message_however_large() {
        let messages = vec![bulk("system", 10), bulk("user", 10), bulk("assistant", 100_000)];
        let cut = compaction_cut(&messages, 10).unwrap();
        assert_eq!(cut, 2, "the last message is kept even though it blows the budget");
    }

    #[test]
    fn summary_transcript_is_flat_text_without_reasoning() {
        let messages = vec![
            json!({ "role": "user", "content": "fix the bug" }),
            json!({
                "role": "assistant",
                "content": "looking",
                "reasoning_content": "SECRET-CHAIN-OF-THOUGHT",
                "tool_calls": [{
                    "id": "a", "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"x.rs\"}" }
                }],
            }),
            json!({ "role": "tool", "tool_call_id": "a", "content": "file contents" }),
        ];
        let text = transcript_for_summary(&messages);
        assert!(text.contains("User: fix the bug"), "{text}");
        assert!(text.contains("Assistant: looking"), "{text}");
        assert!(text.contains("read_file"), "{text}");
        assert!(text.contains("Result: file contents"), "{text}");
        // Reasoning is the bulkiest and least useful part to summarize.
        assert!(!text.contains("SECRET-CHAIN-OF-THOUGHT"), "{text}");
    }

    /// Long tool results are clipped, and the clip is *labelled* — otherwise
    /// the summarizer reads it as a command that failed halfway and records a
    /// phantom problem in the summary.
    #[test]
    fn summary_transcript_labels_clipped_tool_results() {
        let messages = vec![
            json!({ "role": "tool", "tool_call_id": "a", "content": "y".repeat(10_000) }),
        ];
        let text = transcript_for_summary(&messages);
        assert!(text.chars().count() < SUMMARY_RESULT_CAP + 200, "len {}", text.chars().count());
        assert!(text.contains("10000 chars"), "{text}");
        assert!(text.contains("the tool itself succeeded"), "{text}");
    }

    #[test]
    fn side_round_trips_count_in_totals_but_not_in_context_size() {
        let mut s = SessionStats::default();
        s.record(TokenCount { prompt: 1_000, completion: 10 });
        s.record_side(TokenCount { prompt: 90_000, completion: 500 });
        assert_eq!(s.prompt_tokens, 91_000, "the summary costs real tokens");
        assert_eq!(s.round_trips, 2);
        assert_eq!(s.last.unwrap().prompt, 1_000, "`last` still means the conversation");
    }

    #[test]
    fn session_stats_accumulate() {
        let mut s = SessionStats::default();
        s.record(TokenCount { prompt: 100, completion: 10 });
        s.record(TokenCount { prompt: 250, completion: 20 });
        assert_eq!(s.round_trips, 2);
        assert_eq!((s.prompt_tokens, s.completion_tokens), (350, 30));
        assert_eq!(s.last.unwrap().prompt, 250);
    }
}
