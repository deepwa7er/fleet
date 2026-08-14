//! The headless side of remote mode: serve one `LocalWorkspace` over stdio
//! (docs/remote.md). Per-session by design — the process lives exactly as
//! long as the connection; EOF on stdin means the client is gone, and the
//! server flushes every open document before exiting so the sub-second
//! auto-save window cannot leak across a disconnect.
//!
//! Requests are handled sequentially in arrival order; every workspace op is
//! fast (fs reads, capped searches). Revisit if a slow request ever stalls
//! the pipe.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc;
use futures::io::BufReader;
use futures::{AsyncWriteExt as _, StreamExt as _};
use serde::Serialize;
use serde_json::{Value, json};

use crate::rpc::codec::{frame, read_message};
use crate::rpc::{self, method};
use crate::workspace::{LocalWorkspace, WorkspaceService};

/// Run the stdio session to completion. Returns after shutdown or client EOF.
pub async fn serve_stdio() -> Result<()> {
    let mut reader = BufReader::new(smol::Unblock::new(std::io::stdin()));
    let (outgoing, mut outgoing_rx) = mpsc::unbounded::<Vec<u8>>();
    let writer = smol::spawn(async move {
        let mut stdout = smol::Unblock::new(std::io::stdout());
        while let Some(frame) = outgoing_rx.next().await {
            if stdout.write_all(&frame).await.is_err() || stdout.flush().await.is_err() {
                return;
            }
        }
    });

    // The first message must be initialize: version gate, then open the root.
    let first = read_message(&mut reader).await.context("no initialize")?;
    let init_id = first.get("id").cloned().unwrap_or(Value::Null);
    let workspace = match parse_initialize(&first) {
        Ok(params) if params.version != rpc::PROTOCOL_VERSION => {
            respond_err(
                &outgoing,
                init_id,
                format!(
                    "protocol version mismatch (server {} / client {})",
                    rpc::PROTOCOL_VERSION,
                    params.version
                ),
            );
            return Ok(());
        }
        Ok(params) => {
            let root = resolve_root(&params.path);
            match LocalWorkspace::new(&root) {
                Ok(workspace) => {
                    let workspace = Arc::new(workspace);
                    respond_ok(
                        &outgoing,
                        init_id,
                        &rpc::InitializeResult {
                            version: rpc::PROTOCOL_VERSION,
                            root: workspace.root().to_owned(),
                        },
                    );
                    workspace
                }
                Err(err) => {
                    respond_err(&outgoing, init_id, format!("{err:#}"));
                    return Ok(());
                }
            }
        }
        Err(err) => {
            respond_err(&outgoing, init_id, format!("{err:#}"));
            return Ok(());
        }
    };

    // Forward auto-save sync state as notifications.
    let sync_task = workspace.subscribe_sync_state().map(|mut sync| {
        let outgoing = outgoing.clone();
        smol::spawn(async move {
            while let Some(synced) = sync.next().await {
                if notify(&outgoing, method::SYNC_STATE, &rpc::SyncStateParams { synced })
                    .is_err()
                {
                    break;
                }
            }
        })
    });

    // Forward language-server diagnostics as notifications.
    let diagnostics_task = workspace.subscribe_diagnostics().map(|mut diagnostics| {
        let outgoing = outgoing.clone();
        smol::spawn(async move {
            while let Some((path, diagnostics)) = diagnostics.next().await {
                let params = rpc::DiagnosticsParams { path, diagnostics };
                if notify(&outgoing, method::LANG_DIAGNOSTICS, &params).is_err() {
                    break;
                }
            }
        })
    });

    loop {
        let message = match read_message(&mut reader).await {
            Ok(message) => message,
            Err(_) => break, // client gone
        };
        let id = message.get("id").cloned();
        let Some(method_name) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match (id, method_name) {
            (Some(id), method::SHUTDOWN) => {
                match workspace.flush_all().await {
                    Ok(()) => respond_ok(&outgoing, id, &Value::Null),
                    Err(err) => respond_err(&outgoing, id, format!("{err:#}")),
                }
                // Close the channel so the writer drains and ends, then wait
                // for it — dropping it would cancel unsent frames.
                drop(sync_task);
                drop(diagnostics_task);
                drop(outgoing);
                writer.await;
                return Ok(());
            }
            (Some(id), method::READ_DIR) => {
                let reply = match parse::<rpc::PathParams>(params) {
                    Ok(p) => workspace.read_dir(&p.path).await.map(|entries| {
                        serde_json::to_value(entries).expect("dir entries serialize")
                    }),
                    Err(err) => Err(err),
                };
                respond(&outgoing, id, reply);
            }
            (Some(id), method::READ_FILE) => {
                let reply = match parse::<rpc::PathParams>(params) {
                    Ok(p) => workspace.read_file(&p.path).await.map(Value::String),
                    Err(err) => Err(err),
                };
                respond(&outgoing, id, reply);
            }
            (Some(id), method::LIST_FILES) => {
                let reply = workspace
                    .list_files()
                    .await
                    .map(|files| serde_json::to_value(files).expect("paths serialize"));
                respond(&outgoing, id, reply);
            }
            (Some(id), method::SEARCH_TEXT) => {
                let reply = match parse::<rpc::SearchTextParams>(params) {
                    Ok(p) => workspace.search_text(p.query, p.limit).await.map(|hits| {
                        serde_json::to_value(hits).expect("text matches serialize")
                    }),
                    Err(err) => Err(err),
                };
                respond(&outgoing, id, reply);
            }
            // Language requests can be slow (a language server mid-index);
            // they run detached so they never stall the request pipe —
            // responses correlate by id regardless of order.
            (Some(id), method::LANG_COMPLETION) => match parse::<rpc::CompletionRequestParams>(
                params,
            ) {
                Ok(p) => {
                    let request = workspace.completion(&p.path, p.position, p.context);
                    let outgoing = outgoing.clone();
                    smol::spawn(async move {
                        let reply = request.await.map(|response| {
                            serde_json::to_value(response).expect("completions serialize")
                        });
                        respond(&outgoing, id, reply);
                    })
                    .detach();
                }
                Err(err) => respond_err(&outgoing, id, format!("{err:#}")),
            },
            (Some(id), method::LANG_HOVER) => match parse::<rpc::PositionParams>(params) {
                Ok(p) => {
                    let request = workspace.hover(&p.path, p.position);
                    let outgoing = outgoing.clone();
                    smol::spawn(async move {
                        let reply = request
                            .await
                            .map(|hover| serde_json::to_value(hover).expect("hover serializes"));
                        respond(&outgoing, id, reply);
                    })
                    .detach();
                }
                Err(err) => respond_err(&outgoing, id, format!("{err:#}")),
            },
            (Some(id), method::LANG_DEFINITION) => match parse::<rpc::PositionParams>(params) {
                Ok(p) => {
                    let request = workspace.definition(&p.path, p.position);
                    let outgoing = outgoing.clone();
                    smol::spawn(async move {
                        let reply = request
                            .await
                            .map(|links| serde_json::to_value(links).expect("links serialize"));
                        respond(&outgoing, id, reply);
                    })
                    .detach();
                }
                Err(err) => respond_err(&outgoing, id, format!("{err:#}")),
            },
            (Some(id), method::DOC_SAVE) => {
                let reply = match parse::<rpc::DocTextParams>(params) {
                    Ok(p) => workspace
                        .document_save(&p.path, p.text)
                        .await
                        .map(|()| Value::Null),
                    Err(err) => Err(err),
                };
                respond(&outgoing, id, reply);
            }
            (None, method::DOC_OPEN) => {
                if let Ok(p) = parse::<rpc::DocTextParams>(params) {
                    workspace.document_open(&p.path, p.text);
                    // Push this document's completion triggers once its
                    // language server is up; empty sets are not worth a frame.
                    let triggers = workspace.completion_triggers_ready(&p.path);
                    let outgoing = outgoing.clone();
                    let path = p.path;
                    smol::spawn(async move {
                        let triggers = triggers.await;
                        if !triggers.is_empty() {
                            let _ = notify(
                                &outgoing,
                                method::LANG_TRIGGERS,
                                &rpc::TriggersParams { path, triggers },
                            );
                        }
                    })
                    .detach();
                }
            }
            (None, method::DOC_CHANGE) => {
                if let Ok(p) = parse::<rpc::DocTextParams>(params) {
                    workspace.document_changed(&p.path, p.text);
                }
            }
            (None, method::DOC_CLOSE) => {
                if let Ok(p) = parse::<rpc::PathParams>(params) {
                    workspace.document_closed(&p.path);
                }
            }
            (Some(id), unknown) => {
                respond_err(&outgoing, id, format!("method not found: {unknown}"));
            }
            (None, _) => {}
        }
    }

    // Client vanished without shutdown (dropped connection, killed ssh):
    // flush so the last debounce window still reaches disk.
    if let Err(err) = workspace.flush_all().await {
        eprintln!("ide-server: flush on disconnect failed: {err:#}");
    }
    drop(sync_task);
    drop(diagnostics_task);
    drop(outgoing);
    writer.await;
    Ok(())
}

