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
use crate::util::{short_args, truncate_chars};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_TURNS: usize = 40; // tool-call round trips per user message
const REQ_TIMEOUT: Duration = Duration::from_secs(600); // whole API round trip

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
            cancel: Arc::new(AtomicBool::new(false)),
            messages: vec![json!({ "role": "system", "content": system })],
            stats: SessionStats::default(),
        })
    }

    /// Clear the conversation history (the system prompt stays).
    pub fn reset(&mut self) {
        self.messages.truncate(1);
    }

    pub fn push_user(&mut self, text: &str) {
        self.messages.push(json!({ "role": "user", "content": text }));
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
            self.messages.push(msg.clone());
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
                        self.messages.push(json!({
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
        chat_stream(&self.client, &self.messages, &mut self.auth, &self.base_url, &self.model, io)
            .await
    }
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
    io: &mut T,
) -> ChatOutcome {
    let body = json!({
        "model": model,
        "messages": messages,
        "tools": tools::tools(),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
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
