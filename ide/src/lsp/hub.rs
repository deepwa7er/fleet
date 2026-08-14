//! The language hub: routes documents to language servers by extension and
//! workspace root, keeps per-document notification order, and answers
//! completion/hover/definition requests. Successor to the gpui-entity
//! `LspStore` — a plain struct with no gpui types, so it runs identically
//! behind `LocalWorkspace` in the GUI and inside the headless ide-server
//! (docs/remote.md, slice 5c).
//!
//! Ordering: each open document gets one worker task holding an op channel.
//! The worker awaits the (shared) server startup once, sends didOpen, then
//! drains ops sequentially — didChange/didSave/didClose can never reorder,
//! including ops queued while the server was still starting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt as _, StreamExt as _};
use lsp_types::notification as notif;

use super::client::LspClient;
use super::{path_to_uri, uri_to_path};

/// A starting-or-started server, shareable across documents. The error is a
/// pre-formatted message so the result stays `Clone`.
type ServerFuture = Shared<smol::Task<Result<Arc<LspClient>, Arc<str>>>>;

pub type DiagnosticsReceiver = mpsc::UnboundedReceiver<(PathBuf, Vec<lsp_types::Diagnostic>)>;

struct ServerSpec {
    id: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    root: RootRule,
}

enum RootRule {
    /// Nearest ancestor whose `Cargo.toml` declares `[workspace]`, else the
    /// nearest `Cargo.toml`, else the workspace root. Handles the fleet's
    /// nested-workspaces layout (`ide/` is excluded from the root workspace).
    CargoWorkspace,
    /// Nearest ancestor containing any of these files, else the workspace root.
    Markers(&'static [&'static str]),
}

static RUST_ANALYZER: ServerSpec = ServerSpec {
    id: "rust-analyzer",
    command: "rust-analyzer",
    args: &[],
    root: RootRule::CargoWorkspace,
};
static RUBY_LSP: ServerSpec = ServerSpec {
    id: "ruby-lsp",
    command: "ruby-lsp",
    args: &[],
    root: RootRule::Markers(&["Gemfile"]),
};
static GOPLS: ServerSpec = ServerSpec {
    id: "gopls",
    command: "gopls",
    args: &[],
    root: RootRule::Markers(&["go.mod"]),
};
static BASEDPYRIGHT: ServerSpec = ServerSpec {
    id: "basedpyright",
    command: "basedpyright-langserver",
    args: &["--stdio"],
    root: RootRule::Markers(&["pyproject.toml", "setup.py", "requirements.txt"]),
};
static VTSLS: ServerSpec = ServerSpec {
    id: "vtsls",
    command: "vtsls",
    args: &["--stdio"],
    root: RootRule::Markers(&["tsconfig.json", "package.json"]),
};

fn spec_for_extension(extension: &str) -> Option<(&'static ServerSpec, &'static str)> {
    match extension {
        "rs" => Some((&RUST_ANALYZER, "rust")),
        "rb" | "rake" | "ru" | "gemspec" => Some((&RUBY_LSP, "ruby")),
        "erb" => Some((&RUBY_LSP, "eruby")),
        "go" => Some((&GOPLS, "go")),
        "py" | "pyi" => Some((&BASEDPYRIGHT, "python")),
        "ts" | "mts" | "cts" => Some((&VTSLS, "typescript")),
        "tsx" => Some((&VTSLS, "typescriptreact")),
        "js" | "mjs" | "cjs" => Some((&VTSLS, "javascript")),
        "jsx" => Some((&VTSLS, "javascriptreact")),
        _ => None,
    }
}

enum DocOp {
    Change { version: i32, text: String },
    Save { text: String },
    Close,
}

struct Document {
    server: ServerFuture,
    version: i32,
    ops: mpsc::UnboundedSender<DocOp>,
    _worker: smol::Task<()>,
}

struct HubState {
    servers: HashMap<(&'static str, PathBuf), ServerFuture>,
    documents: HashMap<PathBuf, Document>,
}

pub struct LanguageHub {
    workspace_root: PathBuf,
    state: Mutex<HubState>,
    /// Every client funnels publishDiagnostics here, keyed by URI…
    lsp_diagnostics: super::client::DiagnosticsSender,
    /// …a router converts to paths, and the single subscriber takes this end.
    diagnostics: Mutex<Option<DiagnosticsReceiver>>,
    _router: smol::Task<()>,
}

impl LanguageHub {
    pub fn new(workspace_root: PathBuf) -> Self {
        let (lsp_tx, mut lsp_rx) =
            mpsc::unbounded::<(lsp_types::Uri, Vec<lsp_types::Diagnostic>)>();
        let (path_tx, path_rx) = mpsc::unbounded();
        let router = smol::spawn(async move {
            while let Some((uri, diagnostics)) = lsp_rx.next().await {
                let Some(path) = uri_to_path(&uri) else { continue };
                if path_tx.unbounded_send((path, diagnostics)).is_err() {
                    break;
                }
            }
        });

        Self {
            workspace_root,
            state: Mutex::new(HubState {
                servers: HashMap::new(),
                documents: HashMap::new(),
            }),
            lsp_diagnostics: lsp_tx,
            diagnostics: Mutex::new(Some(path_rx)),
            _router: router,
        }
    }

