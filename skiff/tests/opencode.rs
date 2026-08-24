//! OpenCode integration through its real boundary: loopback HTTP, SSE events,
//! the derived store, live state, commands, and the browser WebSocket.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tokio_tungstenite::tungstenite::Message as SocketMessage;

use skiff::ingest::opencode::OpencodeIngest;
use skiff::model::{Message, Role};
use skiff::run::Runs;
use skiff::run::muse_exec::MuseConfig;
use skiff::server::{AppState, router};
use skiff::store::Store;
use skiff::views::ViewSpec;
use skiff::wire::{ClientFrame, Command, ServerFrame};

const SESSION_ID: &str = "ses_open";
const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct FakeOpenCode {
    session: Arc<Mutex<Value>>,
    messages: Arc<Mutex<Vec<Value>>>,
    events: broadcast::Sender<()>,
    event_connected: Arc<AtomicBool>,
}

impl FakeOpenCode {
    fn new() -> Self {
        let (events, _) = broadcast::channel(32);
        Self {
            session: Arc::new(Mutex::new(json!({
                "id": SESSION_ID,
                "title": "Open work",
                "directory": "/tmp",
                "time": { "created": 1, "updated": 1 }
            }))),
            messages: Arc::default(),
            events,
            event_connected: Arc::default(),
        }
    }
}

async fn list(State(fake): State<FakeOpenCode>) -> Json<Vec<Value>> {
    Json(vec![fake.session.lock().await.clone()])
}

