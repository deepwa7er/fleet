//! The document store: the workspace's authoritative copy of every open
//! document, and the auto-save engine (docs/remote.md §6). The UI streams
//! full text on every edit; the store persists after an idle debounce, on
//! explicit save, on close, and on quit. "Unsaved changes" is not a concept
//! the UI carries anymore — at most the debounce window is unflushed.
//!
//! Concurrency model: versions only. Every edit bumps `version`; a flush
//! captures `(version, text)`, writes, then records `flushed = max(flushed,
//! version)`. A stale flush (raced by a newer edit) simply leaves the
//! document dirty for the newer debounce task it can no longer cancel.
//! A failed write keeps the document dirty; the next edit or close retries.
//! gpui-free on purpose — this runs inside ide-server in slice 5c.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::channel::mpsc;

const FLUSH_DEBOUNCE: Duration = Duration::from_millis(500);

struct DocState {
    text: String,
    version: u64,
    flushed: u64,
    /// Replacing this drops (= cancels) the previous debounce timer.
    flush_task: Option<smol::Task<()>>,
}

impl DocState {
    fn dirty(&self) -> bool {
        self.version != self.flushed
    }
}

struct StoreState {
    docs: HashMap<PathBuf, DocState>,
    /// Last emitted all-synced value, to emit only transitions.
    last_synced: bool,
}

pub struct DocumentStore {
    state: Mutex<StoreState>,
    sync_tx: mpsc::UnboundedSender<bool>,
    sync_rx: Mutex<Option<mpsc::UnboundedReceiver<bool>>>,
}

impl DocumentStore {
    pub fn new() -> Arc<Self> {
        let (sync_tx, sync_rx) = mpsc::unbounded();
        Arc::new(Self {
            state: Mutex::new(StoreState {
                docs: HashMap::new(),
                last_synced: true,
            }),
            sync_tx,
            sync_rx: Mutex::new(Some(sync_rx)),
        })
    }

    /// The all-synced stream (true = nothing unflushed), taken exactly once.
    pub fn take_sync_state(&self) -> Option<mpsc::UnboundedReceiver<bool>> {
        self.sync_rx.lock().unwrap().take()
    }

    pub fn open(&self, path: &Path, text: String) {
        let mut state = self.state.lock().unwrap();
        state.docs.insert(
            path.to_owned(),
            DocState {
                text,
                version: 0,
                flushed: 0,
                flush_task: None,
            },
        );
    }

    pub fn changed(self: &Arc<Self>, path: &Path, text: String) {
        let mut state = self.state.lock().unwrap();
        let Some(doc) = state.docs.get_mut(path) else {
            return;
        };
        doc.text = text;
        doc.version += 1;
        let store = self.clone();
        let debounced = path.to_owned();
        doc.flush_task = Some(smol::spawn(async move {
            smol::Timer::after(FLUSH_DEBOUNCE).await;
            if let Err(err) = store.flush(&debounced).await {
                // Still dirty; the next edit or close retries.
                eprintln!("ide: auto-save failed: {err:#}");
            }
        }));
        self.emit_sync(&mut state);
    }

    /// Explicit save (ctrl-s): adopt `text` and flush immediately, reporting
    /// the write result to the caller.
    pub async fn save_now(self: &Arc<Self>, path: &Path, text: String) -> Result<()> {
        let known = {
            let mut state = self.state.lock().unwrap();
            match state.docs.get_mut(path) {
                Some(doc) => {
                    doc.text = text.clone();
                    doc.version += 1;
                    doc.flush_task = None; // cancel any pending debounce
                    self.emit_sync(&mut state);
                    true
                }
                None => false,
            }
        };
        if known {
            self.flush(path).await
        } else {
            // Not an open document (shouldn't happen): plain write.
            write_text(path, &text).await
        }
    }

    /// Flush-if-dirty then forget the document.
    pub async fn close(self: &Arc<Self>, path: &Path) {
        let flush_needed = {
            let state = self.state.lock().unwrap();
            state.docs.get(path).is_some_and(DocState::dirty)
        };
        if flush_needed && let Err(err) = self.flush(path).await {
            eprintln!("ide: flush on close failed: {err:#}");
        }
        let mut state = self.state.lock().unwrap();
        state.docs.remove(path);
        self.emit_sync(&mut state);
    }

    /// Flush every dirty document (app quit).
    pub async fn flush_all(self: &Arc<Self>) -> Result<()> {
        let dirty: Vec<PathBuf> = {
            let state = self.state.lock().unwrap();
            state
                .docs
                .iter()
                .filter(|(_, doc)| doc.dirty())
                .map(|(path, _)| path.clone())
                .collect()
        };
        for path in dirty {
            self.flush(&path).await?;
        }
        Ok(())
    }

    async fn flush(self: &Arc<Self>, path: &Path) -> Result<()> {
        let Some((version, text)) = ({
            let state = self.state.lock().unwrap();
            state
                .docs
                .get(path)
                .filter(|doc| doc.dirty())
                .map(|doc| (doc.version, doc.text.clone()))
        }) else {
            return Ok(()); // already clean, or closed meanwhile
        };

        let result = write_text(path, &text).await;

        let mut state = self.state.lock().unwrap();
        if result.is_ok()
            && let Some(doc) = state.docs.get_mut(path)
        {
            doc.flushed = doc.flushed.max(version);
        }
        self.emit_sync(&mut state);
        result
    }

    fn emit_sync(&self, state: &mut StoreState) {
        let synced = state.docs.values().all(|doc| !doc.dirty());
        if synced != state.last_synced {
            state.last_synced = synced;
            let _ = self.sync_tx.unbounded_send(synced);
        }
    }
}

async fn write_text(path: &Path, text: &str) -> Result<()> {
    let path = path.to_owned();
    let text = text.to_owned();
    blocking::unblock(move || {
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn scratch_file(dir: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn debounced_edit_reaches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch_file(&dir, "a.txt", "old");
        let store = DocumentStore::new();
        store.open(&path, "old".into());
        store.changed(&path, "new".into());
        // Well past the debounce; the spawned flush runs on smol's executor.
        block_on(smol::Timer::after(FLUSH_DEBOUNCE * 3));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn rapid_edits_flush_only_the_latest(){
        let dir = tempfile::tempdir().unwrap();
        let path = scratch_file(&dir, "a.txt", "v0");
        let store = DocumentStore::new();
        store.open(&path, "v0".into());
        for i in 1..=5 {
            store.changed(&path, format!("v{i}"));
        }
        block_on(smol::Timer::after(FLUSH_DEBOUNCE * 3));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v5");
    }

    #[test]
    fn save_now_is_immediate_and_close_flushes() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch_file(&dir, "a.txt", "old");
        let store = DocumentStore::new();
        store.open(&path, "old".into());

        block_on(store.save_now(&path, "saved".into())).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved");

        store.changed(&path, "closing".into());
        block_on(store.close(&path)); // no debounce wait: close flushes
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "closing");
    }

    #[test]
    fn sync_state_emits_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratch_file(&dir, "a.txt", "old");
        let store = DocumentStore::new();
        let mut sync = store.take_sync_state().unwrap();
        assert!(store.take_sync_state().is_none(), "single subscriber");

        store.open(&path, "old".into());
        store.changed(&path, "new".into());
        assert_eq!(block_on(futures::StreamExt::next(&mut sync)), Some(false));
        assert_eq!(block_on(futures::StreamExt::next(&mut sync)), Some(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }
}
