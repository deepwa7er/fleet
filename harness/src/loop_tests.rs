//! End-to-end tests for the agent loop, against a scripted API.
//!
//! [Session::run_turn] is the product: it drives the model, executes tool
//! calls, keeps the history valid across interrupts, and now compacts. Every
//! other test in this crate covers a piece in isolation — SSE parsing, the
//! compaction cut, the history repair. These drive the whole loop.
//!
//! Nothing here touches the network, the user's credentials, or their config:
//! [MockApi] is an axum server on an ephemeral loopback port that replays
//! scripted responses and records what was sent to it, and the [Session] is
//! built directly rather than through [Session::start] — which would load real
//! credentials and create `~/.config/harness/system.md`.
//!
//! Not covered here: the OAuth 401-retry path, which needs a credentials file
//! and an OAuth endpoint to rotate against. Its moving parts (the refresh
//! lock, adopting another process's token) are tested in `auth.rs`; what stays
//! untested is the retry *loop* in `chat_stream`. The API-key 401 path — where
//! no retry is possible — is covered below.

use crate::agent::{MAX_TURNS, MessageSink, Session, SessionStats, Steer, TurnIo};
use crate::auth::Auth;
use axum::response::IntoResponse;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------- mock API

/// One scripted response.
enum Reply {
    /// An SSE body streamed back with 200.
    Sse(String),
    /// An HTTP error status and body.
    Status(u16, &'static str),
}

#[derive(Default)]
struct MockState {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Value>>,
}

struct MockApi {
    base_url: String,
    state: Arc<MockState>,
}

impl MockApi {
    /// The request body the loop sent on round trip `index`.
    fn request(&self, index: usize) -> Value {
        let requests = self.state.requests.lock().unwrap();
        requests
            .get(index)
            .unwrap_or_else(|| panic!("no request {index}; only {} were made", requests.len()))
            .clone()
    }

    fn request_count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }

    /// The `messages` array of request `index`, as (role, content) pairs.
    fn sent_roles(&self, index: usize) -> Vec<String> {
        self.request(index)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap_or("?").to_string())
            .collect()
    }
}

async fn mock_api(replies: Vec<Reply>) -> MockApi {
    async fn handle(
        axum::extract::State(state): axum::extract::State<Arc<MockState>>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        state.requests.lock().unwrap().push(body);
        let reply = state.replies.lock().unwrap().pop_front();
        match reply {
            Some(Reply::Sse(body)) => (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                body,
            )
                .into_response(),
            Some(Reply::Status(code, body)) => {
                (axum::http::StatusCode::from_u16(code).unwrap(), body).into_response()
            }
            // Running past the script is a test bug, not a scenario: fail loudly.
            None => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "mock: the script ran out of replies",
            )
                .into_response(),
        }
    }

    let state = Arc::new(MockState {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    });
    let app = axum::Router::new()
        .route("/chat/completions", axum::routing::post(handle))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    MockApi { base_url: format!("http://{addr}"), state }
}

/// SSE frames plus the trailing usage chunk and `[DONE]`, as the real API sends.
fn sse(frames: Vec<Value>, prompt_tokens: u64) -> Reply {
    let mut body = String::new();
    for frame in frames {
        body.push_str(&format!("data: {frame}\n\n"));
    }
    body.push_str(&format!(
        "data: {}\n\n",
        json!({
            "choices": [{ "delta": {} }],
            "usage": { "prompt_tokens": prompt_tokens, "completion_tokens": 7 },
        })
    ));
    body.push_str("data: [DONE]\n\n");
    Reply::Sse(body)
}

/// An assistant reply that is plain text, streamed in two deltas.
fn reply_text(text: &str, prompt_tokens: u64) -> Reply {
    let (head, tail) = text.split_at(text.len() / 2);
    sse(
        vec![
            json!({ "choices": [{ "delta": { "content": head } }] }),
            json!({ "choices": [{ "delta": { "content": tail } }] }),
        ],
        prompt_tokens,
    )
}

/// An assistant reply that calls tools. `calls` is (id, name, arguments).
fn reply_tools(calls: &[(&str, &str, Value)], prompt_tokens: u64) -> Reply {
    let deltas: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (id, name, args))| {
            json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": i, "id": id, "type": "function",
                "function": { "name": name, "arguments": args.to_string() },
            }] } }] })
        })
        .collect();
    sse(deltas, prompt_tokens)
}

// ---------------------------------------------------------------- test IO

