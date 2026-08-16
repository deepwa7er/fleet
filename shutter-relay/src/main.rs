//! shutter-relay — video-only live relay.
//!
//! Ingest:  Mac app opens `WS /ingest?id=<id>&token=...` (or `Authorization: Bearer ...`),
//!          sends binary fMP4 fragments: first message is the init segment (`ftyp`+`moov`),
//!          subsequent messages are `moof`+`mdat` fragments (video-only, `avc1.42E01E`).
//! Viewer:  Browser opens `WS /watch/:id/stream`, relay sends init then ring then live tail.
//! Page:    `GET /watch/:id` serves the viewer SPA.
//!
//! Single binary, native Linux build (`cargo build --release -p shutter-relay`).
//! State is in-memory: `init + ring (last ~10 s)` per stream, `broadcast` fan-out.

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use bytes::Bytes;
use serde::Deserialize;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

const RING_CAP: usize = 300;
const BROADCAST_CAP: usize = 512;
const DEFAULT_PORT: u16 = 8125;

#[derive(Clone)]
struct Stream {
    init: Option<Bytes>,
    ring: VecDeque<Bytes>,
    tx: broadcast::Sender<Bytes>,
    viewers: Arc<AtomicUsize>,
    created_at: Instant,
}

impl Stream {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        Self {
            init: None,
            ring: VecDeque::with_capacity(RING_CAP),
            tx,
            viewers: Arc::new(AtomicUsize::new(0)),
            created_at: Instant::now(),
        }
    }
}

type Streams = Arc<RwLock<HashMap<String, Stream>>>;

#[derive(Clone)]
struct AppState {
    streams: Streams,
    ingest_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct IngestQuery {
    id: Option<String>,
    token: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct WatchQuery {
    // no query for viewer stream currently, but keep for future
}

fn check_ingest_auth(headers: &HeaderMap, query_token: Option<&str>, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if expected.is_empty() {
        return true;
    }
    // Query token wins if provided, else Authorization header.
    if let Some(q) = query_token {
        if q == expected {
            return true;
        }
    }
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let auth = auth.trim();
        if let Some(bearer) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            if bearer.trim() == expected {
                return true;
            }
        } else if auth == expected {
            return true;
        }
        // Also allow `shutter_<token>` prefix stripping for convenience.
        let normalized = bearer_or_raw(auth);
        if normalized == expected {
            return true;
        }
    }
    false
}

fn bearer_or_raw(v: &str) -> &str {
    if let Some(b) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
        b.trim()
    } else {
        v.trim()
    }
}

fn normalize_token(raw: &str) -> String {
    let t = raw.trim();
    if let Some(stripped) = t.strip_prefix("shutter_") {
        stripped.to_string()
    } else {
        t.to_string()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shutter_relay=info,tower_http=info".into()),
        )
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let bind: String = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
    let ingest_token = std::env::var("SHUTTER_INGEST_TOKEN")
        .ok()
        .map(|s| normalize_token(&s))
        .filter(|s| !s.is_empty());

    if ingest_token.is_some() {
        info!("ingest auth enabled (SHUTTER_INGEST_TOKEN set)");
    } else {
        warn!("ingest auth disabled — set SHUTTER_INGEST_TOKEN to require a token");
    }

    let state = AppState {
        streams: Arc::new(RwLock::new(HashMap::new())),
        ingest_token,
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/ingest", get(ingest_ws))
        .route("/watch/{id}", get(watch_page))
        .route("/watch/{id}/stream", get(viewer_ws))
        .route("/watch/{id}/meta", get(watch_meta))
        .route("/", get(index_redirect))
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    info!("shutter-relay listening on {addr} (healthz on /healthz)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn index_redirect() -> impl IntoResponse {
    // No stream id at `/` — redirect to a placeholder or show landing.
    Html(landing_html())
}

async fn watch_meta(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let streams = state.streams.read().await;
    if let Some(s) = streams.get(&id) {
        let viewers = s.viewers.load(Ordering::Relaxed);
        let age_secs = s.created_at.elapsed().as_secs();
        let live = true; // if entry exists, we consider it live (ingest may have dropped but ring remains briefly)
        let body = serde_json::json!({
            "id": id,
            "live": live,
            "viewers": viewers,
            "age_secs": age_secs,
            "has_init": s.init.is_some(),
            "ring_len": s.ring.len(),
        });
        (StatusCode::OK, axum::Json(body)).into_response()
    } else {
        let body = serde_json::json!({"id": id, "live": false, "viewers": 0});
        (StatusCode::NOT_FOUND, axum::Json(body)).into_response()
    }
}

async fn watch_page(Path(id): Path<String>) -> impl IntoResponse {
    let html = viewer_html(&id);
    Html(html)
}

async fn ingest_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IngestQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // Auth
    if !check_ingest_auth(&headers, q.token.as_deref(), state.ingest_token.as_deref()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized: bad token\n").into_response();
    }
    let id = q
        .id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());
    // Basic id sanitization: allow alphanum + - _
    let id = sanitize_id(&id);
    info!(%id, "ingest ws upgrade");
    ws.on_upgrade(move |socket| ingest_loop(socket, state, id))
}