    /// The diagnostics stream, taken exactly once by the consumer.
    pub fn take_diagnostics(&self) -> Option<DiagnosticsReceiver> {
        self.diagnostics.lock().unwrap().take()
    }

    pub fn document_open(&self, path: &Path, text: String) {
        let Some((spec, language_id)) = path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(spec_for_extension)
        else {
            return; // no language server configured for this file
        };
        let root = self.detect_root(spec, path);

        let mut state = self.state.lock().unwrap();
        let server = Self::ensure_server(
            &mut state.servers,
            spec,
            root,
            self.lsp_diagnostics.clone(),
        );
        let (ops_tx, ops_rx) = mpsc::unbounded();
        let worker = smol::spawn(document_worker(
            server.clone(),
            path_to_uri(path),
            language_id,
            text,
            ops_rx,
        ));
        state.documents.insert(
            path.to_owned(),
            Document {
                server,
                version: 0,
                ops: ops_tx,
                _worker: worker,
            },
        );
    }

    pub fn document_changed(&self, path: &Path, text: String) {
        let mut state = self.state.lock().unwrap();
        let Some(doc) = state.documents.get_mut(path) else {
            return;
        };
        doc.version += 1;
        let _ = doc.ops.unbounded_send(DocOp::Change {
            version: doc.version,
            text,
        });
    }

    pub fn document_saved(&self, path: &Path, text: String) {
        let state = self.state.lock().unwrap();
        if let Some(doc) = state.documents.get(path) {
            let _ = doc.ops.unbounded_send(DocOp::Save { text });
        }
    }

    pub fn document_closed(&self, path: &Path) {
        let Some(doc) = self.state.lock().unwrap().documents.remove(path) else {
            return;
        };
        let _ = doc.ops.unbounded_send(DocOp::Close);
        // The entry is gone; let the worker finish didClose on its own.
        doc._worker.detach();
    }

    pub fn completion_triggers(&self, path: &Path) -> Vec<String> {
        self.started_client(path)
            .and_then(|client| client.capabilities().completion_provider.clone())
            .and_then(|provider| provider.trigger_characters)
            .unwrap_or_default()
    }

    /// Like [`Self::completion_triggers`], but awaits server startup — used
    /// by ide-server to push triggers to the remote client once known.
    pub fn completion_triggers_ready(&self, path: &Path) -> BoxFuture<'static, Vec<String>> {
        let Some(server) = self.server_for(path) else {
            return futures::future::ready(Vec::new()).boxed();
        };
        async move {
            let Ok(client) = server.await else {
                return Vec::new();
            };
            client
                .capabilities()
                .completion_provider
                .clone()
                .and_then(|provider| provider.trigger_characters)
                .unwrap_or_default()
        }
        .boxed()
    }

    pub fn completion(
        &self,
        path: &Path,
        position: lsp_types::Position,
        context: lsp_types::CompletionContext,
    ) -> BoxFuture<'static, Result<lsp_types::CompletionResponse>> {
        let empty = || lsp_types::CompletionResponse::Array(vec![]);
        let Some(server) = self.server_for(path) else {
            return futures::future::ready(Ok(empty())).boxed();
        };
        let params = lsp_types::CompletionParams {
            text_document_position: self.position_params(path, position),
            context: Some(context),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        async move {
            let client = server.await.map_err(|err| anyhow!("{err}"))?;
            let response = client
                .request::<lsp_types::request::Completion>(params)
                .await?;
            Ok(response.unwrap_or_else(empty))
        }
        .boxed()
    }

