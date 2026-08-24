//! The live run, end to end: a fake pi speaking the real protocol, driven
//! through the real socket.
//!
//! A fake rather than a real pi because the assertions here are about
//! *skiffd's* handling of the protocol — overlay assembly, coalescing, run
//! settlement, abort — and a real pi would make them depend on a model's
//! output. The protocol itself is pinned by `run::pi_rpc`'s tests and by a
//! smoke test against a real pi.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use skiff::ingest::source::Source;
use skiff::ingest::{Ingest, pi};
use skiff::run::Runs;
use skiff::server::{AppState, router};
use skiff::store::Store;
use skiff::views::ViewSpec;
use skiff::wire::{ClientFrame, Command, ServerFrame};

const HEADER: &str = r#"{"type":"session","cwd":"/tmp","timestamp":"2026-08-23T10:00:00.000Z"}"#;
const DEADLINE: Duration = Duration::from_secs(5);

/// A fake pi.
///
/// It behaves like the real one in the way that matters most here: it appends
/// the persisted entry to the session file at `message_end`, so the handover
/// from overlay to transcript is exercised for real rather than asserted
/// about. A prompt of "hang" streams forever, so a test can observe a run in
/// flight.
const FAKE_PI: &str = r#"
import sys, json, time, threading, queue

if "--list-models" in sys.argv:
    print("provider model context")
    print("deepseek deepseek-v4-flash 128k")
    print("anthropic claude-sonnet-4 200k")
    sys.exit(0)

session = sys.argv[sys.argv.index("--session") + 1]
lock = threading.Lock()
commands = queue.Queue()
aborted = threading.Event()

def emit(**kw):
    with lock:
        print(json.dumps(kw), flush=True)

# The real pi reads stdin while it works, so abort can arrive mid-run. A fake
# that only read between turns would make the abort test pass for the wrong
# reason — or, as it did, hang forever.
def reader():
    for line in sys.stdin:
        cmd = json.loads(line)
        if cmd["type"] == "abort":
            aborted.set()
            emit(type="response", id=cmd["id"], success=True)
            emit(type="agent_end", willRetry=False)
        else:
            commands.put(cmd)

threading.Thread(target=reader, daemon=True).start()

def persist(entry):
    with open(session, "a") as f:
        f.write(json.dumps(entry) + "\n")

parent = [None]
def append_entry(entry):
    entry["parentId"] = parent[0]
    parent[0] = entry["id"]
    persist(entry)

seq = [0]
def next_id():
    seq[0] += 1
    return "e%d" % seq[0]

while True:
    cmd = commands.get()
    if cmd["type"] == "set_session_name":
        append_entry({"id": next_id(), "type": "session_info", "name": cmd["name"]})
        emit(type="response", id=cmd["id"], success=True)
        continue
    if cmd["type"] == "set_model":
        append_entry({"id": next_id(), "type": "model_change",
                      "provider": cmd["provider"], "modelId": cmd["modelId"]})
        emit(type="response", id=cmd["id"], success=True)
        continue
    if cmd["type"] != "prompt":
        continue

    if cmd["message"].startswith("/orchestrator "):
        active = cmd["message"].endswith(" on")
        append_entry({"id": next_id(), "type": "custom",
                      "customType": "orchestrator-mode", "data": {"active": active}})
        emit(type="response", id=cmd["id"], success=True)
        continue

    aborted.clear()
    emit(type="response", id=cmd["id"], success=True)
    # The real pi persists the user's turn before it starts working.
    append_entry({"id": next_id(), "type": "message",
                  "timestamp": "2026-08-23T10:00:01.000Z",
                  "message": {"role": "user", "content": cmd["message"]}})
    emit(type="agent_start")
    emit(type="message_start", message={"role": "assistant", "model": "fake-1"})

    if cmd["message"] == "hang":
        while not aborted.is_set():
            emit(type="message_update", assistantMessageEvent={
                "type": "text_delta", "contentIndex": 0, "delta": "x"})
            time.sleep(0.01)
        continue

    emit(type="message_update", assistantMessageEvent={
        "type": "thinking_delta", "contentIndex": 0, "delta": "considering"})
    text = ""
    for piece in ["Hello", " from", " a fake"]:
        text += piece
        emit(type="message_update", assistantMessageEvent={
            "type": "text_delta", "contentIndex": 1, "delta": piece})
        time.sleep(0.02)
    append_entry({"id": next_id(), "type": "message",
                  "timestamp": "2026-08-23T10:00:02.000Z",
                  "message": {"role": "assistant", "model": "fake-1",
                              "content": [{"type": "thinking", "thinking": "considering"},
                                          {"type": "text", "text": text}]}})
    emit(type="message_end")
    emit(type="agent_settled")
