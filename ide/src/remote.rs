//! `RemoteWorkspace`: the `WorkspaceService` trait over the wire
//! (docs/remote.md). Spawns a transport process — `ssh <alias> ide-server
//! --stdio` in real use, the server binary directly in tests — and speaks the
//! `rpc` protocol over its stdio.
//!
//! Connection lifecycle (§6): the workspace outlives its connection. The
//! client-facing streams (sync, diagnostics, connection events) are created
//! once and survive reconnects — each session's reader feeds them. When a
//! reader dies it reports the loss (guarded by a generation counter so a
//! stale session can't clobber its successor), a bounded retry round runs
//! (~15s of backoff, the MacBook-wakes-up case), and beyond that reconnect
//! is manual. Requests while disconnected fail fast with an instructive
//! error; document notifications are dropped — editors are read-only while
//! down, and a restored session re-reads server truth anyway.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use futures::io::BufReader;
use futures::stream::BoxStream;
use futures::{AsyncWriteExt as _, FutureExt as _, StreamExt as _};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::rpc::codec::{frame, read_message};
use crate::rpc::{self, method};
use crate::workspace::{ConnectionEvent, DirEntry, TextMatch, WorkspaceService};

type PathDiagnostics = (PathBuf, Vec<lsp_types::Diagnostic>);

/// Backoff between automatic reconnect attempts after a lost connection.
const AUTO_RETRY: &[Duration] = &[
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(8),
];
/// Manual reconnect tries immediately, then briefly again.
const MANUAL_RETRY: &[Duration] = &[Duration::ZERO, Duration::from_secs(2)];

struct Connection {
    generation: u64,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicI64,
    child: Mutex<smol::process::Child>,
    _tasks: Vec<smol::Task<()>>,
}