/// Records what the loop reported, and can inject the two mid-turn inputs.
#[derive(Default)]
struct Recorder {
    notes: Vec<String>,
    content: String,
    tools_run: Vec<String>,
    results: Vec<String>,
    /// Handed back by `steer()` the first time it is polled, cancelling the
    /// in-flight request. `None` means steering never fires.
    steer: Option<Steer>,
    /// Handed back by `pending_line()` once, at the next tool-call boundary.
    queued_line: Option<String>,
}

impl Recorder {
    fn noted(&self, needle: &str) -> bool {
        self.notes.iter().any(|n| n.contains(needle))
    }
}

impl TurnIo for Recorder {
    fn content(&mut self, delta: &str) {
        self.content.push_str(delta);
    }
    fn note(&mut self, text: &str) {
        self.notes.push(text.to_string());
    }
    fn tool_call(&mut self, name: &str, _args: &Value) {
        self.tools_run.push(name.to_string());
    }
    fn tool_result(&mut self, _name: &str, result: &str) {
        self.results.push(result.to_string());
    }
    async fn steer(&mut self) -> Steer {
        match self.steer.take() {
            Some(steer) => steer,
            None => std::future::pending().await,
        }
    }
    async fn pending_line(&mut self) -> Option<String> {
        self.queued_line.take()
    }
}

/// A [MessageSink] that just records the calls, to check that a rewrite is
/// reported as clear-then-append.
#[derive(Default)]
struct SinkLog(Mutex<Vec<String>>);

impl MessageSink for SinkLog {
    fn appended(&self, index: usize, message: &Value) {
        self.0.lock().unwrap().push(format!(
            "append {index} {}",
            message["role"].as_str().unwrap_or("?")
        ));
    }
    fn cleared(&self) {
        self.0.lock().unwrap().push("clear".to_string());
    }
}

// ---------------------------------------------------------------- fixtures

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harness-loop-{}-{tag}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A session wired to the mock, built without credentials or a real system
/// prompt.
fn session(api: &MockApi, cwd: PathBuf) -> Session {
    Session {
        client: reqwest::Client::new(),
        auth: Auth::ApiKey("test-key".into()),
        model: "test-model".into(),
        base_url: api.base_url.clone(),
        cwd,
        yolo: true,
        cancel: Arc::new(AtomicBool::new(false)),
        messages: vec![json!({ "role": "system", "content": "test system prompt" })],
        stats: SessionStats::default(),
        sink: None,
        context_window: 1_000_000,
        compacted_after: 0,
    }
}

/// Assert the invariant the API enforces: every `tool_calls` entry has a
/// matching `role: "tool"` message. A history that breaks this fails every
/// later request, so it is checked after any test that interrupts a turn.
fn assert_tool_calls_all_answered(messages: &[Value]) {
    let answered: Vec<&str> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["tool_call_id"].as_str())
        .collect();
    for message in messages.iter().filter(|m| m["role"] == "assistant") {
        for call in message["tool_calls"].as_array().unwrap_or(&Vec::new()) {
            let id = call["id"].as_str().unwrap_or("");
            assert!(answered.contains(&id), "tool_call {id} has no result in the history");
        }
    }
}

// ---------------------------------------------------------------- the tests

#[tokio::test]
async fn a_plain_turn_streams_content_and_records_usage() {
    let api = mock_api(vec![reply_text("hello there", 120)]).await;
    let mut s = session(&api, std::env::temp_dir());
    let mut io = Recorder::default();

    s.push_user("hi");
    assert!(s.run_turn(&mut io).await.is_none(), "no fatal error");

    assert_eq!(io.content, "hello there", "both deltas reached the frontend");
    assert_eq!(s.stats.last.unwrap().prompt, 120, "usage is taken from the stream");
    assert_eq!(s.messages.len(), 3, "system, user, assistant");
    assert_eq!(s.messages[2]["content"], "hello there");
    assert!(api.request(0)["tools"].is_array(), "an agent turn offers tools");
}

#[tokio::test]
async fn a_tool_call_round_trip_feeds_the_result_back() {
    let dir = scratch("tool");
    std::fs::write(dir.join("f.txt"), "file body here").unwrap();
    let api = mock_api(vec![
        reply_tools(&[("call_1", "read_file", json!({ "path": "f.txt" }))], 100),
        reply_text("all done", 200),
    ])
    .await;
    let mut s = session(&api, dir);
    let mut io = Recorder::default();

    s.push_user("read it");
    assert!(s.run_turn(&mut io).await.is_none());

    assert_eq!(io.tools_run, ["read_file"]);
    // system, user, assistant(tool_calls), tool, assistant
    assert_eq!(s.messages.len(), 5);
    assert_eq!(s.messages[3]["role"], "tool");
    assert_eq!(s.messages[3]["tool_call_id"], "call_1");
    assert!(s.messages[3]["content"].as_str().unwrap().contains("file body here"));
    assert_tool_calls_all_answered(&s.messages);
    // The second round trip must carry the result, or the model is answering blind.
    assert_eq!(api.sent_roles(1), ["system", "user", "assistant", "tool"]);
}

