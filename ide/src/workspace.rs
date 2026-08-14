//! The workspace-service boundary. Every filesystem interaction the UI makes
//! goes through [`WorkspaceService`]; the UI never touches `std::fs`. This is
//! the seam milestone 5 needs: the remote mode (native macOS client talking to
//! a headless ide-server on the desktop over SSH) becomes a second
//! implementation of this trait rather than a rewrite. Search and LSP traffic
//! will join this boundary in later milestones.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use futures::stream::BoxStream;

use crate::documents::DocumentStore;
use crate::lsp::hub::LanguageHub;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Remote-connection lifecycle, for the shell's banner and read-only state.
/// Local workspaces never emit these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectionEvent {
    /// The wire died; editors should go read-only.
    Lost,
    /// An automatic reconnect round started.
    Reconnecting,
    /// A fresh session is up; the shell re-reads open documents from the
    /// workspace (server truth) and resumes editing.
    Restored,
}

/// One full-text search hit.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextMatch {
    /// Relative to the workspace root.
    pub path: PathBuf,
    /// 1-based, as grep tools report it.
    pub line: u32,
    /// The matched line, trimmed.
    pub text: String,
}

/// Async and dyn-compatible (`BoxFuture` rather than `async fn`) so the UI can
/// hold an `Arc<dyn WorkspaceService>` and futures can run on gpui's
/// background executor.
pub trait WorkspaceService: Send + Sync {
    fn root(&self) -> &Path;

    /// Entries of one directory, directories first, then case-insensitive by
    /// name. Returns everything present — display policy (hiding `.git`,
    /// build dirs) belongs to the UI, not the service.
    fn read_dir(&self, path: &Path) -> BoxFuture<'static, Result<Vec<DirEntry>>>;

    fn read_file(&self, path: &Path) -> BoxFuture<'static, Result<String>>;

    /// Every searchable file, as paths relative to the root — gitignore
    /// respected, hidden files skipped. Feeds the fuzzy file picker.
    fn list_files(&self) -> BoxFuture<'static, Result<Vec<PathBuf>>>;

    /// Literal full-text search (smart-case), at most `limit` hits.
    fn search_text(&self, query: String, limit: usize)
    -> BoxFuture<'static, Result<Vec<TextMatch>>>;

    // ── Documents & language intelligence (docs/remote.md §4–§5) ─────────
    //
    // The document pipeline is the seam's second half: locally it feeds the
    // in-process language hub; remotely the same calls become the wire. In
    // slice 5b it also becomes the persistence path (server-side auto-save).

    /// A document is now open with `text`. No-op for unconfigured languages.
    fn document_open(&self, path: &Path, text: String);

    /// The full current text after an edit (ordered per document).
    fn document_changed(&self, path: &Path, text: String);

    /// Explicit save (ctrl-s): adopt `text`, flush to disk immediately, then
    /// didSave to the language server. Auto-save persists edits regardless;
    /// this is the user-driven "now, and tell the tools" variant.
    fn document_save(&self, path: &Path, text: String) -> BoxFuture<'static, Result<()>>;

    /// The document closed; the language server hears didClose.
    fn document_closed(&self, path: &Path);

    fn completion(
        &self,
        path: &Path,
        position: lsp_types::Position,
        context: lsp_types::CompletionContext,
    ) -> BoxFuture<'static, Result<lsp_types::CompletionResponse>>;