async fn show(State(fake): State<FakeOpenCode>, Path(id): Path<String>) -> Response {
    if id != SESSION_ID {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(fake.session.lock().await.clone()).into_response()
}

async fn messages(State(fake): State<FakeOpenCode>, Path(id): Path<String>) -> Response {
    if id != SESSION_ID {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(fake.messages.lock().await.clone()).into_response()
}

async fn prompt(
    State(fake): State<FakeOpenCode>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> StatusCode {
    if id != SESSION_ID {
        return StatusCode::NOT_FOUND;
    }
    let text = body
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    {
        let mut messages = fake.messages.lock().await;
        messages.push(json!({
            "info": { "id": "user-1", "role": "user", "time": { "created": 2 } },
            "parts": [{ "type": "text", "text": text }]
        }));
        messages.push(json!({
            "info": { "id": "assistant-1", "role": "assistant", "modelID": "fake", "time": { "created": 3 } },
            "parts": [{ "type": "text", "text": "half" }]
        }));
    }
    let _ = fake.events.send(());
    if text != "hang" {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let mut messages = fake.messages.lock().await;
            let assistant = messages.last_mut().unwrap();
            assistant["info"]["time"]["completed"] = json!(4);
            assistant["parts"][0]["text"] = json!("OpenCode completed");
            drop(messages);
            let _ = fake.events.send(());
        });
    }
    StatusCode::NO_CONTENT
}

async fn abort(State(fake): State<FakeOpenCode>, Path(id): Path<String>) -> StatusCode {
    if id != SESSION_ID {
        return StatusCode::NOT_FOUND;
    }
    fake.messages.lock().await.retain(|message| {
        message["info"]["role"] != "assistant" || message["info"]["time"]["completed"].is_number()
    });
    let _ = fake.events.send(());
    StatusCode::NO_CONTENT
}

async fn rename(
    State(fake): State<FakeOpenCode>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> StatusCode {
    if id != SESSION_ID {
        return StatusCode::NOT_FOUND;
    }
    fake.session.lock().await["title"] = body["title"].clone();
    let _ = fake.events.send(());
    StatusCode::NO_CONTENT
}

async fn events(State(fake): State<FakeOpenCode>) -> Response {
    fake.event_connected.store(true, Ordering::SeqCst);
    let receiver = fake.events.subscribe();
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(()) => {
                    let bytes = Bytes::from_static(
                        b"data: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_open\"}}\n\n",
                    );
                    return Some((Ok::<_, Infallible>(bytes), receiver));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

struct Harness {
    addr: SocketAddr,
    _api: tokio::task::JoinHandle<()>,
    _app: tokio::task::JoinHandle<()>,
    _dist: tempfile::TempDir,
    _state: FakeOpenCode,
}

async fn start() -> Harness {
    let fake = FakeOpenCode::new();
    let api = Router::new()
        .route("/session", get(list))
        .route("/session/{id}", get(show).patch(rename))
        .route("/session/{id}/message", get(messages))
        .route("/session/{id}/prompt_async", post(prompt))
        .route("/session/{id}/abort", post(abort))
        .route("/event", get(events))
        .with_state(fake.clone());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let api_addr = listener.local_addr().unwrap();
    let api_task = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });

    let store = Store::in_memory().unwrap();
    let (topics, _) = broadcast::channel(256);
    let missing = tempfile::tempdir().unwrap();
    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        missing.path().join("pi"),
        missing.path().join("pi-sessions"),
        true,
        MuseConfig {
            binary: missing.path().join("muse"),
            session_dir: missing.path().join("data/muse/sessions"),
            session_dir_explicit: true,
        },
        &format!("http://{api_addr}"),
    );
    OpencodeIngest::new(
        runs.opencode().client(),
        store.clone(),
        runs.opencode(),
        topics.clone(),
    )
    .spawn();

    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<!doctype html>").unwrap();
    let app = router(
        AppState::new(store, runs, topics),
        PathBuf::from(dist.path()),
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    tokio::time::timeout(DEADLINE, async {
        while !fake.event_connected.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("OpenCode event stream was never connected");

    Harness {
        addr,
        _api: api_task,
        _app: app_task,
        _dist: dist,
        _state: fake,
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
            .expect("timed out waiting for OpenCode")
            .expect("the socket closed")
            .expect("the socket failed");
        if let SocketMessage::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

fn session() -> skiff::model::SessionKey {
    format!("opencode:{SESSION_ID}").parse().unwrap()
}

async fn subscribe_session(socket: &mut Socket) {
    send(
        socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Session { id: session() },
        },
    )
    .await;
    loop {
        if let ServerFrame::Snapshot { data, .. } = next(socket).await {
            let skiff::views::ViewData::Session(view) = data else {
                continue;
            };
            if view.session.is_some() {
                return;
            }
        }
    }
}

fn text(message: &Message) -> String {
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

#[tokio::test]
async fn opencode_streams_then_commits_and_renames_through_one_socket() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    subscribe_session(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "hello".into(),
                client_id: "o-1".into(),
            },
        },
    )
    .await;

    let mut saw_pending_prompt = false;
    let mut saw_stream = false;
    let mut saw_commit = false;
    while !(saw_pending_prompt && saw_stream && saw_commit) {
        match next(&mut socket).await {
            ServerFrame::Live { live, .. } => {
                saw_pending_prompt |= live.pending_prompt.is_some();
                saw_stream |= live
                    .pending
                    .as_ref()
                    .is_some_and(|message| text(message) == "half");
            }
            ServerFrame::Snapshot { data, .. } => {
                let skiff::views::ViewData::Session(view) = data else {
                    continue;
                };
                saw_commit |= view.messages.iter().any(|message| {
                    message.role == Role::Assistant && text(message) == "OpenCode completed"
                });
            }
            ServerFrame::Error { error, .. } => panic!("OpenCode failed: {error}"),
            _ => {}
        }
    }

    send(
        &mut socket,
        ClientFrame::Command {
            req: 2,
            cmd: Command::Rename {
                session: session(),
                name: "Renamed remotely".into(),
            },
        },
    )
    .await;
    loop {
        match next(&mut socket).await {
            ServerFrame::Snapshot { data, .. } => {
                let skiff::views::ViewData::Session(view) = data else {
                    continue;
                };
                if view.session.and_then(|session| session.title).as_deref()
                    == Some("Renamed remotely")
                {
                    return;
                }
            }
            ServerFrame::Error { error, .. } => panic!("OpenCode rename failed: {error}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn opencode_abort_converges_to_idle_without_a_ghost_reply() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    subscribe_session(&mut socket).await;
    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::Send {
                session: session(),
                text: "hang".into(),
                client_id: "o-2".into(),
            },
        },
    )
    .await;
    loop {
        if let ServerFrame::Live { live, .. } = next(&mut socket).await
            && live.working
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
            ServerFrame::Error { error, .. } => panic!("OpenCode abort failed: {error}"),
            _ => {}
        }
    }
}
