//! The HTTP surface: one WebSocket, and the client bundle.
//!
//! There are no REST read endpoints, by design (DW-004 §6) — `/healthz` is a
//! liveness probe, not a read path.
//!
//! One task owns one connection: the socket, its subscriptions, and its topic
//! receiver. Nothing is shared between connections, so there is no lock to
//! contend and a slow client can only ever starve itself.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{State, WebSocketUpgrade};
use axum::extract::ws::{Message, WebSocket};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

use crate::ingest::Topic;
use crate::run::{LiveState, Runs};
use crate::store::{SCHEMA_VERSION, Store};
use crate::views::{Update, ViewData, ViewSpec};
use crate::wire::{ClientFrame, Command, ServerFrame, SubId};

#[derive(Clone)]
pub struct AppState {
    store: Store,
    runs: Arc<Runs>,
    topics: broadcast::Sender<Topic>,
}

impl AppState {
    pub fn new(store: Store, runs: Arc<Runs>, topics: broadcast::Sender<Topic>) -> Self {
        Self { store, runs, topics }
    }
}

/// Build the router. `web_dist` is the built client; a request that matches no
/// file falls back to `index.html`, because the client owns routing.
pub fn router(state: AppState, web_dist: PathBuf) -> Router {
    let index = web_dist.join("index.html");
    let files = ServeDir::new(&web_dist).fallback(ServeFile::new(index));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .with_state(Arc::new(state))
        .fallback_service(files)
}

pub async fn serve(addr: SocketAddr, router: Router) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "skiffd listening");
    axum::serve(listener, router).await.context("serving")?;
    Ok(())
}

async fn ws_upgrade(
    State(state): State<Arc<AppState>>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| connection(socket, state))
}

