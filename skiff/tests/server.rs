//! End-to-end: a real socket, the real protocol, real files on disk.
//!
//! The unit tests cover each layer; this covers the seam the browser actually
//! speaks. It asserts the two properties the whole design rests on — that a
//! subscription answers with a snapshot, and that a file appearing on disk
//! pushes a new one without the client asking.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use skiff::ingest::{Ingest, pi};
use skiff::ingest::source::Source;
use skiff::run::Runs;
use skiff::run::muse_exec::MuseConfig;
use skiff::server::{AppState, router};
use skiff::store::Store;
use skiff::wire::{ClientFrame, ServerFrame};
use skiff::views::{ViewData, ViewSpec};

const HEADER: &str = r#"{"type":"session","cwd":"/home/x","timestamp":"2026-08-23T10:00:00.000Z"}"#;

/// Frames must arrive promptly; a test that hangs forever on a protocol bug is
/// worse than one that fails.
const DEADLINE: Duration = Duration::from_secs(5);

struct Harness {
    addr: SocketAddr,
    sessions: tempfile::TempDir,
    ingest: Ingest,
}

/// These tests exercise one source; the multi-source scan is covered in
/// `ingest`'s own tests.
fn pi_source(root: &Path) -> Vec<Box<dyn Source>> {
    vec![Box::new(pi::Pi::new(root.to_path_buf()))]
}

async fn start() -> Harness {
    let sessions = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let (topics, _) = tokio::sync::broadcast::channel(64);
    let ingest = Ingest::new(store.clone(), pi_source(sessions.path()), topics.clone());

    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        // No pi is spawned by these tests; a name that does not resolve is
        // the honest placeholder, and any test that did spawn would fail
        // loudly rather than silently reaching a real pi.
        "skiff-tests-have-no-pi".into(),
        sessions.path().to_path_buf(),
        true,
        MuseConfig {
            binary: sessions.path().join("missing-muse"),
            session_dir: sessions.path().join("muse/sessions"),
            session_dir_explicit: true,
        },
        "http://127.0.0.1:1",
    );

    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(dist.path().join("service-worker.js"), "// worker").unwrap();
    std::fs::write(dist.path().join("manifest.webmanifest"), "{}").unwrap();
    let app = router(AppState::new(store, runs, topics), dist.path().to_path_buf());

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // `dist` is moved in so the served directory outlives the server.
        let _dist = dist;
        axum::serve(listener, app).await.unwrap();
    });

    Harness { addr, sessions, ingest }
}

impl Harness {
    fn write_session(&self, name: &str, body: &str) {
        std::fs::write(self.sessions.path().join(name), body).unwrap();
    }
}

type Socket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn connect(addr: SocketAddr) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
    socket
}

