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
}
