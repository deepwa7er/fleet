//! A minimal LSP client: JSON-RPC 2.0 over the child process's stdio, framed
//! with `Content-Length` headers. Hand-rolled on smol primitives (zed's own
//! approach) rather than embedding a second async runtime inside gpui's smol
//! world. One `LspClient` is one running language-server process.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use futures::io::BufReader;
use futures::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, SinkExt as _, StreamExt as _};
use gpui::{BackgroundExecutor, Task};
use lsp_types::notification::Notification as _;
use serde::Serialize;
use serde_json::{Value, json};

/// Diagnostics published by any server are funneled into one channel owned by
/// the `LspStore`, tagged with the document URI.
pub type DiagnosticsSender = mpsc::UnboundedSender<(lsp_types::Uri, Vec<lsp_types::Diagnostic>)>;

pub struct LspClient {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicI64,
    capabilities: OnceLock<lsp_types::ServerCapabilities>,
    child: Mutex<smol::process::Child>,
    _tasks: Vec<Task<()>>,
}

impl LspClient {
    /// Spawn `command` in `root`, run the initialize handshake, and return a
    /// ready client. Fails if the binary is missing or initialization fails.
    pub async fn start(
        command: &str,
        args: &[&str],
        root: &Path,
        executor: BackgroundExecutor,
        diagnostics: DiagnosticsSender,
    ) -> Result<Self> {
        let mut child = smol::process::Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot spawn {command} — is it installed?"))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (outgoing, outgoing_rx) = mpsc::unbounded::<Vec<u8>>();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let writer_task = executor.spawn(write_loop(stdin, outgoing_rx));
        let reader_task = executor.spawn(read_loop(
            stdout,
            pending.clone(),
            outgoing.clone(),
            diagnostics,
            command.to_string(),
        ));
        let stderr_task = executor.spawn(drain_stderr(stderr, command.to_string()));

        let client = Self {
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            capabilities: OnceLock::new(),
            child: Mutex::new(child),
            _tasks: vec![writer_task, reader_task, stderr_task],
        };

        let result = client
            .request::<lsp_types::request::Initialize>(initialize_params(root))
            .await
            .with_context(|| format!("{command}: initialize failed"))?;
        client
            .capabilities
            .set(result.capabilities)
            .expect("capabilities are set exactly once");
        client.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {})?;

        Ok(client)
    }

    pub fn capabilities(&self) -> &lsp_types::ServerCapabilities {
        self.capabilities
            .get()
            .expect("start() sets capabilities before returning")
    }

    /// Send a request; the returned future is `Send` and owns everything it
    /// needs, so it can run on the background executor.
    pub fn request<R: lsp_types::request::Request>(
        &self,
        params: R::Params,
    ) -> impl Future<Output = Result<R::Result>> + Send + use<R> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let frame = frame(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": params,
        }));
        let send = self.outgoing.unbounded_send(frame);

        async move {
            send.map_err(|_| anyhow!("language server exited"))?;
            let value = rx
                .await
                .map_err(|_| anyhow!("language server dropped the request"))??;
            Ok(serde_json::from_value(value)?)
        }
    }

    pub fn notify<N: lsp_types::notification::Notification>(
        &self,
        params: N::Params,
    ) -> Result<()> {
        let frame = frame(&json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": params,
        }));
        self.outgoing
            .unbounded_send(frame)
            .map_err(|_| anyhow!("language server exited"))
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort shutdown; the process dies with the pipe either way.
        let _ = self.child.lock().unwrap().kill();
    }
}

fn frame(message: &impl Serialize) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("lsp messages serialize");
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend(body);
    frame
}

async fn write_loop(mut stdin: smol::process::ChildStdin, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = rx.next().await {
        if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
}

async fn read_loop(
    stdout: smol::process::ChildStdout,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    mut diagnostics: DiagnosticsSender,
    server: String,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let message = match read_message(&mut reader).await {
            Ok(message) => message,
            Err(_) => break, // server exited or spoke garbage — stop reading
        };

        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            // Server → client request: answer inline; nothing here needs the UI.
            (Some(id), Some(method)) => {
                let response = respond_to_server_request(&message, method, id);
                if outgoing.unbounded_send(frame(&response)).is_err() {
                    break;
                }
            }
            // Notification.
            (None, Some(method)) => {
                if method == lsp_types::notification::PublishDiagnostics::METHOD
                    && let Some(params) = message.get("params")
                    && let Ok(params) = serde_json::from_value::<
                        lsp_types::PublishDiagnosticsParams,
                    >(params.clone())
                    && diagnostics
                        .send((params.uri, params.diagnostics))
                        .await
                        .is_err()
                {
                    break;
                }
            }
            // Response to one of our requests.
            (Some(id), None) => {
                let Some(id) = id.as_i64() else { continue };
                let Some(tx) = pending.lock().unwrap().remove(&id) else {
                    continue;
                };
                let result = if let Some(error) = message.get("error") {
                    Err(anyhow!("{server}: {error}"))
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
            (None, None) => {}
        }
    }

    // The server is gone: fail everything still waiting.
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(anyhow!("{server} exited")));
    }
}

fn respond_to_server_request(message: &Value, method: &str, id: Value) -> Value {
    match method {
        // Trivial acknowledgements the spec requires an answer to.
        "window/workDoneProgress/create" | "client/registerCapability"
        | "client/unregisterCapability" | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh" | "workspace/diagnostic/refresh"
        | "workspace/codeLens/refresh" => {
            json!({ "jsonrpc": "2.0", "id": id, "result": null })
        }
        // No configuration to offer: one null per requested item.
        "workspace/configuration" => {
            let count = message
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            json!({ "jsonrpc": "2.0", "id": id, "result": vec![Value::Null; count] })
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") },
        }),
    }
}

async fn read_message(reader: &mut BufReader<smol::process::ChildStdout>) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            anyhow::bail!("eof");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse()?);
        }
    }
    let content_length = content_length.context("missing Content-Length header")?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

async fn drain_stderr(stderr: smol::process::ChildStderr, server: String) {
    let mut lines = BufReader::new(stderr).lines();
    while let Some(Ok(line)) = lines.next().await {
        // Language servers log progress here; keep it out of the way unless
        // someone is debugging from a terminal.
        eprintln!("[{server}] {line}");
    }
}

fn initialize_params(root: &Path) -> lsp_types::InitializeParams {
    let uri = super::path_to_uri(root);
    lsp_types::InitializeParams {
        process_id: Some(std::process::id()),
        workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
            name: root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "workspace".to_string()),
            uri,
        }]),
        capabilities: lsp_types::ClientCapabilities {
            general: Some(lsp_types::GeneralClientCapabilities {
                position_encodings: Some(vec![lsp_types::PositionEncodingKind::UTF16]),
                ..Default::default()
            }),
            text_document: Some(lsp_types::TextDocumentClientCapabilities {
                synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                    did_save: Some(true),
                    ..Default::default()
                }),
                publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
                    ..Default::default()
                }),
                hover: Some(lsp_types::HoverClientCapabilities {
                    content_format: Some(vec![
                        lsp_types::MarkupKind::Markdown,
                        lsp_types::MarkupKind::PlainText,
                    ]),
                    ..Default::default()
                }),
                completion: Some(lsp_types::CompletionClientCapabilities {
                    ..Default::default()
                }),
                definition: Some(lsp_types::GotoCapability {
                    link_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}