    pub fn hover(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>>> {
        let Some(server) = self.server_for(path) else {
            return futures::future::ready(Ok(None)).boxed();
        };
        let params = lsp_types::HoverParams {
            text_document_position_params: self.position_params(path, position),
            work_done_progress_params: Default::default(),
        };
        async move {
            let client = server.await.map_err(|err| anyhow!("{err}"))?;
            client
                .request::<lsp_types::request::HoverRequest>(params)
                .await
        }
        .boxed()
    }

    pub fn definition(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Vec<lsp_types::LocationLink>>> {
        let Some(server) = self.server_for(path) else {
            return futures::future::ready(Ok(vec![])).boxed();
        };
        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params: self.position_params(path, position),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        async move {
            let client = server.await.map_err(|err| anyhow!("{err}"))?;
            let response = client
                .request::<lsp_types::request::GotoDefinition>(params)
                .await?;
            Ok(match response {
                None => vec![],
                Some(lsp_types::GotoDefinitionResponse::Link(links)) => links,
                Some(lsp_types::GotoDefinitionResponse::Scalar(location)) => {
                    vec![location_to_link(location)]
                }
                Some(lsp_types::GotoDefinitionResponse::Array(locations)) => {
                    locations.into_iter().map(location_to_link).collect()
                }
            })
        }
        .boxed()
    }

    fn position_params(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> lsp_types::TextDocumentPositionParams {
        lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: path_to_uri(path),
            },
            position,
        }
    }

    fn server_for(&self, path: &Path) -> Option<ServerFuture> {
        Some(self.state.lock().unwrap().documents.get(path)?.server.clone())
    }

    fn started_client(&self, path: &Path) -> Option<Arc<LspClient>> {
        self.server_for(path)?.peek()?.as_ref().ok().cloned()
    }

    fn ensure_server(
        servers: &mut HashMap<(&'static str, PathBuf), ServerFuture>,
        spec: &'static ServerSpec,
        root: PathBuf,
        diagnostics: super::client::DiagnosticsSender,
    ) -> ServerFuture {
        servers
            .entry((spec.id, root.clone()))
            .or_insert_with(|| {
                smol::spawn(async move {
                    LspClient::start(spec.command, spec.args, &root, diagnostics)
                        .await
                        .map(Arc::new)
                        .map_err(|err| {
                            eprintln!("ide: lsp: {}: {err:#}", spec.id);
                            Arc::<str>::from(err.to_string())
                        })
                })
                .shared()
            })
            .clone()
    }

    fn detect_root(&self, spec: &ServerSpec, file: &Path) -> PathBuf {
        let bound = self.workspace_root.as_path();
        let ancestors = file
            .parent()
            .into_iter()
            .flat_map(|dir| dir.ancestors())
            .take_while(|dir| dir.starts_with(bound));

        match spec.root {
            RootRule::CargoWorkspace => {
                let mut nearest_manifest = None;
                for dir in ancestors {
                    let manifest = dir.join("Cargo.toml");
                    if manifest.is_file() {
                        nearest_manifest.get_or_insert_with(|| dir.to_owned());
                        if std::fs::read_to_string(&manifest)
                            .is_ok_and(|text| text.contains("[workspace]"))
                        {
                            return dir.to_owned();
                        }
                    }
                }
                nearest_manifest.unwrap_or_else(|| bound.to_owned())
            }
            RootRule::Markers(markers) => {
                for dir in ancestors {
                    if markers.iter().any(|marker| dir.join(marker).is_file()) {
                        return dir.to_owned();
                    }
                }
                bound.to_owned()
            }
        }
    }
}

async fn document_worker(
    server: ServerFuture,
    uri: lsp_types::Uri,
    language_id: &'static str,
    text: String,
    mut ops: mpsc::UnboundedReceiver<DocOp>,
) {
    // Server failed to start: ops drain into the void; the failure was
    // logged once by the start task.
    let Ok(client) = server.await else { return };

    let _ = client.notify::<notif::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: lsp_types::TextDocumentItem {
            uri: uri.clone(),
            language_id: language_id.to_string(),
            version: 0,
            text,
        },
    });

    while let Some(op) = ops.next().await {
        match op {
            DocOp::Change { version, text } => {
                let _ = client.notify::<notif::DidChangeTextDocument>(
                    lsp_types::DidChangeTextDocumentParams {
                        text_document: lsp_types::VersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version,
                        },
                        content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                            range: None,
                            range_length: None,
                            text,
                        }],
                    },
                );
            }
            DocOp::Save { text } => {
                let _ = client.notify::<notif::DidSaveTextDocument>(
                    lsp_types::DidSaveTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        text: Some(text),
                    },
                );
            }
            DocOp::Close => {
                let _ = client.notify::<notif::DidCloseTextDocument>(
                    lsp_types::DidCloseTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    },
                );
                break;
            }
        }
    }
}

fn location_to_link(location: lsp_types::Location) -> lsp_types::LocationLink {
    lsp_types::LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: location.range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_routing_covers_the_fleet() {
        for (ext, id) in [
            ("rs", "rust-analyzer"),
            ("rb", "ruby-lsp"),
            ("erb", "ruby-lsp"),
            ("go", "gopls"),
            ("py", "basedpyright"),
            ("ts", "vtsls"),
            ("tsx", "vtsls"),
        ] {
            let (spec, _) = spec_for_extension(ext).expect(ext);
            assert_eq!(spec.id, id, "extension {ext}");
        }
        assert!(spec_for_extension("md").is_none());
    }
}
