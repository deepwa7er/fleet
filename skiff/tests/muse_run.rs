//! Muse live-run integration: the file adapter and per-prompt process converge
//! through the same WebSocket the browser uses.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as SocketMessage;

use skiff::ingest::source::Source;
use skiff::ingest::{Ingest, muse};
use skiff::run::Runs;
use skiff::run::muse_exec::MuseConfig;
use skiff::server::{AppState, router};
use skiff::store::Store;
use skiff::views::ViewSpec;
use skiff::wire::{ClientFrame, Command, ServerFrame};

const SESSION_ID: &str = "26ea1b5e-0000-4000-8000-0000000000f1";
const DEADLINE: Duration = Duration::from_secs(5);

const FAKE_MUSE: &str = r#"
import sys, json, time, pathlib, uuid, signal

args = sys.argv
session_id = args[args.index("--session-id") + 1]
prompt_file = pathlib.Path(args[args.index("--prompt-file") + 1])
prompt = prompt_file.read_text()
root = pathlib.Path(__import__("os").environ["XDG_DATA_HOME"]) / "muse" / "sessions"
files = list(root.glob("*/*/*/%s/session.jsonl" % session_id))
if not files:
    raise SystemExit("session file not found")
session_file = files[0]

def envelope(payload_type, payload):
    return {"id": str(uuid.uuid4()), "recorded_at": int(time.time() * 1000000),
            "payload_type": payload_type, "payload": payload}

def append(event):
    record = envelope("runtime.session", {"kind": "run", "event": event})
    with session_file.open("a") as output:
        output.write(json.dumps(record) + "\n")

print(json.dumps(envelope("runtime.command.accepted", {"kind": "command_accepted"})), flush=True)
append({"kind": "started", "prompt": prompt})

if prompt == "hang":
    while True:
        print(json.dumps(envelope("run.output.delta", {"text": "x"})), flush=True)
        time.sleep(0.02)

pieces = [prompt[:max(1, len(prompt)//2)], prompt[max(1, len(prompt)//2):]]
for piece in pieces:
    if piece:
        print(json.dumps(envelope("run.output.delta", {"text": piece})), flush=True)
        time.sleep(0.04)
append({"kind": "assistant_message_committed", "message_id": str(uuid.uuid4()), "text": prompt})
append({"kind": "terminal", "terminal": "completed"})
print(json.dumps(envelope("run.terminal.completed", {"terminal": "completed"})), flush=True)
"#;

struct Harness {
    addr: SocketAddr,
    _root: tempfile::TempDir,
    _dist: tempfile::TempDir,
}

fn executable(dir: &Path) -> PathBuf {
    let path = dir.join("fake-muse");
    std::fs::write(&path, format!("#!/usr/bin/env python3\n{FAKE_MUSE}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn start() -> Harness {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("data/muse/sessions");
    let destination = sessions.join(format!("2026/08/10/{SESSION_ID}"));
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "bridge/test/fixtures/muse-sessions/2026/08/10/{SESSION_ID}/session.jsonl"
        )),
        destination.join("session.jsonl"),
    )
    .unwrap();

    let store = Store::in_memory().unwrap();
    let (topics, _) = tokio::sync::broadcast::channel(256);
    let sources = || -> Vec<Box<dyn Source>> { vec![Box::new(muse::Muse::new(sessions.clone()))] };
    Ingest::new(store.clone(), sources(), topics.clone())
        .scan()
        .unwrap();
    Ingest::new(store.clone(), sources(), topics.clone()).spawn();

    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        root.path().join("missing-pi"),
        root.path().join("pi"),
        true,
        MuseConfig {
            binary: executable(root.path()),
            session_dir: sessions,
            session_dir_explicit: true,
        },
        "http://127.0.0.1:1",
    );
    tokio::spawn({
        let runs = runs.clone();
        let mut receiver = topics.subscribe();
        async move {
            while let Ok(topic) = receiver.recv().await {
                if let skiff::ingest::Topic::Session(session) = topic {
                    runs.session_changed(&session).await;
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
        _root: root,
        _dist: dist,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr) -> Socket {
    tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap()
        .0
}

async fn send(socket: &mut Socket, frame: ClientFrame) {
    socket
        .send(SocketMessage::text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();
}

async fn next(socket: &mut Socket) -> ServerFrame {
    loop {
        let message = tokio::time::timeout(DEADLINE, socket.next())
            .await
            .expect("timed out waiting for Muse")
            .expect("the socket closed")
            .expect("the socket failed");
        if let SocketMessage::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

fn session() -> skiff::model::SessionKey {
    format!("muse:{SESSION_ID}").parse().unwrap()
}

fn message_text(message: &skiff::model::Message) -> String {
    use skiff::content::{Block, Inline};
    use skiff::model::Part;

    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { blocks } => Some(blocks),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            Block::Paragraph { inlines } => Some(inlines),
            _ => None,
        })
        .flatten()
        .filter_map(|inline| match inline {
            Inline::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn subscribe(socket: &mut Socket) {
    send(
        socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Session { id: session() },
        },
    )
    .await;
    while !matches!(next(socket).await, ServerFrame::Snapshot { .. }) {}
}

#[tokio::test]
async fn a_muse_prompt_streams_and_hands_over_to_the_file_transcript() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "Muse replies here".into(),
                client_id: "m-1".into(),
            },
        },
    )
    .await;

    let mut saw_ack = false;
    let mut saw_overlay = false;
    let mut saw_committed = false;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while tokio::time::Instant::now() < deadline {
        match next(&mut socket).await {
            ServerFrame::Ack { req: 1 } => saw_ack = true,
            ServerFrame::Live { live, .. } => {
                if live
                    .pending
                    .as_ref()
                    .is_some_and(|message| message.id.contains(":muse:"))
                {
                    saw_overlay = true;
                }
            }
            ServerFrame::Snapshot { data, .. } => {
                let skiff::views::ViewData::Session(view) = data else {
                    continue;
                };
                if view.messages.iter().any(|message| {
                    message.role == skiff::model::Role::Assistant
                        && message_text(message).contains("Muse replies here")
                }) {
                    saw_committed = true;
                }
            }
            ServerFrame::Error { error, .. } => panic!("Muse command failed: {error}"),
            _ => {}
        }
        if saw_ack && saw_overlay && saw_committed {
            return;
        }
    }
    panic!("Muse did not converge: ack={saw_ack} overlay={saw_overlay} committed={saw_committed}");
}

#[tokio::test]
async fn aborting_muse_drops_the_uncommitted_overlay() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    subscribe(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "hang".into(),
                client_id: "m-2".into(),
            },
        },
    )
    .await;
    loop {
        if let ServerFrame::Live { live, .. } = next(&mut socket).await
            && live.working
            && live.pending.is_some()
        {
            break;
        }
    }
    send(
        &mut socket,
        ClientFrame::Command {
            req: 2,
            cmd: Command::Abort { session: session() },
        },
    )
    .await;
    loop {
        match next(&mut socket).await {
            ServerFrame::Live { live, .. }
                if !live.working && live.pending.is_none() && live.pending_prompt.is_none() =>
            {
                return;
            }
            ServerFrame::Error { error, .. } => panic!("Muse abort failed: {error}"),
            _ => {}
        }
    }
}