fn sanitize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch == ' ' {
            out.push('-');
        }
    }
    if out.is_empty() {
        uuid::Uuid::new_v4().to_string()[..8].to_string()
    } else {
        // limit length
        if out.len() > 32 {
            out.truncate(32);
        }
        out
    }
}

async fn ingest_loop(mut socket: WebSocket, state: AppState, id: String) {
    // Ensure a Stream entry exists and get its sender. New ingest replaces old state
    // (drops old broadcast, viewers will see close and may reconnect).
    let tx = {
        let mut streams = state.streams.write().await;
        // If an existing stream for this id exists, we replace it — old viewers
        // on that id will have to reconnect (their rx will get Lagged/Closed).
        let stream = streams.entry(id.clone()).or_insert_with(Stream::new);
        // If this is a reconnection, reset init/ring but keep the same broadcast channel
        // so existing viewers stay subscribed? For clean semantics we create a fresh channel.
        // Drop and recreate to ensure old fragments don't leak.
        if stream.init.is_some() || !stream.ring.is_empty() {
            *stream = Stream::new();
        }
        stream.tx.clone()
    };
    info!(%id, "ingest connected");

    let mut is_first = true;
    let mut bytes_ingested: usize = 0;
    let mut fragments: usize = 0;

    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!(%id, error=%e, "ingest recv error");
                break;
            }
        };
        match msg {
            Message::Binary(data) => {
                if data.is_empty() {
                    continue;
                }
                let data = Bytes::from(data.to_vec());
                bytes_ingested += data.len();
                fragments += 1;
                // First binary message is the init segment; store it.
                {
                    let mut streams = state.streams.write().await;
                    if let Some(s) = streams.get_mut(&id) {
                        if is_first {
                            s.init = Some(data.clone());
                            is_first = false;
                            info!(%id, bytes=data.len(), "ingest init segment");
                        } else {
                            if s.ring.len() >= RING_CAP {
                                s.ring.pop_front();
                            }
                            s.ring.push_back(data.clone());
                        }
                    }
                }
                // Broadcast to viewers (lagging viewers will get Lagged and should re-init)
                let _ = tx.send(data);
            }
            Message::Text(txt) => {
                // Control messages: allow {"type":"end"} to close gracefully, or hello.
                // For v1 we just log and ignore; a text "end" will break the loop.
                let txt = txt.to_string();
                if txt.contains("\"type\"") && txt.contains("end") {
                    info!(%id, "ingest sent end");
                    break;
                }
                // Hello message with id is okay — no action needed.
                tracing::debug!(%id, txt=%txt, "ingest text message");
            }
            Message::Ping(payload) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
        }
    }

    info!(%id, fragments, bytes_ingested, "ingest disconnected");
    // Keep the stream entry for a grace period so late viewers can still get init+ring
    // as "stream ended" rather than 404. We keep it for now; a cleanup task could
    // prune after e.g. 5 minutes. For v1 we keep it until replaced or server restart.
}