impl Connection {
    fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &'static str,
        params: P,
    ) -> impl Future<Output = Result<R>> + Send + use<P, R> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let frame = frame(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let send = self.outgoing.unbounded_send(frame);
        async move {
            send.map_err(|_| anyhow!("connection to ide-server lost"))?;
            let value = rx
                .await
                .map_err(|_| anyhow!("connection to ide-server lost"))??;
            Ok(serde_json::from_value(value)?)
        }
    }

    fn notify<P: Serialize>(&self, method: &'static str, params: P) {
        let frame = frame(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
        let _ = self.outgoing.unbounded_send(frame);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

pub struct RemoteWorkspace {
    command: String,
    args: Vec<String>,
    workspace_path: String,
    /// Set by the first session; later sessions must agree.
    root: OnceLock<PathBuf>,
    /// Weak self, set at construction: `reconnect(&self)` needs an owning
    /// handle to hand the retry task.
    weak_self: OnceLock<Weak<RemoteWorkspace>>,
    conn: Mutex<Option<Arc<Connection>>>,
    generation: AtomicU64,
    retrying: AtomicBool,
    // Persistent client-facing channels: subscribed once, fed by every session.
    sync_tx: mpsc::UnboundedSender<bool>,
    sync_rx: Mutex<Option<mpsc::UnboundedReceiver<bool>>>,
    diagnostics_tx: mpsc::UnboundedSender<PathDiagnostics>,
    diagnostics_rx: Mutex<Option<mpsc::UnboundedReceiver<PathDiagnostics>>>,
    events_tx: mpsc::UnboundedSender<ConnectionEvent>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnectionEvent>>>,
    triggers: Mutex<HashMap<PathBuf, Vec<String>>>,
}

impl RemoteWorkspace {
    /// Spawn `command args…`, run the initialize handshake for
    /// `workspace_path`, and return a connected workspace.
    pub async fn connect(
        command: &str,
        args: &[String],
        workspace_path: &str,
    ) -> Result<Arc<Self>> {
        let (sync_tx, sync_rx) = mpsc::unbounded();
        let (diagnostics_tx, diagnostics_rx) = mpsc::unbounded();
        let (events_tx, events_rx) = mpsc::unbounded();
        let this = Arc::new(Self {
            command: command.to_string(),
            args: args.to_vec(),
            workspace_path: workspace_path.to_string(),
            root: OnceLock::new(),
            weak_self: OnceLock::new(),
            conn: Mutex::new(None),
            generation: AtomicU64::new(0),
            retrying: AtomicBool::new(false),
            sync_tx,
            sync_rx: Mutex::new(Some(sync_rx)),
            diagnostics_tx,
            diagnostics_rx: Mutex::new(Some(diagnostics_rx)),
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
            triggers: Mutex::new(HashMap::new()),
        });
        this.weak_self
            .set(Arc::downgrade(&this))
            .expect("weak_self is set exactly once");
        this.establish().await?;
        Ok(this)
    }

    /// Spawn a session, handshake, and install it as the current connection.
    async fn establish(self: &Arc<Self>) -> Result<()> {
        let mut child = smol::process::Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot spawn {}", self.command))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;

        let (outgoing, mut outgoing_rx) = mpsc::unbounded::<Vec<u8>>();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let writer = smol::spawn({
            let mut stdin = stdin;
            async move {
                while let Some(frame) = outgoing_rx.next().await {
                    if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                        return;
                    }
                }
            }
        });

        let reader = smol::spawn(read_session(
            stdout,
            pending.clone(),
            Arc::downgrade(self),
            generation,
        ));

        let conn = Arc::new(Connection {
            generation,
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            child: Mutex::new(child),
            _tasks: vec![writer, reader],
        });

        let init: rpc::InitializeResult = conn
            .request(
                method::INITIALIZE,
                rpc::InitializeParams {
                    version: rpc::PROTOCOL_VERSION,
                    path: self.workspace_path.clone(),
                },
            )
            .await
            .context("initialize failed — is the workspace path valid on the server?")?;
        anyhow::ensure!(
            init.version == rpc::PROTOCOL_VERSION,
            "protocol version mismatch (client {} / server {}): rebuild ide-server on the host \
             (cd ~/code/fleet && git pull && cargo install --path ide --bin ide-server)",
            rpc::PROTOCOL_VERSION,
            init.version,
        );

        let root = self.root.get_or_init(|| init.root.clone());
        anyhow::ensure!(
            *root == init.root,
            "server reopened a different root ({} vs {})",
            init.root.display(),
            root.display(),
        );

        *self.conn.lock().unwrap() = Some(conn);
        Ok(())
    }

    fn on_session_dead(self: &Arc<Self>, generation: u64) {
        {
            let mut conn = self.conn.lock().unwrap();
            match conn.as_ref() {
                Some(current) if current.generation == generation => *conn = None,
                _ => return, // a stale session ended after its successor took over
            }
        }
        let _ = self.events_tx.unbounded_send(ConnectionEvent::Lost);
        self.spawn_retry_round(AUTO_RETRY);
    }

    fn spawn_retry_round(self: &Arc<Self>, delays: &'static [Duration]) {
        if self.retrying.swap(true, Ordering::SeqCst) {
            return; // a round is already running
        }
        let this = self.clone();
        smol::spawn(async move {
            let _ = this.events_tx.unbounded_send(ConnectionEvent::Reconnecting);
            for delay in delays {
                smol::Timer::after(*delay).await;
                match this.establish().await {
                    Ok(()) => {
                        this.retrying.store(false, Ordering::SeqCst);
                        let _ = this.events_tx.unbounded_send(ConnectionEvent::Restored);
                        return;
                    }
                    Err(err) => eprintln!("ide: reconnect attempt failed: {err:#}"),
                }
            }
            this.retrying.store(false, Ordering::SeqCst);
            let _ = this.events_tx.unbounded_send(ConnectionEvent::Lost);
        })
        .detach();
    }

    fn current(&self) -> Option<Arc<Connection>> {
        self.conn.lock().unwrap().clone()
    }

    fn request<P, R>(&self, method: &'static str, params: P) -> BoxFuture<'static, Result<R>>
    where
        P: Serialize + Send + 'static,
        R: DeserializeOwned + Send + 'static,
    {
        match self.current() {
            Some(conn) => conn.request(method, params).boxed(),
            None => futures::future::ready(Err(anyhow!(
                "disconnected from ide-server — reconnect with ctrl-shift-r"
            )))
            .boxed(),
        }
    }

    fn notify<P: Serialize>(&self, method: &'static str, params: P) {
        if let Some(conn) = self.current() {
            conn.notify(method, params);
        }
        // Down: dropped by design — editors are read-only and a restored
        // session re-reads server truth.
    }

    fn record_triggers(&self, path: PathBuf, triggers: Vec<String>) {
        self.triggers.lock().unwrap().insert(path, triggers);
    }

    /// Test instrumentation: kill the live session's transport process so the
    /// disconnect/reconnect path can be exercised end-to-end.
    #[doc(hidden)]
    pub fn debug_kill_connection(&self) {
        if let Some(conn) = self.current() {
            let _ = conn.child.lock().unwrap().kill();
        }
    }
}

/// One session's reader: feeds the workspace's persistent streams, then
/// reports its own death (generation-guarded).
async fn read_session(
    stdout: smol::process::ChildStdout,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>,
    workspace: Weak<RemoteWorkspace>,
    generation: u64,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let message = match read_message(&mut reader).await {
            Ok(message) => message,
            Err(_) => break, // server gone
        };
        let id = message.get("id").and_then(Value::as_i64);
        let method_name = message.get("method").and_then(Value::as_str);
        let params = || message.get("params").cloned().unwrap_or(Value::Null);
        match (id, method_name) {
            (Some(id), None) => {
                let Some(tx) = pending.lock().unwrap().remove(&id) else {
                    continue;
                };
                let result = if let Some(error) = message.get("error") {
                    Err(anyhow!("ide-server: {error}"))
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
            (None, Some(method::SYNC_STATE)) => {
                let Some(this) = workspace.upgrade() else { break };
                if let Ok(p) = serde_json::from_value::<rpc::SyncStateParams>(params()) {
                    let _ = this.sync_tx.unbounded_send(p.synced);
                }
            }
            (None, Some(method::LANG_DIAGNOSTICS)) => {
                let Some(this) = workspace.upgrade() else { break };
                if let Ok(p) = serde_json::from_value::<rpc::DiagnosticsParams>(params()) {
                    let _ = this.diagnostics_tx.unbounded_send((p.path, p.diagnostics));
                }
            }
            (None, Some(method::LANG_TRIGGERS)) => {
                let Some(this) = workspace.upgrade() else { break };
                if let Ok(p) = serde_json::from_value::<rpc::TriggersParams>(params()) {
                    this.record_triggers(p.path, p.triggers);
                }
            }
            _ => {}
        }
    }

    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(anyhow!("connection to ide-server lost")));
    }
    if let Some(this) = workspace.upgrade() {
        this.on_session_dead(generation);
    }
}

impl WorkspaceService for RemoteWorkspace {
    fn root(&self) -> &Path {
        self.root.get().expect("connect() sets the root")
    }

    fn read_dir(&self, path: &Path) -> BoxFuture<'static, Result<Vec<DirEntry>>> {
        self.request(method::READ_DIR, rpc::PathParams { path: path.to_owned() })
    }

    fn read_file(&self, path: &Path) -> BoxFuture<'static, Result<String>> {
        self.request(method::READ_FILE, rpc::PathParams { path: path.to_owned() })
    }

    fn list_files(&self) -> BoxFuture<'static, Result<Vec<PathBuf>>> {
        self.request(method::LIST_FILES, ())
    }

    fn search_text(
        &self,
        query: String,
        limit: usize,
    ) -> BoxFuture<'static, Result<Vec<TextMatch>>> {
        self.request(method::SEARCH_TEXT, rpc::SearchTextParams { query, limit })
    }

    fn document_open(&self, path: &Path, text: String) {
        self.notify(
            method::DOC_OPEN,
            rpc::DocTextParams {
                path: path.to_owned(),
                text,
            },
        );
    }

    fn document_changed(&self, path: &Path, text: String) {
        self.notify(
            method::DOC_CHANGE,
            rpc::DocTextParams {
                path: path.to_owned(),
                text,
            },
        );
    }

    fn document_save(&self, path: &Path, text: String) -> BoxFuture<'static, Result<()>> {
        self.request(
            method::DOC_SAVE,
            rpc::DocTextParams {
                path: path.to_owned(),
                text,
            },
        )
    }

    fn document_closed(&self, path: &Path) {
        self.notify(method::DOC_CLOSE, rpc::PathParams { path: path.to_owned() });
    }

    fn completion(
        &self,
        path: &Path,
        position: lsp_types::Position,
        context: lsp_types::CompletionContext,
    ) -> BoxFuture<'static, Result<lsp_types::CompletionResponse>> {
        self.request(
            method::LANG_COMPLETION,
            rpc::CompletionRequestParams {
                path: path.to_owned(),
                position,
                context,
            },
        )
    }

    fn hover(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>>> {
        self.request(
            method::LANG_HOVER,
            rpc::PositionParams {
                path: path.to_owned(),
                position,
            },
        )
    }

    fn definition(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Vec<lsp_types::LocationLink>>> {
        self.request(
            method::LANG_DEFINITION,
            rpc::PositionParams {
                path: path.to_owned(),
                position,
            },
        )
    }

    fn completion_triggers(&self, path: &Path) -> Vec<String> {
        self.triggers
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    fn subscribe_diagnostics(
        &self,
    ) -> Option<BoxStream<'static, (PathBuf, Vec<lsp_types::Diagnostic>)>> {
        Some(futures::StreamExt::boxed(
            self.diagnostics_rx.lock().unwrap().take()?,
        ))
    }

    fn subscribe_sync_state(&self) -> Option<BoxStream<'static, bool>> {
        Some(futures::StreamExt::boxed(
            self.sync_rx.lock().unwrap().take()?,
        ))
    }

    fn subscribe_connection(&self) -> Option<BoxStream<'static, ConnectionEvent>> {
        Some(futures::StreamExt::boxed(
            self.events_rx.lock().unwrap().take()?,
        ))
    }

    fn reconnect(&self) {
        if self.current().is_some() {
            return; // already connected
        }
        if let Some(this) = self.weak_self.get().and_then(Weak::upgrade) {
            this.spawn_retry_round(MANUAL_RETRY);
        }
    }

    fn flush_all(&self) -> BoxFuture<'static, Result<()>> {
        // Persistence is server-side; the server flushes on disconnect and
        // shutdown. Quitting the client has nothing local to flush.
        futures::future::ready(Ok(())).boxed()
    }
}