async fn send(socket: &mut Socket, frame: ClientFrame) {
    socket.send(Message::text(serde_json::to_string(&frame).unwrap())).await.unwrap();
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

/// The next snapshot, skipping the greeting.
async fn next_sessions(socket: &mut Socket) -> (u32, Vec<String>) {
    loop {
        match next_frame(socket).await {
            ServerFrame::Hello { .. } => continue,
            ServerFrame::Snapshot { sub, data: ViewData::Sessions(view), .. } => {
                return (sub, view.sessions.iter().map(|s| s.id.to_string()).collect());
            }
            other => panic!("expected a sessions snapshot, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_subscription_answers_with_a_snapshot_and_then_stays_current() {
    let harness = start().await;
    harness.write_session("first.jsonl", &format!("{HEADER}\n"));
    harness.ingest.scan().unwrap();

    let mut socket = connect(harness.addr).await;
    send(&mut socket, ClientFrame::Subscribe { sub: 1, view: ViewSpec::Sessions }).await;

    let (sub, ids) = next_sessions(&mut socket).await;
    assert_eq!(sub, 1);
    assert_eq!(ids, ["pi:first"]);

    // A file appearing on disk must push, with nothing asked of the client.
    harness.write_session("second.jsonl", &format!("{HEADER}\n"));
    harness.ingest.scan().unwrap();

    let (_, ids) = next_sessions(&mut socket).await;
    assert_eq!(ids.len(), 2, "the new session arrived unbidden");
    assert!(ids.contains(&"pi:second".to_owned()));
}

#[tokio::test]
async fn the_greeting_names_the_read_model_the_client_is_talking_to() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    match next_frame(&mut socket).await {
        ServerFrame::Hello { read_model } => {
            assert_eq!(read_model, skiff::store::SCHEMA_VERSION);
        }
        other => panic!("the first frame must be the greeting, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unsubscribed_view_stops_arriving() {
    let harness = start().await;
    harness.write_session("first.jsonl", &format!("{HEADER}\n"));
    harness.ingest.scan().unwrap();

    let mut socket = connect(harness.addr).await;
    send(&mut socket, ClientFrame::Subscribe { sub: 1, view: ViewSpec::Sessions }).await;
    next_sessions(&mut socket).await;

    send(&mut socket, ClientFrame::Unsubscribe { sub: 1 }).await;
    // Give the unsubscribe time to land before the change that would have
    // pushed; otherwise this test would pass for the wrong reason.
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.write_session("second.jsonl", &format!("{HEADER}\n"));
    harness.ingest.scan().unwrap();

    let quiet = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
    assert!(quiet.is_err(), "a closed subscription must not keep pushing");
}

#[tokio::test]
async fn a_malformed_frame_is_answered_rather_than_dropping_the_connection() {
    let harness = start().await;
    let mut socket = connect(harness.addr).await;
    assert!(matches!(next_frame(&mut socket).await, ServerFrame::Hello { .. }));

    socket.send(Message::text("{\"t\":\"nonsense\"}")).await.unwrap();
    match next_frame(&mut socket).await {
        ServerFrame::Error { sub, error, .. } => {
            assert_eq!(sub, None, "a bad frame belongs to no subscription");
            assert!(error.contains("bad frame"), "got: {error}");
        }
        other => panic!("expected an error frame, got {other:?}"),
    }

    // And the connection still works.
    send(&mut socket, ClientFrame::Subscribe { sub: 9, view: ViewSpec::Sessions }).await;
    assert_eq!(next_sessions(&mut socket).await.0, 9);
}

#[tokio::test]
async fn the_client_bundle_is_served_with_a_fallback_for_client_routes() {
    let harness = start().await;
    let base = format!("http://{}", harness.addr);
    let client = reqwest_lite::Client;

    assert_eq!(client.get(&format!("{base}/healthz")).await, "ok");
    // An unknown path is a client route, not a 404: the client owns routing.
    assert!(client.get(&format!("{base}/s/pi:abc")).await.contains("<!doctype html>"));
}

#[tokio::test]
async fn mutable_pwa_metadata_is_never_served_as_an_immutable_asset() {
    let harness = start().await;
    let base = format!("http://{}", harness.addr);
    let client = reqwest_lite::Client;

    let worker = client.raw(&format!("{base}/service-worker.js")).await;
    assert!(worker.contains("cache-control: no-cache"));
    assert!(worker.contains("content-type: text/javascript; charset=utf-8"));
    assert!(worker.ends_with("// worker"));

    let manifest = client.raw(&format!("{base}/manifest.webmanifest")).await;
    assert!(manifest.contains("cache-control: no-cache"));
    assert!(manifest.contains("content-type: application/manifest+json"));
    assert!(manifest.ends_with("{}"));
}

/// A three-line HTTP GET, so the test suite does not pull in an HTTP client
/// for two assertions.
mod reqwest_lite {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub struct Client;

    impl Client {
        pub async fn raw(&self, url: &str) -> String {
            let rest = url.strip_prefix("http://").expect("an http url");
            let (host, path) = rest.split_once('/').expect("a path");
            let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
            let request =
                format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }

        pub async fn get(&self, url: &str) -> String {
            self.raw(url)
                .await
                .split_once("\r\n\r\n")
                .expect("a response body")
                .1
                .to_owned()
        }
    }
}