/// One client connection, for its lifetime.
async fn connection(mut socket: WebSocket, state: Arc<AppState>) {
    let mut topics = state.topics.subscribe();
    let mut subs: HashMap<SubId, Subscription> = HashMap::new();

    if send(&mut socket, &ServerFrame::Hello { read_model: SCHEMA_VERSION }).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                if !handle_message(&mut socket, &state, &mut subs, message).await {
                    break;
                }
            }
            topic = topics.recv() => {
                match topic {
                    Ok(topic) => {
                        if !refresh(&mut socket, &state, &mut subs, &topic).await {
                            break;
                        }
                    }
                    // The connection fell behind the ingest. Every view is
                    // snapshot-based and every subscription is about to be
                    // recomputed from the store, so a missed topic costs
                    // nothing but a redundant recompute.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "client lagged the ingest; resnapshotting");
                        if !refresh_all(&mut socket, &state, &mut subs).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

struct Subscription {
    view: ViewSpec,
    seq: u64,
}

/// Handle one client frame. Answers whether the connection should continue.
async fn handle_message(
    socket: &mut WebSocket,
    state: &AppState,
    subs: &mut HashMap<SubId, Subscription>,
    message: Message,
) -> bool {
    let text = match message {
        Message::Text(text) => text,
        Message::Close(_) => return false,
        // Ping/Pong are answered by axum; a binary frame is not part of this
        // protocol and is ignored rather than treated as a protocol violation.
        _ => return true,
    };

    let frame = match serde_json::from_str::<ClientFrame>(&text) {
        Ok(frame) => frame,
        Err(err) => {
            let error =
                ServerFrame::Error { sub: None, req: None, error: format!("bad frame: {err}") };
            return send(socket, &error).await.is_ok();
        }
    };

    match frame {
        ClientFrame::Subscribe { sub, view } => {
            subs.insert(sub, Subscription { view: view.clone(), seq: 0 });
            snapshot(socket, state, subs, sub).await
        }
        ClientFrame::Unsubscribe { sub } => {
            subs.remove(&sub);
            true
        }
        ClientFrame::Command { req, cmd } => {
            let frame = match run_command(state, cmd).await {
                Ok(()) => ServerFrame::Ack { req },
                // The error carries pi's own reason. It never echoes the
                // prompt text, which is the user's and has no business in a
                // log line or an error banner.
                Err(err) => {
                    ServerFrame::Error { sub: None, req: Some(req), error: format!("{err:#}") }
                }
            };
            send(socket, &frame).await.is_ok()
        }
    }
}

async fn run_command(state: &AppState, cmd: Command) -> anyhow::Result<()> {
    match cmd {
        Command::Send { session, text, client_id } => {
            state.runs.send(&session, &text, &client_id).await
        }
        Command::Abort { session } => state.runs.abort(&session).await,
    }
}

/// Re-snapshot every subscription the topic affects.
async fn refresh(
    socket: &mut WebSocket,
    state: &AppState,
    subs: &mut HashMap<SubId, Subscription>,
    topic: &Topic,
) -> bool {
    let affected: Vec<(SubId, Update)> = subs
        .iter()
        .filter_map(|(id, sub)| sub.view.update_for(topic).map(|update| (*id, update)))
        .collect();
    for (id, update) in affected {
        let sent = match update {
            Update::Snapshot => snapshot(socket, state, subs, id).await,
            Update::Live => live(socket, state, subs, id).await,
        };
        if !sent {
            return false;
        }
    }
    true
}

/// Send one subscription's live state. Deliberately cheap: no SQLite, no
/// transcript.
async fn live(
    socket: &mut WebSocket,
    state: &AppState,
    subs: &mut HashMap<SubId, Subscription>,
    id: SubId,
) -> bool {
    let Some(sub) = subs.get(&id) else { return true };
    let Some(session) = sub.view.session().cloned() else { return true };
    let live = state.runs.live(&session).await;

    // Re-fetch after the await: an unsubscribe may have landed.
    let Some(sub) = subs.get_mut(&id) else { return true };
    sub.seq += 1;
    let frame = ServerFrame::Live { sub: id, seq: sub.seq, live };
    send(socket, &frame).await.is_ok()
}

async fn refresh_all(
    socket: &mut WebSocket,
    state: &AppState,
    subs: &mut HashMap<SubId, Subscription>,
) -> bool {
    for id in subs.keys().copied().collect::<Vec<_>>() {
        if !snapshot(socket, state, subs, id).await {
            return false;
        }
    }
    true
}

/// Compute and send one subscription's snapshot.
async fn snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    subs: &mut HashMap<SubId, Subscription>,
    id: SubId,
) -> bool {
    let Some(sub) = subs.get(&id) else { return true };
    let view = sub.view.clone();

    // The live state comes from the run registry, which is async; the view
    // computation is blocking. Fetching here keeps the two apart.
    let live = match view.session() {
        Some(session) => state.runs.live(session).await,
        None => LiveState::default(),
    };

    match compute(state.store.clone(), view, live).await {
        Ok(data) => {
            // Re-fetch: the await above yielded, and an unsubscribe may have
            // landed in the meantime. Sending a snapshot for a closed
            // subscription would leave the client holding a pane it closed.
            let Some(sub) = subs.get_mut(&id) else { return true };
            sub.seq += 1;
            let frame = ServerFrame::Snapshot { sub: id, seq: sub.seq, data };
            send(socket, &frame).await.is_ok()
        }
        Err(err) => {
            let frame =
                ServerFrame::Error { sub: Some(id), req: None, error: format!("{err:#}") };
            send(socket, &frame).await.is_ok()
        }
    }
}

/// Views read SQLite, which is blocking, so they never run on the runtime's
/// worker threads.
async fn compute(store: Store, view: ViewSpec, live: LiveState) -> Result<ViewData> {
    tokio::task::spawn_blocking(move || view.compute(&store, live))
        .await
        .context("the view task panicked")?
}

async fn send(socket: &mut WebSocket, frame: &ServerFrame) -> Result<()> {
    let text = serde_json::to_string(frame).context("serialising a server frame")?;
    socket.send(Message::Text(text.into())).await.context("writing to the socket")?;
    Ok(())
}