    fn hover(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>>>;

    fn definition(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Vec<lsp_types::LocationLink>>>;

    /// The completion trigger characters the server declared, empty until it
    /// is up (or for unconfigured languages).
    fn completion_triggers(&self, path: &Path) -> Vec<String>;

    /// Diagnostics for open documents, keyed by path. Single-subscriber: the
    /// shell takes this once at startup; later calls return `None`.
    fn subscribe_diagnostics(
        &self,
    ) -> Option<BoxStream<'static, (PathBuf, Vec<lsp_types::Diagnostic>)>>;

    /// Auto-save sync state: emits `true` when every open document is
    /// flushed, `false` when something is pending — transitions only.
    /// Single-subscriber, like diagnostics.
    fn subscribe_sync_state(&self) -> Option<BoxStream<'static, bool>>;

    /// Connection lifecycle (docs/remote.md §6). Local workspaces have no
    /// connection: the default is no stream.
    fn subscribe_connection(&self) -> Option<BoxStream<'static, ConnectionEvent>> {
        None
    }

    /// Manual reconnect (the banner's action). No-op locally.
    fn reconnect(&self) {}

    /// Flush every dirty document — the app-quit hook.
    fn flush_all(&self) -> BoxFuture<'static, Result<()>>;
}

pub struct LocalWorkspace {
    root: PathBuf,
    hub: Arc<LanguageHub>,
    docs: Arc<DocumentStore>,
}

impl LocalWorkspace {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open workspace root {}", root.display()))?;
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        let hub = Arc::new(LanguageHub::new(root.clone()));
        let docs = DocumentStore::new();
        Ok(Self { root, hub, docs })
    }

    /// Await-until-known trigger characters — ide-server pushes these to the
    /// remote client, whose sync `completion_triggers` reads a cache.
    pub fn completion_triggers_ready(&self, path: &Path) -> BoxFuture<'static, Vec<String>> {
        self.hub.completion_triggers_ready(path)
    }
}