"#;

/// A fake pi that asks a blocking question the moment it is prompted.
const DIALOG_PI: &str = r#"
import sys, json
for line in sys.stdin:
    cmd = json.loads(line)
    if cmd["type"] == "prompt":
        print(json.dumps({"type": "response", "id": cmd["id"], "success": True}), flush=True)
        print(json.dumps({"type": "extension_ui_request", "id": "d1",
                          "method": "confirm", "message": "ok?"}), flush=True)
        # Blocked until answered. If skiffd never cancels, this hangs forever.
        reply = json.loads(sys.stdin.readline())
        print(json.dumps({"type": "agent_start"}), flush=True)
        print(json.dumps({"type": "message_start",
                          "message": {"role": "assistant"}}), flush=True)
        print(json.dumps({"type": "message_update", "assistantMessageEvent": {
            "type": "text_delta", "contentIndex": 0,
            "delta": "cancelled=%s" % reply.get("cancelled")}}), flush=True)
"#;

/// A fake pi that rejects everything.
const REFUSING_PI: &str = r#"
import sys, json
for line in sys.stdin:
    cmd = json.loads(line)
    print(json.dumps({"type": "response", "id": cmd["id"],
                      "success": False, "error": "busy elsewhere"}), flush=True)
"#;

struct Harness {
    addr: SocketAddr,
    _sessions: tempfile::TempDir,
    _dist: tempfile::TempDir,
}

fn fake(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("fake-pi");
    std::fs::write(&path, format!("#!/usr/bin/env python3\n{script}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// These tests exercise one source; the multi-source scan is covered in
/// `ingest`'s own tests.
fn pi_source(root: &Path) -> Vec<Box<dyn Source>> {
    vec![Box::new(pi::Pi::new(root.to_path_buf()))]
}

async fn start(script: &str) -> Harness {
    let sessions = tempfile::tempdir().unwrap();
    std::fs::write(sessions.path().join("abc.jsonl"), format!("{HEADER}\n")).unwrap();

    let store = Store::in_memory().unwrap();
    let (topics, _) = tokio::sync::broadcast::channel(256);
    let ingest = Ingest::new(store.clone(), pi_source(sessions.path()), topics.clone());
    ingest.scan().unwrap();
    // The watcher runs for real: these tests turn on the file landing.
    Ingest::new(store.clone(), pi_source(sessions.path()), topics.clone()).spawn();

    let binary = fake(sessions.path(), script);
    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        binary,
        sessions.path().to_path_buf(),
        true,
    );

    // The same handover wiring `main` installs: a finished reply moves from
    // the overlay to the transcript when the *file* catches up.
    tokio::spawn({
        let runs = runs.clone();
        let mut topics = topics.subscribe();
        async move {
            while let Ok(topic) = topics.recv().await {
                if let skiff::ingest::Topic::Session(id) = topic {
                    runs.session_changed(&id).await;
                }
            }
        }
    });

    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<!doctype html>").unwrap();
    let app = router(
        AppState::new(store, runs, topics),
        dist.path().to_path_buf(),
    );

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Harness {
        addr,
        _sessions: sessions,
        _dist: dist,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    socket
}

async fn send(socket: &mut Socket, frame: ClientFrame) {
    socket
        .send(Message::text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();
}

async fn next_frame(socket: &mut Socket) -> ServerFrame {
    loop {
        let message = tokio::time::timeout(DEADLINE, socket.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("the socket closed")
            .expect("a socket error");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("an unrecognised server frame");
        }
    }
}

fn session() -> skiff::model::SessionKey {
    "pi:abc".parse().unwrap()
}

/// Subscribe and consume the greeting and the first snapshot.
async fn subscribe(socket: &mut Socket) {
    send(
        socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Session { id: session() },
        },
    )
    .await;
    loop {
        match next_frame(socket).await {
            ServerFrame::Hello { .. } => continue,
            ServerFrame::Snapshot { .. } => return,
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }
}

/// Wait for a `live` frame satisfying `want`, ignoring the rest.
async fn live_until(
    socket: &mut Socket,
    want: impl Fn(&skiff::run::LiveState) -> bool,
) -> skiff::run::LiveState {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the condition never held"
        );
        match next_frame(socket).await {
            ServerFrame::Live { live, .. } if want(&live) => return live,
            ServerFrame::Error { error, .. } => panic!("server error: {error}"),
            _ => continue,
        }
    }
}

fn reply_text(live: &skiff::run::LiveState) -> String {
    use skiff::content::{Block, Inline};
    use skiff::model::Part;
    let Some(message) = &live.pending else {
        return String::new();
    };
    message
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { blocks } => Some(blocks),
            _ => None,
        })
        .flatten()
        .map(|b| match b {
            Block::Paragraph { inlines } => inlines
                .iter()
                .map(|i| match i {
                    Inline::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect::<String>(),
            _ => String::new(),
        })
        .collect()
}

#[tokio::test]
async fn a_prompt_streams_a_reply_and_then_settles() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;

    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "go".into(),
                client_id: "c-1".into(),
            },
        },
    )
    .await;

    // The sender's own message appears immediately, without waiting for pi.
    let live = live_until(&mut socket, |l| l.pending_prompt.is_some()).await;
    let prompt = live.pending_prompt.unwrap();
    assert_eq!(prompt.client_id, "c-1");
    assert_eq!(prompt.text, "go");

    let live = live_until(&mut socket, |l| reply_text(l).contains("a fake")).await;
    assert!(live.working, "the run is in flight while the reply streams");
    assert_eq!(reply_text(&live), "Hello from a fake");

    let pending = live.pending.as_ref().unwrap();
    assert!(
        pending.id.starts_with("run:"),
        "the overlay carries a run id: {}",
        pending.id
    );
    assert_eq!(
        pending.completed_ms, None,
        "a live reply is not recorded as finished"
    );
    assert_eq!(pending.agent.as_deref(), Some("fake-1"));

    // The handover: the overlay is released only once the persisted entry is
    // actually in the transcript. Dropping it at `message_end` would make the
    // finished reply vanish and reappear a moment later.
    let live = live_until(&mut socket, |l| !l.working && l.pending.is_none()).await;
    assert_eq!(live.pending, None);
    assert_eq!(
        live.pending_prompt, None,
        "the sent prompt is in the transcript now"
    );
}

