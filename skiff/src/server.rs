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
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinSet;
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
    changes: change::ChangeService,
    landing: change::LandingService,
    landings: Arc<Mutex<JoinSet<()>>>,
}

impl AppState {
    pub fn new(store: Store, runs: Arc<Runs>, topics: broadcast::Sender<Topic>) -> Self {
        let changes = change::ChangeService::new(
            change::Store::new(change::default_change_dir().expect("HOME resolves change state")),
            change::default_repos_dir().expect("HOME resolves repositories"),
            change::Jj::new(
                std::env::var_os("JJ_BINARY")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "jj".into()),
            ),
        );
        Self::with_changes(
            store,
            runs,
            topics,
            changes,
            change::LandingConfig::from_env(),
        )
        .expect("default landing configuration is valid")
    }

    pub fn with_changes(
        store: Store,
        runs: Arc<Runs>,
        topics: broadcast::Sender<Topic>,
        changes: change::ChangeService,
        landing_config: change::LandingConfig,
    ) -> change::Result<Self> {
        let landing = change::LandingService::new(changes.clone(), landing_config)?;
        Ok(Self {
            store,
            runs,
            topics,
            changes,
            landing,
            landings: Arc::new(Mutex::new(JoinSet::new())),
        })
    }

    pub async fn shutdown(&self) {
        let mut landings = self.landings.lock().await;
        while let Some(result) = landings.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "landing task failed during shutdown");
            }
        }
    }

    async fn spawn_landing(&self, repo: String, card: u64) {
        let landing = self.landing.clone();
        let topics = self.topics.clone();
        let mut landings = self.landings.lock().await;
        while landings.try_join_next().is_some() {}
        landings.spawn(async move {
            if let Err(error) = landing.land(&repo, card).await {
                tracing::error!(repo, card, %error, "landing failed");
            }
            let _ = topics.send(Topic::Change { repo, card });
            let _ = topics.send(Topic::ChangeList);
        });
    }
}

/// Build the router. `web_dist` is the built client; a request that matches no
/// file falls back to `index.html`, because the client owns routing.
pub fn router(state: AppState, web_dist: PathBuf) -> Router {
    let index = web_dist.join("index.html");
    let worker = web_dist.join("service-worker.js");
    let manifest = web_dist.join("manifest.webmanifest");
    let files = ServeDir::new(&web_dist).fallback(ServeFile::new(index));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        // These two files are mutable release pointers, unlike Vite's hashed
        // assets. Explicit no-cache keeps worker updates and install metadata
        // from waiting on an intermediary's freshness guess.
        .route(
            "/service-worker.js",
            get(move || no_cache_file(worker.clone(), "text/javascript; charset=utf-8")),
        )
        .route(
            "/manifest.webmanifest",
            get(move || no_cache_file(manifest.clone(), "application/manifest+json")),
        )
        .with_state(Arc::new(state))
        .fallback_service(files)
}

async fn no_cache_file(
    path: PathBuf,
    content_type: &'static str,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let body = tokio::fs::read(path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_TYPE, content_type),
        ],
        body,
    ))
}

pub async fn serve(addr: SocketAddr, router: Router, state: AppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "skiffd listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Drain before asking axum's long-lived WebSockets to close. A
            // browser may keep a socket open indefinitely; it must never keep
            // an already-running push from reaching its durable outcome.
            state.shutdown().await;
        })
        .await
        .context("serving")?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("installing SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
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

    if send(
        &mut socket,
        &ServerFrame::Hello {
            read_model: SCHEMA_VERSION,
        },
    )
    .await
    .is_err()
    {
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
            let error = ServerFrame::Error {
                sub: None,
                req: None,
                error: format!("bad frame: {err}"),
            };
            return send(socket, &error).await.is_ok();
        }
    };

    match frame {
        ClientFrame::Subscribe { sub, view } => {
            subs.insert(
                sub,
                Subscription {
                    view: view.clone(),
                    seq: 0,
                },
            );
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
                    // Logged as well as answered: a command that fails for an
                    // environmental reason (a harness that is not installed, a
                    // session file that vanished) leaves no other trace, and
                    // "it just says no" is not something anyone can debug.
                    tracing::warn!(error = format!("{err:#}"), "command failed");
                    ServerFrame::Error {
                        sub: None,
                        req: Some(req),
                        error: format!("{err:#}"),
                    }
                }
            };
            send(socket, &frame).await.is_ok()
        }
    }
}

