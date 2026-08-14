//! `RemoteWorkspace`: the `WorkspaceService` trait over the wire
//! (docs/remote.md). Spawns a transport process — `ssh <alias> ide-server
//! --stdio` in real use, the server binary directly in tests — and speaks the
//! `rpc` protocol over its stdio. Documents persist server-side (the server
//! runs the same `DocumentStore` auto-save); language requests answer empty
//! until slice 5d wires them through.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use futures::io::BufReader;
use futures::{AsyncWriteExt as _, FutureExt as _, StreamExt as _};
use futures::stream::BoxStream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::rpc::codec::{frame, read_message};
use crate::rpc::{self, method};
use crate::workspace::{DirEntry, TextMatch, WorkspaceService};

pub struct RemoteWorkspace {
    root: PathBuf,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicI64,
    sync_rx: Mutex<Option<mpsc::UnboundedReceiver<bool>>>,
    child: Mutex<smol::process::Child>,
    _tasks: Vec<smol::Task<()>>,
}

impl RemoteWorkspace {
    /// Spawn `command args…`, run the initialize handshake for
    /// `workspace_path`, and return a connected workspace.
    pub async fn connect(command: &str, args: &[String], workspace_path: &str) -> Result<Self> {
        let mut child = smol::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot spawn {command}"))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let (outgoing, mut outgoing_rx) = mpsc::unbounded::<Vec<u8>>();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (sync_tx, sync_rx) = mpsc::unbounded::<bool>();

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

        let reader = smol::spawn({
            let pending = pending.clone();
            async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    let message = match read_message(&mut reader).await {
                        Ok(message) => message,
                        Err(_) => break, // server gone
                    };
                    let id = message.get("id").and_then(Value::as_i64);
                    let method_name = message.get("method").and_then(Value::as_str);
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
                            if let Some(params) = message.get("params")
                                && let Ok(params) = serde_json::from_value::<rpc::SyncStateParams>(
                                    params.clone(),
                                )
                                && sync_tx.unbounded_send(params.synced).is_err()
                            {
                                break;
                            }
                        }
                        _ => {} // slice 5d: diagnostics and friends
                    }
                }
                for (_, tx) in pending.lock().unwrap().drain() {
                    let _ = tx.send(Err(anyhow!("connection to ide-server lost")));
                }
            }
        });

        let mut this = Self {
            root: PathBuf::new(),
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            sync_rx: Mutex::new(Some(sync_rx)),
            child: Mutex::new(child),
            _tasks: vec![writer, reader],
        };

        let init: rpc::InitializeResult = this
            .request(
                method::INITIALIZE,
                rpc::InitializeParams {
                    version: rpc::PROTOCOL_VERSION,
                    path: workspace_path.to_string(),
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

        this.root = init.root;
        Ok(this)
    }

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

impl Drop for RemoteWorkspace {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

impl WorkspaceService for RemoteWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn read_dir(&self, path: &Path) -> BoxFuture<'static, Result<Vec<DirEntry>>> {
        self.request(method::READ_DIR, rpc::PathParams { path: path.to_owned() })
            .boxed()
    }

    fn read_file(&self, path: &Path) -> BoxFuture<'static, Result<String>> {
        self.request(method::READ_FILE, rpc::PathParams { path: path.to_owned() })
            .boxed()
    }

    fn list_files(&self) -> BoxFuture<'static, Result<Vec<PathBuf>>> {
        self.request(method::LIST_FILES, ()).boxed()
    }

    fn search_text(
        &self,
        query: String,
        limit: usize,
    ) -> BoxFuture<'static, Result<Vec<TextMatch>>> {
        self.request(method::SEARCH_TEXT, rpc::SearchTextParams { query, limit })
            .boxed()
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
        self.request::<_, ()>(
            method::DOC_SAVE,
            rpc::DocTextParams {
                path: path.to_owned(),
                text,
            },
        )
        .boxed()
    }

    fn document_closed(&self, path: &Path) {
        self.notify(method::DOC_CLOSE, rpc::PathParams { path: path.to_owned() });
    }

    // Language intelligence crosses the wire in slice 5d; until then remote
    // editors simply have none.
    fn completion(
        &self,
        _path: &Path,
        _position: lsp_types::Position,
        _context: lsp_types::CompletionContext,
    ) -> BoxFuture<'static, Result<lsp_types::CompletionResponse>> {
        futures::future::ready(Ok(lsp_types::CompletionResponse::Array(vec![]))).boxed()
    }

    fn hover(
        &self,
        _path: &Path,
        _position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>>> {
        futures::future::ready(Ok(None)).boxed()
    }

    fn definition(
        &self,
        _path: &Path,
        _position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Vec<lsp_types::LocationLink>>> {
        futures::future::ready(Ok(vec![])).boxed()
    }

    fn completion_triggers(&self, _path: &Path) -> Vec<String> {
        Vec::new()
    }

    fn subscribe_diagnostics(
        &self,
    ) -> Option<BoxStream<'static, (PathBuf, Vec<lsp_types::Diagnostic>)>> {
        None // slice 5d
    }

    fn subscribe_sync_state(&self) -> Option<BoxStream<'static, bool>> {
        Some(futures::StreamExt::boxed(
            self.sync_rx.lock().unwrap().take()?,
        ))
    }

    fn flush_all(&self) -> BoxFuture<'static, Result<()>> {
        // Persistence is server-side; the server flushes on disconnect and
        // shutdown. Quitting the client has nothing local to flush.
        futures::future::ready(Ok(())).boxed()
    }
}
