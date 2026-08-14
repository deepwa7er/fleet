//! Language intelligence: one `LspStore` owns every language-server process,
//! routes documents to the right server by language and workspace root, and
//! bridges gpui-component's editor provider traits onto real LSP requests.
//!
//! Boundary note: this subsystem talks to local processes directly — language
//! servers must run next to the code, so in milestone 5 this whole module
//! moves behind the `WorkspaceService` seam and the UI-side traits stay as
//! they are. It is deliberately self-contained for that move.

mod client;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr as _;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use futures::future::Shared;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, BackgroundExecutor, Context, Entity, EventEmitter, Task, Window};
use gpui_component::input::{
    CompletionProvider, DefinitionProvider, HoverProvider, InputBaseState, Rope, RopeExt as _,
};

use client::{DiagnosticsSender, LspClient};

/// A starting-or-started server, shareable across documents. The error is a
/// pre-formatted message so the result stays `Clone`.
type ServerFuture = Shared<Task<Result<Arc<LspClient>, Arc<str>>>>;

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

pub enum LspEvent {
    Diagnostics {
        uri: lsp_types::Uri,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
}

struct Document {
    uri: lsp_types::Uri,
    server: ServerFuture,
    version: i32,
    /// Sequential outbox: every op chains on the previous one, so didOpen /
    /// didChange / didSave / didClose reach the server in order even though
    /// each is spawned independently.
    chain: Task<()>,
}

pub struct LspStore {
    workspace_root: PathBuf,
    executor: BackgroundExecutor,
    diagnostics_tx: DiagnosticsSender,
    servers: HashMap<(&'static str, PathBuf), ServerFuture>,
    documents: HashMap<String, Document>,
    _diagnostics_task: Task<()>,
}

impl EventEmitter<LspEvent> for LspStore {}

impl LspStore {
    pub fn new(workspace_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (diagnostics_tx, mut diagnostics_rx) =
            mpsc::unbounded::<(lsp_types::Uri, Vec<lsp_types::Diagnostic>)>();

        let diagnostics_task = cx.spawn(async move |this, cx| {
            while let Some((uri, diagnostics)) = diagnostics_rx.next().await {
                let alive = this.update(cx, |_, cx| {
                    cx.emit(LspEvent::Diagnostics { uri, diagnostics });
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        Self {
            workspace_root,
            executor: cx.background_executor().clone(),
            diagnostics_tx,
            servers: HashMap::new(),
            documents: HashMap::new(),
            _diagnostics_task: diagnostics_task,
        }
    }

    /// Route `path` to a language server if one is configured for it. Sends
    /// didOpen and returns the provider bridge to install on the editor.
    /// `None` means "no LSP for this file" — a missing binary also lands here
    /// at await time, with one log line from the start task.
    pub fn attach(&mut self, path: &Path, text: String, cx: &mut Context<Self>) -> Option<Rc<EditorLsp>> {
        let extension = path.extension()?.to_str()?;
        let (spec, language_id) = spec_for_extension(extension)?;
        let root = self.detect_root(spec, path);
        let server = self.ensure_server(spec, root);

        let uri = path_to_uri(path);
        let key = uri.to_string();
        self.documents.insert(
            key.clone(),
            Document {
                uri: uri.clone(),
                server: server.clone(),
                version: 0,
                chain: Task::ready(()),
            },
        );

        let open_uri = uri.clone();
        self.enqueue(&key, cx, async move {
            let Ok(client) = server.await else { return };
            let _ = client.notify::<lsp_types::notification::DidOpenTextDocument>(
                lsp_types::DidOpenTextDocumentParams {
                    text_document: lsp_types::TextDocumentItem {
                        uri: open_uri,
                        language_id: language_id.to_string(),
                        version: 0,
                        text,
                    },
                },
            );
        });

        Some(Rc::new(EditorLsp {
            store: cx.entity(),
            uri,
            key,
        }))
    }

    pub fn document_changed(&mut self, path: &Path, text: String, cx: &mut Context<Self>) {
        let key = path_to_uri(path).to_string();
        let Some(doc) = self.documents.get_mut(&key) else {
            return;
        };
        doc.version += 1;
        let (server, uri, version) = (doc.server.clone(), doc.uri.clone(), doc.version);
        self.enqueue(&key, cx, async move {
            let Ok(client) = server.await else { return };
            let _ = client.notify::<lsp_types::notification::DidChangeTextDocument>(
                lsp_types::DidChangeTextDocumentParams {
                    text_document: lsp_types::VersionedTextDocumentIdentifier { uri, version },
                    content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text,
                    }],
                },
            );
        });
    }

    pub fn document_saved(&mut self, path: &Path, text: String, cx: &mut Context<Self>) {
        let key = path_to_uri(path).to_string();
        let Some(doc) = self.documents.get(&key) else {
            return;
        };
        let (server, uri) = (doc.server.clone(), doc.uri.clone());
        self.enqueue(&key, cx, async move {
            let Ok(client) = server.await else { return };
            let _ = client.notify::<lsp_types::notification::DidSaveTextDocument>(
                lsp_types::DidSaveTextDocumentParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri },
                    text: Some(text),
                },
            );
        });
    }

    pub fn document_closed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let key = path_to_uri(path).to_string();
        let Some(mut doc) = self.documents.remove(&key) else {
            return;
        };
        let prev = std::mem::replace(&mut doc.chain, Task::ready(()));
        let (server, uri) = (doc.server.clone(), doc.uri.clone());
        // The document entry is gone; run the tail of its outbox detached so
        // didClose still reaches the server.
        cx.spawn(async move |_, _| {
            prev.await;
            let Ok(client) = server.await else { return };
            let _ = client.notify::<lsp_types::notification::DidCloseTextDocument>(
                lsp_types::DidCloseTextDocumentParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri },
                },
            );
        })
        .detach();
    }