#[tokio::test]
async fn the_loop_stops_at_max_turns_instead_of_spinning() {
    let dir = scratch("max");
    std::fs::write(dir.join("f.txt"), "x").unwrap();
    let replies: Vec<Reply> = (0..MAX_TURNS + 5)
        .map(|i| {
            reply_tools(
                &[(&format!("call_{i}"), "read_file", json!({ "path": "f.txt" }))],
                100,
            )
        })
        .collect();
    let api = mock_api(replies).await;
    let mut s = session(&api, dir);
    let mut io = Recorder::default();

    s.push_user("loop forever");
    assert!(s.run_turn(&mut io).await.is_none(), "hitting the bound is not a fatal error");

    assert_eq!(api.request_count(), MAX_TURNS, "exactly one round trip per turn, then stop");
    assert!(io.noted("MAX_TURNS"), "the user is told why it stopped: {:?}", io.notes);
    assert_tool_calls_all_answered(&s.messages);
}

/// A web session must not be able to kill the server, so API failures are
/// returned rather than fatal to the process — and the session keeps working.
#[tokio::test]
async fn an_api_error_ends_the_turn_but_not_the_session() {
    let api = mock_api(vec![
        Reply::Status(500, "upstream exploded"),
        reply_text("fine now", 100),
    ])
    .await;
    let mut s = session(&api, std::env::temp_dir());
    let mut io = Recorder::default();

    s.push_user("first");
    let fatal = s.run_turn(&mut io).await.expect("the turn reports the failure");
    assert!(fatal.contains("500"), "{fatal}");
    assert!(fatal.contains("upstream exploded"), "the body is surfaced: {fatal}");

    // The same session runs the next turn normally.
    s.push_user("second");
    assert!(s.run_turn(&mut io).await.is_none());
    assert_eq!(io.content, "fine now");
}

#[tokio::test]
async fn a_401_on_an_api_key_says_what_to_do_about_it() {
    let api = mock_api(vec![Reply::Status(401, "bad key")]).await;
    let mut s = session(&api, std::env::temp_dir());
    let mut io = Recorder::default();

    s.push_user("hi");
    let fatal = s.run_turn(&mut io).await.expect("401 ends the turn");
    assert!(fatal.contains("401"), "{fatal}");
    assert!(fatal.contains("KIMI_API_KEY") || fatal.contains("kimi"), "actionable: {fatal}");
    // An API key cannot be refreshed, so there is no second attempt.
    assert_eq!(api.request_count(), 1);
}

/// Interrupting between tool calls is the case most likely to corrupt a
/// history: the model asked for two tools, the user changed direction after
/// the first. Both calls must still be answered.
#[tokio::test]
async fn guidance_at_a_tool_boundary_still_answers_every_call() {
    let dir = scratch("boundary");
    std::fs::write(dir.join("f.txt"), "body").unwrap();
    let api = mock_api(vec![
        reply_tools(
            &[
                ("call_a", "read_file", json!({ "path": "f.txt" })),
                ("call_b", "read_file", json!({ "path": "f.txt" })),
            ],
            100,
        ),
        reply_text("switching", 150),
    ])
    .await;
    let mut s = session(&api, dir);
    let mut io = Recorder { queued_line: Some("stop, do X instead".into()), ..Default::default() };

    s.push_user("read both");
    assert!(s.run_turn(&mut io).await.is_none());

    assert!(io.tools_run.is_empty(), "the guidance arrived before either tool ran");
    assert_tool_calls_all_answered(&s.messages);
    let tool_messages: Vec<&Value> =
        s.messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tool_messages.len(), 2, "both calls answered even though neither ran");
    for message in tool_messages {
        assert!(message["content"].as_str().unwrap().contains("interrupted by user"));
    }
    // The guidance joins the history and the turn continues from it.
    assert!(
        s.messages.iter().any(|m| m["role"] == "user" && m["content"] == "stop, do X instead"),
        "{:#?}",
        s.messages
    );
    assert_eq!(io.content, "switching");
}