async fn viewer_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let id = sanitize_id(&id);
    // Check if stream exists — if not, still upgrade but send a close with reason.
    let exists = { state.streams.read().await.contains_key(&id) };
    if !exists {
        // Still upgrade; the loop will send a "not found" text and close.
        warn!(%id, "viewer requested unknown stream");
    }
    ws.on_upgrade(move |socket| viewer_loop(socket, state, id))
}

async fn viewer_loop(mut socket: WebSocket, state: AppState, id: String) {
    let (init, ring, mut rx) = {
        let streams = state.streams.read().await;
        if let Some(s) = streams.get(&id) {
            s.viewers.fetch_add(1, Ordering::Relaxed);
            (s.init.clone(), s.ring.clone(), s.tx.subscribe())
        } else {
            // No stream — send error and return (socket drop closes).
            let _ = socket
                .send(Message::Text(format!("{{\"error\":\"no stream {id}\"}}").into()))
                .await;
            return;
        }
    };
    info!(%id, viewers=%(0), "viewer connected");

    // Send init first.
    if let Some(init_bytes) = init {
        if socket.send(Message::Binary(init_bytes)).await.is_err() {
            cleanup_viewer(&state, &id).await;
            return;
        }
    }
    // Then ring (last GOP / last ~10 s)
    for frag in ring {
        if socket.send(Message::Binary(frag)).await.is_err() {
            cleanup_viewer(&state, &id).await;
            return;
        }
    }

    // Now fan-out live tail.
    loop {
        tokio::select! {
            // Forward broadcasted fragments
            res = rx.recv() => {
                match res {
                    Ok(data) => {
                        if socket.send(Message::Binary(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(%id, skipped, "viewer lagged, re-sending init");
                        // Re-send init so decoder can resync
                        let init_again = {
                            let streams = state.streams.read().await;
                            streams.get(&id).and_then(|s| s.init.clone())
                        };
                        if let Some(init_bytes) = init_again {
                            let _ = socket.send(Message::Binary(init_bytes)).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        let _ = socket.send(Message::Text("{\"type\":\"ended\"}".into())).await;
                        break;
                    }
                }
            }
            // Handle viewer pings / closes
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Ping(p))) => {
                        let _ = socket.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => {
                        // Viewers are read-only in v1; ignore.
                    }
                    Some(Err(_)) => break,
                }
            }
        }
    }

    cleanup_viewer(&state, &id).await;
    info!(%id, "viewer disconnected");
}

async fn cleanup_viewer(state: &AppState, id: &str) {
    let streams = state.streams.read().await;
    if let Some(s) = streams.get(id) {
        s.viewers.fetch_sub(1, Ordering::Relaxed);
    }
}

fn landing_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Shutter Live</title>
<style>
  :root{--bg:#f7f2e9;--ink:#1a1a1a;--blue:#1e3a5f;--fill:#fffdf8}
  *{box-sizing:border-box} body{margin:0;font-family:ui-sans-serif,system-ui,sans-serif;background:var(--bg);color:var(--ink);display:grid;place-items:center;min-height:100vh;padding:2rem}
  .card{background:var(--fill);border:1px solid rgba(0,0,0,.08);padding:2.5rem;max-width:560px;width:100%;box-shadow:4px 4px 0 rgba(0,0,0,.08)}
  h1{margin:0 0 .5rem;font-size:1.4rem;letter-spacing:-.02em} p{margin:0 0 1rem;line-height:1.5;color:rgba(0,0,0,.65)}
  code{background:rgba(0,0,0,.06);padding:.15rem .35rem;font-size:.85em}
</style>
</head>
<body>
<div class="card">
  <h1>Shutter Live</h1>
  <p>Press <code>⇧⌘9</code> in Shutter on your Mac, drag a region, and share the link that appears. This page is the viewer.</p>
  <p>Open <code>/watch/&lt;id&gt;</code> to watch a stream, e.g. <code>/watch/demo</code>.</p>
</div>
</body>
</html>"#
        .to_string()
}