#[tokio::test]
async fn a_finished_reply_is_never_absent_from_both_the_overlay_and_the_transcript() {
    // The flicker this guards against: pi writes the entry at `message_end`,
    // but skiffd does not see it until the watcher fires. Between those two
    // moments the reply must still be on screen.
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "go".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;

    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut seen_reply = false;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the reply never reached the transcript"
        );
        match next_frame(&mut socket).await {
            ServerFrame::Live { live, .. } => {
                if reply_text(&live).contains("a fake") {
                    seen_reply = true;
                } else if seen_reply && live.pending.is_none() {
                    // The overlay was released. The transcript must already
                    // hold the reply — the snapshot that carried it precedes
                    // this frame.
                    return;
                }
            }
            ServerFrame::Snapshot { data, .. } => {
                let skiff::views::ViewData::Session(view) = data else {
                    continue;
                };
                if view
                    .messages
                    .iter()
                    .any(|m| m.role == skiff::model::Role::Assistant)
                {
                    seen_reply = true;
                }
            }
            _ => continue,
        }
    }
}

#[tokio::test]
async fn reasoning_streams_as_reasoning_not_as_the_reply() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "go".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;

    let live = live_until(&mut socket, |l| {
        l.pending.as_ref().is_some_and(|m| {
            m.parts
                .iter()
                .any(|p| matches!(p, skiff::model::Part::Reasoning { .. }))
        })
    })
    .await;
    // And it is not mistaken for the reply itself.
    assert!(!reply_text(&live).contains("considering"));
}

#[tokio::test]
async fn a_command_is_acknowledged_by_its_request_id() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 42,
            cmd: Command::Send {
                session: session(),
                text: "go".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no ack arrived");
        if let ServerFrame::Ack { req } = next_frame(&mut socket).await {
            assert_eq!(req, 42);
            return;
        }
    }
}

#[tokio::test]
async fn a_session_snapshot_carries_pis_authoritative_model_catalog() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    send(
        &mut socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Session { id: session() },
        },
    )
    .await;

    loop {
        if let ServerFrame::Snapshot { data, .. } = next_frame(&mut socket).await {
            let skiff::views::ViewData::Session(view) = data else {
                panic!("expected a session view")
            };
            assert_eq!(view.models.error, None);
            assert_eq!(view.models.options.len(), 2);
            assert_eq!(view.models.options[0].provider, "deepseek");
            assert_eq!(view.models.options[0].id, "deepseek-v4-flash");
            return;
        }
    }
}