    fn enqueue(&mut self, key: &str, cx: &mut Context<Self>, op: impl Future<Output = ()> + 'static) {
        let Some(doc) = self.documents.get_mut(key) else {
            return;
        };
        let prev = std::mem::replace(&mut doc.chain, Task::ready(()));
        doc.chain = cx.spawn(async move |_, _| {
            prev.await;
            op.await;
        });
    }

    fn ensure_server(&mut self, spec: &'static ServerSpec, root: PathBuf) -> ServerFuture {
        let executor = self.executor.clone();
        let diagnostics_tx = self.diagnostics_tx.clone();
        self.servers
            .entry((spec.id, root.clone()))
            .or_insert_with(|| {
                executor
                    .clone()
                    .spawn(async move {
                        LspClient::start(spec.command, spec.args, &root, executor, diagnostics_tx)
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

    fn server_for(&self, key: &str) -> Option<ServerFuture> {
        Some(self.documents.get(key)?.server.clone())
    }
}

/// The per-document provider bridge installed on an editor's `lsp` slot.
pub struct EditorLsp {
    store: Entity<LspStore>,
    uri: lsp_types::Uri,
    key: String,
}

impl EditorLsp {
    fn server(&self, cx: &App) -> Option<ServerFuture> {
        self.store.read(cx).server_for(&self.key)
    }

    fn text_document_position(
        &self,
        rope: &Rope,
        offset: usize,
    ) -> lsp_types::TextDocumentPositionParams {
        lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: self.uri.clone(),
            },
            position: rope.offset_to_position(offset),
        }
    }
}

impl CompletionProvider for EditorLsp {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        cx: &mut Context<InputBaseState>,
    ) -> Task<Result<lsp_types::CompletionResponse>> {
        let empty = || lsp_types::CompletionResponse::Array(vec![]);
        let Some(server) = self.server(cx) else {
            return Task::ready(Ok(empty()));
        };
        let params = lsp_types::CompletionParams {
            text_document_position: self.text_document_position(rope, offset),
            context: Some(trigger),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        cx.background_executor().spawn(async move {
            let client = server.await.map_err(|err| anyhow!("{err}"))?;
            let response = client
                .request::<lsp_types::request::Completion>(params)
                .await?;
            Ok(response.unwrap_or_else(empty))
        })
    }

    fn inline_completion(
        &self,
        _rope: &Rope,
        _offset: usize,
        _trigger: lsp_types::InlineCompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputBaseState>,
    ) -> Task<Result<lsp_types::InlineCompletionResponse>> {
        Task::ready(Ok(lsp_types::InlineCompletionResponse::Array(vec![])))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        cx: &mut Context<InputBaseState>,
    ) -> bool {
        let Some(last) = new_text.chars().last() else {
            return false;
        };
        if last.is_alphanumeric() || last == '_' {
            return true;
        }
        // Ask the server's declared trigger characters once it is up.
        self.server(cx)
            .as_ref()
            .and_then(|server| server.peek())
            .and_then(|started| started.as_ref().ok())
            .and_then(|client| client.capabilities().completion_provider.as_ref())
            .and_then(|provider| provider.trigger_characters.as_ref())
            .is_some_and(|triggers| triggers.iter().any(|t| t.as_str() == last.to_string()))
    }
}

impl HoverProvider for EditorLsp {
    fn hover(
        &self,
        rope: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let Some(server) = self.server(cx) else {
            return Task::ready(Ok(None));
        };
        let params = lsp_types::HoverParams {
            text_document_position_params: self.text_document_position(rope, offset),
            work_done_progress_params: Default::default(),
        };
        cx.background_executor().spawn(async move {
            let client = server.await.map_err(|err| anyhow!("{err}"))?;
            client
                .request::<lsp_types::request::HoverRequest>(params)
                .await
        })
    }
}

impl DefinitionProvider for EditorLsp {
    fn definitions(
        &self,
        rope: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<lsp_types::LocationLink>>> {
        let Some(server) = self.server(cx) else {
            return Task::ready(Ok(vec![]));
        };
        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params: self.text_document_position(rope, offset),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        cx.background_executor().spawn(async move {
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
        })
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

/// file:// URI from an absolute path, percent-encoding what the RFC requires.
pub fn path_to_uri(path: &Path) -> lsp_types::Uri {
    let mut encoded = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    lsp_types::Uri::from_str(&encoded).expect("percent-encoded absolute path is a valid uri")
}

/// Path from a file:// URI; `None` for other schemes.
pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    if uri.scheme().map(|s| s.as_str()) != Some("file") {
        return None;
    }
    let raw = uri.path().as_str();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut rest = raw.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let hi = rest.next()?;
            let lo = rest.next()?;
            let hex = [hi, lo];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip_plain() {
        let path = Path::new("/home/deepwater/code/fleet/tugboat/src/main.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri.to_string(), "file:///home/deepwater/code/fleet/tugboat/src/main.rs");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn uri_round_trip_special_chars() {
        let path = Path::new("/tmp/with space/héllo.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn non_file_uri_is_not_a_path() {
        let uri = lsp_types::Uri::from_str("https://doc.rust-lang.org/std/").unwrap();
        assert_eq!(uri_to_path(&uri), None);
    }

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