fn viewer_html(id: &str) -> String {
    // Minimal DW-001 styling, video-only MSE. `id` is HTML-escaped via serde_json string.
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "\"unknown\"".to_string());
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Live — Shutter</title>
<style>
  :root {{
    --bg:#f7f2e9; --fill:#fffdf8; --ink:#1a1a1a; --muted:rgba(0,0,0,.55);
    --line:rgba(0,0,0,.08); --blue:#1e3a5f; --red:#b42318;
  }}
  *{{box-sizing:border-box}} html,body{{height:100%}} body{{margin:0;background:var(--bg);color:var(--ink);font-family:ui-sans-serif,system-ui,sans-serif}}
  header{{display:flex;align-items:center;justify-content:space-between;padding:1rem 1.25rem;border-bottom:1px solid var(--line);background:rgba(247,242,233,.9);backdrop-filter:saturate(1.1) blur(6px);position:sticky;top:0}}
  .brand{{font-weight:700;letter-spacing:-.02em}}
  .meta{{display:flex;gap:1rem;align-items:center;font-variant-numeric:tabular-nums;font-size:.85rem}}
  .live{{display:inline-flex;align-items:center;gap:.4rem;background:var(--blue);color:white;padding:.25rem .5rem;font-size:.7rem;letter-spacing:.08em;text-transform:uppercase}}
  .live::before{{content:"";width:.45rem;height:.45rem;background:white;border-radius:50%;box-shadow:0 0 0 6px rgba(255,255,255,.18)}}
  .live.off{{background:var(--muted)}} .live.off::before{{background:rgba(255,255,255,.5)}}
  main{{max-width:1100px;margin:0 auto;padding:1.25rem}}
  .stage{{background:var(--fill);border:1px solid var(--line);box-shadow:4px 4px 0 rgba(0,0,0,.07);overflow:hidden}}
  video{{width:100%;height:auto;display:block;background:#111;aspect-ratio:16/9}}
  .bar{{display:flex;justify-content:space-between;align-items:center;padding:.75rem 1rem;border-top:1px solid var(--line);font-size:.8rem;color:var(--muted)}}
  .bar code{{background:rgba(0,0,0,.06);padding:.15rem .35rem}}
  a{{color:var(--blue);text-decoration:none}} a:hover{{text-decoration:underline}}
</style>
</head>
<body>
<header>
  <div class="brand">Shutter Live</div>
  <div class="meta">
    <span id="pill" class="live off">waiting</span>
    <span id="viewers">— viewers</span>
    <span id="id" title="Stream ID"><code>{id_esc}</code></span>
  </div>
</header>
<main>
  <div class="stage">
    <video id="v" autoplay muted playsinline controls></video>
    <div class="bar">
      <span id="status">Connecting…</span>
      <span>Share: <code id="share"></code> <a href='#' id="copy">Copy</a></span>
    </div>
  </div>
</main>
<script>
const STREAM_ID = {id_json};
const WS_URL = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/watch/' + encodeURIComponent(STREAM_ID) + '/stream';
const META_URL = '/watch/' + encodeURIComponent(STREAM_ID) + '/meta';
const video = document.getElementById('v');
const pill = document.getElementById('pill');
const viewersEl = document.getElementById('viewers');
const statusEl = document.getElementById('status');
const shareEl = document.getElementById('share');
const copyEl = document.getElementById('copy');
shareEl.textContent = location.origin + '/watch/' + STREAM_ID;
copyEl.addEventListener('click', (e) => {{ e.preventDefault(); navigator.clipboard.writeText(shareEl.textContent); copyEl.textContent='Copied'; setTimeout(()=>copyEl.textContent='Copy',1500); }});

let ws, mediaSource, sourceBuffer, queue = [], appending = false;
let hasInit = false;

function setPill(live) {{
  pill.textContent = live ? 'live' : 'ended';
  pill.classList.toggle('off', !live);
}}

function fetchMeta() {{
  fetch(META_URL).then(r=>r.json()).then(j=>{{
    viewersEl.textContent = (j.viewers ?? 0) + ' viewers';
    if (j.live === false) setPill(false);
  }}).catch(()=>{{}});
}}
setInterval(fetchMeta, 3000); fetchMeta();

function isSupported() {{
  return window.MediaSource && MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"');
}}

if (!isSupported()) {{
  statusEl.textContent = 'MSE not supported — try Chrome/Edge/Firefox, or Safari Technology Preview.';
}} else {{
  mediaSource = new MediaSource();
  video.src = URL.createObjectURL(mediaSource);
  mediaSource.addEventListener('sourceopen', () => {{
    try {{
      sourceBuffer = mediaSource.addSourceBuffer('video/mp4; codecs="avc1.42E01E"');
      sourceBuffer.mode = 'segments';
      sourceBuffer.addEventListener('updateend', () => {{
        appending = false;
        if (queue.length) appendNext();
        // Keep buffer from growing unbounded (evict old 30s)
        try {{
          if (video.buffered.length) {{
            const end = video.buffered.end(video.buffered.length-1);
            if (end - video.buffered.start(0) > 30) {{
              sourceBuffer.remove(0, end - 30);
            }}
          }}
        }} catch(e){{}}
      }});
      sourceBuffer.addEventListener('error', (e)=>{{ console.error('sourceBuffer error', e); }});
      connectWS();
    }} catch(e) {{
      console.error(e); statusEl.textContent = 'Failed to init decoder: ' + e;
    }}
  }});
}}

function appendNext() {{
  if (!sourceBuffer || appending || !queue.length) return;
  if (sourceBuffer.updating) return;
  appending = true;
  const data = queue.shift();
  try {{ sourceBuffer.appendBuffer(data); }}
  catch(e) {{
    // QuotaExceeded — evict and retry
    console.warn('append error', e);
    try {{ sourceBuffer.remove(0, Math.max(0, video.buffered.end(0) - 5)); }} catch(_){{}}
    queue.unshift(data);
    appending = false;
    setTimeout(appendNext, 50);
  }}
}}

function handleBinary(data) {{
  // data is ArrayBuffer
  const bytes = new Uint8Array(data);
  // Very small text frames are control JSON — viewer loop sends {{"type":"ended"}} as text,
  // so binary here is always fMP4.
  queue.push(bytes);
  if (!hasInit) {{
    hasInit = true;
    statusEl.textContent = 'Receiving…';
    setPill(true);
    // Try to play as soon as we have init
    video.play().catch(()=>{{}});
  }}
  appendNext();
  // Ensure video is playing once we have data
  if (video.paused && hasInit) {{
    video.play().catch(()=>{{}});
  }}
}}

function connectWS() {{
  statusEl.textContent = 'Connecting to ' + WS_URL + '…';
  ws = new WebSocket(WS_URL);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {{ statusEl.textContent = 'Connected — waiting for stream…'; }};
  ws.onclose = (e) => {{
    console.log('ws close', e.code, e.reason);
    statusEl.textContent = 'Disconnected — reconnecting…';
    setPill(false);
    setTimeout(connectWS, 1500);
  }};
  ws.onerror = () => {{ statusEl.textContent = 'WebSocket error — reconnecting…'; }};
  ws.onmessage = (ev) => {{
    if (typeof ev.data === 'string') {{
      try {{
        const msg = JSON.parse(ev.data);
        if (msg.error) {{ statusEl.textContent = msg.error; setPill(false); ws.close(); }}
        if (msg.type === 'ended') {{ statusEl.textContent = 'Stream ended'; setPill(false); }}
      }} catch(_) {{}}
      return;
    }}
    handleBinary(ev.data);
  }};
}}
</script>
</body>
</html>"#,
        id_esc = html_escape(id),
        id_json = id_json
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