impl WorkspaceService for LocalWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn read_dir(&self, path: &Path) -> BoxFuture<'static, Result<Vec<DirEntry>>> {
        let path = path.to_owned();
        // blocking::unblock moves the sync fs call onto a thread pool so the
        // executor thread driving this future is never blocked.
        blocking::unblock(move || {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path)
                .with_context(|| format!("cannot read directory {}", path.display()))?
            {
                let entry = entry?;
                let Ok(name) = entry.file_name().into_string() else {
                    continue; // non-UTF-8 names have no display story yet
                };
                let is_dir = entry.file_type()?.is_dir();
                entries.push(DirEntry {
                    path: entry.path(),
                    name,
                    is_dir,
                });
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(entries)
        })
        .boxed()
    }

    fn read_file(&self, path: &Path) -> BoxFuture<'static, Result<String>> {
        let path = path.to_owned();
        blocking::unblock(move || {
            std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))
        })
        .boxed()
    }

    fn list_files(&self) -> BoxFuture<'static, Result<Vec<PathBuf>>> {
        let root = self.root.clone();
        blocking::unblock(move || {
            let mut files = Vec::new();
            // require_git(false): honor .gitignore even before `git init`.
            for entry in ignore::WalkBuilder::new(&root).require_git(false).build() {
                let entry = entry?;
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && let Ok(rel) = entry.path().strip_prefix(&root)
                {
                    files.push(rel.to_owned());
                }
            }
            files.sort();
            Ok(files)
        })
        .boxed()
    }

    fn search_text(
        &self,
        query: String,
        limit: usize,
    ) -> BoxFuture<'static, Result<Vec<TextMatch>>> {
        let root = self.root.clone();
        blocking::unblock(move || {
            let matcher = grep_regex::RegexMatcherBuilder::new()
                .case_smart(true)
                .fixed_strings(true)
                .build(&query)
                .context("cannot build search pattern")?;
            let mut searcher = grep_searcher::SearcherBuilder::new()
                .binary_detection(grep_searcher::BinaryDetection::quit(0))
                .build();

            let mut matches = Vec::new();
            for entry in ignore::WalkBuilder::new(&root).require_git(false).build() {
                if matches.len() >= limit {
                    break;
                }
                let entry = entry?;
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let Ok(rel) = entry.path().strip_prefix(&root).map(Path::to_owned) else {
                    continue;
                };
                let sink = grep_searcher::sinks::UTF8(|line, text| {
                    matches.push(TextMatch {
                        path: rel.clone(),
                        line: line as u32,
                        text: text.trim().to_string(),
                    });
                    // Stop this file once the global limit is reached.
                    Ok(matches.len() < limit)
                });
                let _ = searcher.search_path(&matcher, entry.path(), sink);
            }
            matches.truncate(limit);
            Ok(matches)
        })
        .boxed()
    }

    fn document_open(&self, path: &Path, text: String) {
        self.docs.open(path, text.clone());
        self.hub.document_open(path, text);
    }

    fn document_changed(&self, path: &Path, text: String) {
        self.docs.changed(path, text.clone());
        self.hub.document_changed(path, text);
    }

    fn document_save(&self, path: &Path, text: String) -> BoxFuture<'static, Result<()>> {
        let docs = self.docs.clone();
        let hub = self.hub.clone();
        let path = path.to_owned();
        async move {
            docs.save_now(&path, text.clone()).await?;
            hub.document_saved(&path, text);
            Ok(())
        }
        .boxed()
    }

    fn document_closed(&self, path: &Path) {
        self.hub.document_closed(path);
        let docs = self.docs.clone();
        let path = path.to_owned();
        // Final flush-if-dirty happens off-thread; failures are logged there.
        smol::spawn(async move { docs.close(&path).await }).detach();
    }

    fn completion(
        &self,
        path: &Path,
        position: lsp_types::Position,
        context: lsp_types::CompletionContext,
    ) -> BoxFuture<'static, Result<lsp_types::CompletionResponse>> {
        self.hub.completion(path, position, context)
    }

    fn hover(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>>> {
        self.hub.hover(path, position)
    }

    fn definition(
        &self,
        path: &Path,
        position: lsp_types::Position,
    ) -> BoxFuture<'static, Result<Vec<lsp_types::LocationLink>>> {
        self.hub.definition(path, position)
    }

    fn completion_triggers(&self, path: &Path) -> Vec<String> {
        self.hub.completion_triggers(path)
    }

    fn subscribe_diagnostics(
        &self,
    ) -> Option<BoxStream<'static, (PathBuf, Vec<lsp_types::Diagnostic>)>> {
        Some(futures::StreamExt::boxed(self.hub.take_diagnostics()?))
    }

    fn subscribe_sync_state(&self) -> Option<BoxStream<'static, bool>> {
        Some(futures::StreamExt::boxed(self.docs.take_sync_state()?))
    }

    fn flush_all(&self) -> BoxFuture<'static, Result<()>> {
        let docs = self.docs.clone();
        async move { docs.flush_all().await }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_workspace() -> (tempfile::TempDir, LocalWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/keel.rs"), "fn keel() {}\nlet ballast = 7;\n")
            .unwrap();
        std::fs::write(dir.path().join("notes.md"), "the ballast shifted\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "ballast ballast\n").unwrap();
        let ws = LocalWorkspace::new(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn list_files_respects_gitignore() {
        let (_dir, ws) = scratch_workspace();
        let files = futures::executor::block_on(ws.list_files()).unwrap();
        assert!(files.contains(&PathBuf::from("src/keel.rs")));
        assert!(files.contains(&PathBuf::from("notes.md")));
        assert!(!files.iter().any(|p| p.ends_with("ignored.log")));
    }

    #[test]
    fn search_text_finds_lines_and_respects_limit() {
        let (_dir, ws) = scratch_workspace();
        let hits = futures::executor::block_on(ws.search_text("ballast".into(), 10)).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(
            hits.iter()
                .any(|h| h.path == Path::new("src/keel.rs") && h.line == 2)
        );
        assert!(
            hits.iter()
                .any(|h| h.path == Path::new("notes.md") && h.line == 1)
        );

        let capped = futures::executor::block_on(ws.search_text("ballast".into(), 1)).unwrap();
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn search_text_is_literal_not_regex() {
        let (_dir, ws) = scratch_workspace();
        let hits = futures::executor::block_on(ws.search_text("keel()".into(), 10)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "fn keel() {}");
    }
}