#[tokio::test]
async fn an_interruption_while_waiting_ends_the_turn_cleanly() {
    let api = mock_api(vec![reply_text("never seen", 100)]).await;
    let mut s = session(&api, std::env::temp_dir());
    let mut io = Recorder { steer: Some(Steer::Interrupted), ..Default::default() };

    s.push_user("hi");
    assert!(s.run_turn(&mut io).await.is_none(), "an interrupt is not a fatal error");

    assert!(io.noted("interrupted"), "{:?}", io.notes);
    // The partial reply is discarded: only the system prompt and the user line.
    assert_eq!(s.messages.len(), 2, "{:#?}", s.messages);
}

#[tokio::test]
async fn compaction_fires_inside_the_loop_and_asks_without_tools() {
    let api = mock_api(vec![
        reply_text("first answer", 900), // pushes the measured prompt over the trigger
        reply_text("A SUMMARY OF EARLIER WORK", 400), // the compaction side request
        reply_text("second answer", 200), // the real second turn
    ])
    .await;
    let mut s = session(&api, std::env::temp_dir());
    s.context_window = 1_000; // trigger at 750, keep-tail budget 300
    let mut io = Recorder::default();

    s.push_user(&"x".repeat(4_000)); // ~1000 estimated tokens: too big to keep
    assert!(s.run_turn(&mut io).await.is_none());
    assert_eq!(s.messages.len(), 3);

    s.push_user("next");
    assert!(s.run_turn(&mut io).await.is_none());

    assert!(io.noted("compacting"), "{:?}", io.notes);
    assert!(io.noted("compacted"), "{:?}", io.notes);
    assert_eq!(api.request_count(), 3, "turn, summary, turn");
    assert!(
        api.request(1)["tools"].is_null(),
        "the summary request must not offer tools: {}",
        api.request(1)
    );
    // History is system + summary + retained tail; the big message is gone.
    assert_eq!(s.messages[0]["role"], "system");
    let summary = s.messages[1]["content"].as_str().unwrap();
    assert!(summary.contains("A SUMMARY OF EARLIER WORK"), "{summary}");
    assert!(
        !s.messages.iter().any(|m| m["content"].as_str().is_some_and(|c| c.len() == 4_000)),
        "the compacted-away message is gone"
    );
    // And the request that followed used the compacted history.
    assert!(api.request(2)["messages"].as_array().unwrap().len() < 5);
}

/// Running out of context is bad; destroying the conversation because the
/// summarizer was unreachable is worse.
#[tokio::test]
async fn a_failed_summary_leaves_the_history_untouched() {
    let api = mock_api(vec![
        reply_text("first answer", 900),
        Reply::Status(503, "summarizer down"),
        reply_text("second answer", 200),
    ])
    .await;
    let mut s = session(&api, std::env::temp_dir());
    s.context_window = 1_000;
    let mut io = Recorder::default();

    s.push_user(&"x".repeat(4_000));
    assert!(s.run_turn(&mut io).await.is_none());
    let before = s.messages.clone();

    s.push_user("next");
    assert!(s.run_turn(&mut io).await.is_none(), "the turn still succeeds");

    assert!(io.noted("compaction skipped"), "{:?}", io.notes);
    assert_eq!(s.messages[..before.len()], before[..], "the old history is intact");
    assert_eq!(io.content, "first answersecond answer", "the turn ran anyway");
}

#[tokio::test]
async fn compaction_reports_the_rewrite_to_the_sink_as_clear_then_append() {
    let api = mock_api(vec![
        reply_text("first answer", 900),
        reply_text("SUMMARY", 400),
        reply_text("second answer", 200),
    ])
    .await;
    let log = Arc::new(SinkLog::default());
    let mut s = session(&api, std::env::temp_dir());
    s.context_window = 1_000;
    s.sink = Some(log.clone());
    let mut io = Recorder::default();

    s.push_user(&"x".repeat(4_000));
    assert!(s.run_turn(&mut io).await.is_none());
    s.push_user("next");
    assert!(s.run_turn(&mut io).await.is_none());

    let entries = log.0.lock().unwrap().clone();
    let clear_at = entries.iter().position(|e| e == "clear").expect("the rewrite cleared");
    // Everything after the clear is the rebuilt history, starting at index 1 —
    // index 0 is the system prompt, which is never stored.
    assert_eq!(entries[clear_at + 1], "append 1 user", "{entries:?}");
    assert!(
        entries[clear_at + 1..].iter().all(|e| e.starts_with("append ")),
        "no interleaving after the clear: {entries:?}"
    );
}
