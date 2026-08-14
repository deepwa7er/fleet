//! The workspace-service boundary. Every filesystem interaction the UI makes
//! goes through [`WorkspaceService`]; the UI never touches `std::fs`. This is
//! the seam milestone 5 needs: the remote mode (native macOS client talking to
//! a headless ide-server on the desktop over SSH) becomes a second
//! implementation of this trait rather than a rewrite. Search and LSP traffic
//! will join this boundary in later milestones.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use futures::FutureExt as _;
use futures::future::BoxFuture;

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// One full-text search hit.
#[derive(Debug, Clone, PartialEq)]
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

    fn write_file(&self, path: &Path, contents: String) -> BoxFuture<'static, Result<()>>;

    /// Every searchable file, as paths relative to the root — gitignore
    /// respected, hidden files skipped. Feeds the fuzzy file picker.
    fn list_files(&self) -> BoxFuture<'static, Result<Vec<PathBuf>>>;

    /// Literal full-text search (smart-case), at most `limit` hits.
    fn search_text(&self, query: String, limit: usize)
    -> BoxFuture<'static, Result<Vec<TextMatch>>>;
}

pub struct LocalWorkspace {
    root: PathBuf,
}

impl LocalWorkspace {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open workspace root {}", root.display()))?;
        anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
        Ok(Self { root })
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

    fn write_file(&self, path: &Path, contents: String) -> BoxFuture<'static, Result<()>> {
        let path = path.to_owned();
        blocking::unblock(move || {
            std::fs::write(&path, contents)
                .with_context(|| format!("cannot write {}", path.display()))
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