fn notify(
    outgoing: &mpsc::UnboundedSender<Vec<u8>>,
    method: &'static str,
    params: &impl Serialize,
) -> Result<()> {
    outgoing
        .unbounded_send(frame(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })))
        .map_err(|_| anyhow!("writer gone"))
}

fn parse_initialize(message: &Value) -> Result<rpc::InitializeParams> {
    anyhow::ensure!(
        message.get("method").and_then(Value::as_str) == Some(method::INITIALIZE),
        "first message must be initialize"
    );
    parse(message.get("params").cloned().unwrap_or(Value::Null))
}

fn parse<P: serde::de::DeserializeOwned>(params: Value) -> Result<P> {
    serde_json::from_value(params).map_err(|err| anyhow!("invalid params: {err}"))
}

/// Relative workspace paths resolve against $HOME (`ide desktop:code/fleet`).
fn resolve_root(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(path)
    }
}

fn respond(outgoing: &mpsc::UnboundedSender<Vec<u8>>, id: Value, reply: Result<Value>) {
    match reply {
        Ok(result) => respond_ok(outgoing, id, &result),
        Err(err) => respond_err(outgoing, id, format!("{err:#}")),
    }
}

fn respond_ok(outgoing: &mpsc::UnboundedSender<Vec<u8>>, id: Value, result: &impl Serialize) {
    let _ = outgoing.unbounded_send(frame(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })));
}

fn respond_err(outgoing: &mpsc::UnboundedSender<Vec<u8>>, id: Value, message: String) {
    let _ = outgoing.unbounded_send(frame(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": message },
    })));
}