async fn run_command(state: &AppState, cmd: Command) -> anyhow::Result<()> {
    match cmd {
        Command::AnnotateChange {
            repo,
            card,
            round,
            path,
            line,
            side,
            text,
        } => {
            let changes = state.changes.clone();
            let repo_for_task = repo.clone();
            tokio::task::spawn_blocking(move || {
                changes.add_annotation(&repo_for_task, card, round, &path, line, side, &text)
            })
            .await??;
            let _ = state.topics.send(Topic::Change { repo, card });
            Ok(())
        }
        Command::RequestChanges { repo, card, note } => {
            let change = {
                let changes = state.changes.clone();
                let repo = repo.clone();
                tokio::task::spawn_blocking(move || changes.store().require(&repo, card)).await??
            };
            if change.state != change::ChangeState::InReview {
                anyhow::bail!(
                    "change {repo}/{card} is {}; only in_review changes take requests",
                    change.state
                );
            }
            let session = change
                .session
                .ok_or_else(|| anyhow::anyhow!("change {repo}/{card} has no bound session"))?
                .parse()
                .map_err(|()| {
                    anyhow::anyhow!("change {repo}/{card} has an invalid session binding")
                })?;
            let client_id = format!("review:{repo}:{card}:{}", change.updated_at);
            state.runs.send(&session, &note, &client_id).await?;
            let changes = state.changes.clone();
            let repo_for_task = repo.clone();
            tokio::task::spawn_blocking(move || {
                changes.store().request_changes(&repo_for_task, card, &note)
            })
            .await??;
            let _ = state.topics.send(Topic::Change { repo, card });
            let _ = state.topics.send(Topic::ChangeList);
            Ok(())
        }
        Command::ApproveChange { repo, card } => {
            let landing = state.landing.clone();
            let repo_for_task = repo.clone();
            tokio::task::spawn_blocking(move || landing.begin(&repo_for_task, card)).await??;
            let _ = state.topics.send(Topic::Change {
                repo: repo.clone(),
                card,
            });
            let _ = state.topics.send(Topic::ChangeList);
            state.spawn_landing(repo, card).await;
            Ok(())
        }
        Command::Send {
            session,
            text,
            client_id,
        } => state.runs.send(&session, &text, &client_id).await,
        Command::Abort { session } => state.runs.abort(&session).await,
        Command::Rename { session, name } => state.runs.rename(&session, &name).await,
        Command::SetModel {
            session,
            provider,
            model_id,
        } => state.runs.set_model(&session, &provider, &model_id).await,
        Command::SetOrchestrator { session, active } => {
            state.runs.set_orchestrator(&session, active).await
        }
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
    let Some(sub) = subs.get(&id) else {
        return true;
    };
    let Some(session) = sub.view.session().cloned() else {
        return true;
    };
    let live = state.runs.live(&session).await;

    // Re-fetch after the await: an unsubscribe may have landed.
    let Some(sub) = subs.get_mut(&id) else {
        return true;
    };
    sub.seq += 1;
    let frame = ServerFrame::Live {
        sub: id,
        seq: sub.seq,
        live,
    };
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
    let Some(sub) = subs.get(&id) else {
        return true;
    };
    let view = sub.view.clone();

    // The live state comes from the run registry, which is async; the view
    // computation is blocking. Fetching here keeps the two apart.
    let (live, models) = match view.session() {
        Some(session) => {
            let (live, models) =
                tokio::join!(state.runs.live(session), state.runs.model_catalog(session));
            (live, models)
        }
        None => (LiveState::default(), crate::model::ModelCatalog::default()),
    };

    let will_deploy = if matches!(view, ViewSpec::Change { .. }) {
        match state.landing.tugboat() {
            Some(tugboat) => tugboat
                .service_count()
                .await
                .ok()
                .and_then(|count| u32::try_from(count).ok()),
            None => None,
        }
    } else {
        None
    };

    match compute(
        state.store.clone(),
        state.changes.clone(),
        view,
        live,
        models,
        will_deploy,
    )
    .await
    {
        Ok(data) => {
            // Re-fetch: the await above yielded, and an unsubscribe may have
            // landed in the meantime. Sending a snapshot for a closed
            // subscription would leave the client holding a pane it closed.
            let Some(sub) = subs.get_mut(&id) else {
                return true;
            };
            sub.seq += 1;
            let frame = ServerFrame::Snapshot {
                sub: id,
                seq: sub.seq,
                data,
            };
            send(socket, &frame).await.is_ok()
        }
        Err(err) => {
            let frame = ServerFrame::Error {
                sub: Some(id),
                req: None,
                error: format!("{err:#}"),
            };
            send(socket, &frame).await.is_ok()
        }
    }
}

/// Views read SQLite, which is blocking, so they never run on the runtime's
/// worker threads.
async fn compute(
    store: Store,
    changes: change::ChangeService,
    view: ViewSpec,
    live: LiveState,
    models: crate::model::ModelCatalog,
    will_deploy: Option<u32>,
) -> Result<ViewData> {
    tokio::task::spawn_blocking(move || {
        let mut data = view.compute(&store, &changes, live, models)?;
        if let ViewData::Change(change) = &mut data {
            change.will_deploy = will_deploy;
        }
        Ok(data)
    })
    .await
    .context("the view task panicked")?
}

async fn send(socket: &mut WebSocket, frame: &ServerFrame) -> Result<()> {
    let text = serde_json::to_string(frame).context("serialising a server frame")?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("writing to the socket")?;
    Ok(())
}