#[tokio::test]
async fn pi_capability_commands_are_acknowledged_and_persist_through_ingest() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;

    let commands = [
        Command::Rename {
            session: session(),
            name: "Renamed here".into(),
        },
        Command::SetModel {
            session: session(),
            provider: "anthropic".into(),
            model_id: "claude-sonnet-4".into(),
        },
        Command::SetOrchestrator {
            session: session(),
            active: true,
        },
    ];
    for (index, cmd) in commands.into_iter().enumerate() {
        let req = 50 + index as u32;
        send(&mut socket, ClientFrame::Command { req, cmd }).await;
        loop {
            match next_frame(&mut socket).await {
                ServerFrame::Ack { req: answer } if answer == req => break,
                ServerFrame::Error { error, .. } => panic!("capability command failed: {error}"),
                _ => {}
            }
        }
    }

    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the file-backed summary never converged"
        );
        match next_frame(&mut socket).await {
            ServerFrame::Snapshot { data, .. } => {
                let skiff::views::ViewData::Session(view) = data else {
                    continue;
                };
                let summary = view.session.expect("the session still exists");
                if summary.title.as_deref() == Some("Renamed here")
                    && summary.model.as_deref() == Some("claude-sonnet-4")
                    && summary.orchestrator_active
                {
                    return;
                }
            }
            ServerFrame::Error { error, .. } => panic!("command failed: {error}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_rejected_prompt_reports_pis_reason_and_clears_the_pending_bubble() {
    let harness = start(REFUSING_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 7,
            cmd: Command::Send {
                session: session(),
                text: "secret prompt text".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;

    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no error arrived");
        if let ServerFrame::Error { req, error, .. } = next_frame(&mut socket).await {
            assert_eq!(req, Some(7));
            assert!(error.contains("busy elsewhere"), "pi's own reason: {error}");
            assert!(
                !error.contains("secret prompt text"),
                "an error must never echo the prompt: {error}"
            );
            return;
        }
    }
}

#[tokio::test]
async fn abort_ends_the_run_and_drops_a_reply_that_will_never_be_persisted() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;

    // "hang" streams forever, so the run is observably in flight when the
    // abort lands.
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "hang".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;
    live_until(&mut socket, |l| l.working && l.pending.is_some()).await;

    send(
        &mut socket,
        ClientFrame::Command {
            req: 2,
            cmd: Command::Abort { session: session() },
        },
    )
    .await;

    let live = live_until(&mut socket, |l| !l.working && l.pending.is_none()).await;
    assert!(!live.working);
    assert_eq!(
        live.pending, None,
        "an aborted reply is never persisted, so it must not linger"
    );
}

#[tokio::test]
async fn a_blocking_dialog_is_cancelled_rather_than_wedging_the_session() {
    // There is no human on the other end of this socket. A dialog that is
    // never answered blocks the agent, and with it every viewer of the
    // session, forever.
    let harness = start(DIALOG_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "go".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;

    let live = live_until(&mut socket, |l| reply_text(l).contains("cancelled")).await;
    assert_eq!(
        reply_text(&live),
        "cancelled=True",
        "the dialog was declined, not answered"
    );
}

#[tokio::test]
async fn aborting_an_idle_session_is_not_an_error() {
    // It is what the button does when the run finished a moment ago.
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 3,
            cmd: Command::Abort { session: session() },
        },
    )
    .await;

    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no answer arrived");
        match next_frame(&mut socket).await {
            ServerFrame::Ack { req } => {
                assert_eq!(req, 3);
                return;
            }
            ServerFrame::Error { error, .. } => panic!("aborting an idle session errored: {error}"),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn a_session_skiff_cannot_run_is_refused_by_name() {
    // Without this, a prompt to a muse session would resolve a file through
    // pi's layout and spawn pi against a muse log.
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 8,
            cmd: Command::Send {
                session: "muse:abc".parse().unwrap(),
                text: "go".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no error arrived");
        if let ServerFrame::Error { req, error, .. } = next_frame(&mut socket).await {
            assert_eq!(req, Some(8));
            assert!(
                error.contains("muse"),
                "the refusal names the harness: {error}"
            );
            return;
        }
    }
}

#[tokio::test]
async fn an_empty_prompt_is_refused_before_pi_is_involved() {
    let harness = start(FAKE_PI).await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 5,
            cmd: Command::Send {
                session: session(),
                text: "   ".into(),
                client_id: "c".into(),
            },
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no error arrived");
        if let ServerFrame::Error { req, error, .. } = next_frame(&mut socket).await {
            assert_eq!(req, Some(5));
            assert!(error.contains("empty prompt"), "{error}");
            return;
        }
    }
}
