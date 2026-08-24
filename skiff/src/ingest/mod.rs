//! Ingest: harness sources → the derived read model (DW-004 §4).
//!
//! One adapter per external truth. An adapter's whole job is to turn its
//! native format into domain records; it never touches HTTP, SQL, or views.
//! This module owns the loop around them — watching, watermarking, persisting,
//! and announcing what changed.
//!
//! **A missing source degrades, it never kills.** A harness whose session
//! directory is absent becomes a named error on that source, surfaced to the
//! client. It is never a dead service and never a silently short session list.
//!
//! **Restart is not destructive.** Everything ingested here re-derives from
//! files, so skiffd restarting loses nothing durable — the next scan converges.

pub mod pi;
pub mod pi_map;
mod tail;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::model::{Harness, SessionKey, SourceHealth};
use crate::store::{SessionIngest, Store};

/// What changed. Subscriptions declare the topics they care about; an
/// invalidated subscription recomputes and re-sends (DW-004 §6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Topic {
    /// A session was added, removed, or its summary changed.
    SessionList,
    /// A specific session's entries changed. Rare: pi persists a message
    /// once per reply.
    Session(SessionKey),
    /// A session's *live* state changed — the in-flight reply grew, a run
    /// started or settled. Frequent, and deliberately separate: this path
    /// touches no SQLite and carries only the live message, where
    /// `Session` costs a whole transcript.
    Run(SessionKey),
    /// A source's reachability changed.
    SourceHealth,
}

/// How long a burst of filesystem events is allowed to settle before a scan.
/// A live harness writes many lines per second; coalescing them into one pass
/// is the difference between a scan per line and a scan per burst.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// A floor scan, run even when the watcher reports nothing.
///
/// This is a safety net, not the mechanism — inotify cannot watch a directory
/// that does not exist yet, silently drops events under queue overflow, and
/// does not work at all on some filesystems. Without a floor, any of those
/// turns into a session list that is quietly and permanently stale.
const FLOOR_SCAN: Duration = Duration::from_secs(15);

pub struct Ingest {
    store: Store,
    pi_dir: PathBuf,
    topics: broadcast::Sender<Topic>,
}

impl Ingest {
    pub fn new(store: Store, pi_dir: PathBuf, topics: broadcast::Sender<Topic>) -> Self {
        Self { store, pi_dir, topics }
    }

    /// Run the ingest loop on its own thread until the process ends.
    ///
    /// A dedicated thread rather than a tokio task: every step here is
    /// blocking (SQLite, `read_dir`, file reads), and the loop's natural shape
    /// is "block on a channel, then work". Nothing is gained by making it
    /// async, and the sync/async boundary would have to be crossed twice per
    /// event.
    pub fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("skiff-ingest".to_owned())
            .spawn(move || self.pump())
            .expect("spawning the ingest thread")
    }

    fn pump(self) {
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |event| {
            // A send failure means the pump is gone; nothing to do but drop it.
            let _ = events_tx.send(event);
        }) {
            Ok(watcher) => Some(watcher),
            Err(err) => {
                tracing::error!(%err, "no filesystem watcher; falling back to the floor scan");
                None
            }
        };
        let mut watching = false;

        loop {
            if let Some(watcher) = watcher.as_mut()
                && !watching
                && let Err(err) = watcher.watch(&self.pi_dir, RecursiveMode::Recursive)
            {
                // Almost always "the directory does not exist yet". The floor
                // scan keeps trying, and `scan` records the reason for the
                // client either way.
                tracing::debug!(dir = %self.pi_dir.display(), %err, "watch not established yet");
            } else if watcher.is_some() {
                watching = true;
            }

            if let Err(err) = self.scan() {
                tracing::error!(%err, "ingest scan failed");
            }

            // Block until something happens or the floor expires, then let the
            // burst settle before scanning again.
            match events_rx.recv_timeout(FLOOR_SCAN) {
                Ok(_) => while events_rx.recv_timeout(DEBOUNCE).is_ok() {},
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // The watcher was dropped or died. The floor scan is still
                    // a correct, if slower, ingest.
                    watching = false;
                    watcher = None;
                    std::thread::sleep(FLOOR_SCAN);
                }
            }
        }
    }

    /// One full pass over every source. Announces what changed.
    pub fn scan(&self) -> Result<()> {
        let mut changed = HashSet::new();
        let outcome = self.scan_pi(&mut changed);

        let error = match outcome {
            Ok(()) => None,
            Err(err) => {
                // The error is data, not a crash: it reaches the client as a
                // named unhealthy source.
                tracing::warn!(source = pi::SOURCE, %err, "source unreadable");
                Some(format!("{err:#}"))
            }
        };
        let health = SourceHealth { source: pi::SOURCE.to_owned(), error, checked_ms: now_ms() };
        let previous = self
            .store
            .source_health()?
            .into_iter()
            .find(|h| h.source == health.source)
            .map(|h| h.error);
        self.store.set_source_health(&health)?;
        if previous.as_ref() != Some(&health.error) {
            changed.insert(Topic::SourceHealth);
        }

        for topic in changed {
            // No subscribers is the normal state when nobody has the app open.
            let _ = self.topics.send(topic);
        }
        Ok(())
    }

    fn scan_pi(&self, changed: &mut HashSet<Topic>) -> Result<()> {
        if !self.pi_dir.is_dir() {
            anyhow::bail!("session directory {} does not exist", self.pi_dir.display());
        }
        let files = pi::session_files(&self.pi_dir)
            .with_context(|| format!("scanning {}", self.pi_dir.display()))?;

        let mut seen = HashSet::new();
        for file in &files {
            let Some(key) = pi::key_for_file(file) else { continue };
            seen.insert(key.clone());
            match self.ingest_pi_file(&key, file) {
                Ok(true) => {
                    changed.insert(Topic::Session(key));
                    changed.insert(Topic::SessionList);
                }
                Ok(false) => {}
                // One unreadable file must not hide every other session, so
                // this is logged against the file rather than failing the pass.
                Err(err) => tracing::warn!(file = %file.display(), %err, "skipping session file"),
            }
        }

        for stale in self.store.sessions()?.into_iter().filter(|s| s.harness == Harness::Pi) {
            if !seen.contains(&stale.id) {
                self.store.forget_session(&stale.id)?;
                changed.insert(Topic::Session(stale.id));
                changed.insert(Topic::SessionList);
            }
        }
        Ok(())
    }

    /// Read one session file forward from its watermark. Answers whether
    /// anything changed.
    fn ingest_pi_file(&self, key: &SessionKey, path: &Path) -> Result<bool> {
        let cursor = self.store.cursor(pi::SOURCE, &path.to_string_lossy())?;
        let tail = tail::read_forward(path, cursor)?;
        if tail.lines.is_empty() && !tail.restarted {
            return Ok(false);
        }

        // A restarted read means the file is not the one the watermark
        // described. Whatever was derived from the old one is now wrong.
        if tail.restarted {
            self.store.clear_entries(key)?;
        }

        let parsed = pi::parse_lines(&tail.lines, tail.first_line);

        // The header is line 1 only, so a resumed read recovers it from the
        // store rather than going without.
        let header = match parsed.header.clone() {
            Some(header) => Some(header),
            None if tail.restarted => None,
            None => self.store.session_header(key)?,
        };

        // A `.jsonl` file with no session header is not a session — it is some
        // other file that happens to share the extension, or a file pi created
        // but has not written yet. Either way it must not appear in the list.
        // Nothing is persisted, so a header arriving later is picked up whole
        // on the next scan.
        let Some(header) = header else { return Ok(false) };

        // The summary reads the leaf branch, so it needs every entry — not
        // just the ones that arrived in this batch.
        let mut all = if tail.restarted { Vec::new() } else { self.store.entries(key)? };
        all.extend(parsed.entries.iter().cloned());

        let summary = pi::summarize(key, Some(&header), &all);
        self.store.ingest_session(SessionIngest {
            summary: &summary,
            header: parsed.header.as_ref(),
            entries: &parsed.entries,
        })?;
        self.store.set_cursor(pi::SOURCE, &path.to_string_lossy(), tail.cursor)?;
        Ok(true)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        ingest: Ingest,
        store: Store,
        topics: broadcast::Receiver<Topic>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::in_memory().unwrap();
            let (tx, topics) = broadcast::channel(64);
            let ingest = Ingest::new(store.clone(), dir.path().to_path_buf(), tx);
            Self { dir, ingest, store, topics }
        }

        fn write(&self, name: &str, contents: &str) {
            let path = self.dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn append(&self, name: &str, contents: &str) {
            use std::io::Write;
            let path = self.dir.path().join(name);
            let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
        }

        fn drain(&mut self) -> HashSet<Topic> {
            let mut out = HashSet::new();
            while let Ok(topic) = self.topics.try_recv() {
                out.insert(topic);
            }
            out
        }
    }

    const HEADER: &str =
        r#"{"type":"session","cwd":"/home/x","timestamp":"2026-08-23T10:00:00.000Z"}"#;

    #[test]
    fn a_scan_ingests_a_session_and_announces_it() {
        let mut f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n{{\"id\":\"a\"}}\n"));

        f.ingest.scan().unwrap();

        let sessions = f.store.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id.to_string(), "pi:abc");
        assert_eq!(sessions[0].directory.as_deref(), Some("/home/x"));

        let topics = f.drain();
        assert!(topics.contains(&Topic::SessionList));
        assert!(topics.contains(&Topic::Session("pi:abc".parse().unwrap())));
    }

    #[test]
    fn a_second_scan_with_no_writes_announces_nothing() {
        let mut f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n"));
        f.ingest.scan().unwrap();
        f.drain();

        f.ingest.scan().unwrap();
        assert!(f.drain().is_empty(), "an idle scan must not wake every subscriber");
    }

    #[test]
    fn an_appended_entry_is_picked_up_without_re_reading_the_file() {
        let f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n{{\"id\":\"a\"}}\n"));
        f.ingest.scan().unwrap();

        f.append("abc.jsonl", r#"{"id":"b","parentId":"a","type":"session_info","name":"named"}"#);
        f.append("abc.jsonl", "\n");
        f.ingest.scan().unwrap();

        let key: SessionKey = "pi:abc".parse().unwrap();
        assert_eq!(f.store.entries(&key).unwrap().len(), 2);
        // The name is derived from the leaf branch, which spans both reads —
        // and the header, which only the first read saw.
        let sessions = f.store.sessions().unwrap();
        assert_eq!(sessions[0].title.as_deref(), Some("named"));
        assert_eq!(sessions[0].directory.as_deref(), Some("/home/x"));
    }

    #[test]
    fn a_rewritten_file_replaces_its_entries_rather_than_merging() {
        let f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n{{\"id\":\"a\"}}\n{{\"id\":\"b\"}}\n"));
        f.ingest.scan().unwrap();

        f.write("abc.jsonl", &format!("{HEADER}\n{{\"id\":\"z\"}}\n"));
        f.ingest.scan().unwrap();

        let entries = f.store.entries(&"pi:abc".parse().unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "z");
    }

    #[test]
    fn a_deleted_session_file_is_forgotten() {
        let mut f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n"));
        f.ingest.scan().unwrap();
        f.drain();

        std::fs::remove_file(f.dir.path().join("abc.jsonl")).unwrap();
        f.ingest.scan().unwrap();

        assert!(f.store.sessions().unwrap().is_empty());
        assert!(f.drain().contains(&Topic::SessionList));
    }

    #[test]
    fn sessions_are_found_in_nested_per_cwd_directories() {
        let f = Fixture::new();
        f.write("--home-x--/abc.jsonl", &format!("{HEADER}\n"));
        f.ingest.scan().unwrap();
        assert_eq!(f.store.sessions().unwrap().len(), 1);
    }

    #[test]
    fn a_missing_session_directory_is_a_named_error_not_a_failure() {
        let mut f = Fixture::new();
        let missing = f.dir.path().join("nope");
        let (tx, topics) = broadcast::channel(64);
        f.ingest = Ingest::new(f.store.clone(), missing, tx);
        f.topics = topics;

        f.ingest.scan().expect("a missing source must not fail the pass");

        let health = f.store.source_health().unwrap();
        assert_eq!(health.len(), 1);
        assert!(health[0].error.as_ref().unwrap().contains("does not exist"));
        assert!(f.drain().contains(&Topic::SourceHealth));
    }

    #[test]
    fn source_health_announces_only_when_it_actually_changes() {
        let mut f = Fixture::new();
        f.write("abc.jsonl", &format!("{HEADER}\n"));
        f.ingest.scan().unwrap();
        f.drain();
        f.ingest.scan().unwrap();
        assert!(!f.drain().contains(&Topic::SourceHealth));
    }

    #[test]
    fn a_file_that_is_not_a_session_is_skipped_without_hiding_the_others() {
        let f = Fixture::new();
        f.write("good.jsonl", &format!("{HEADER}\n"));
        f.write("notes.txt", "ignore me");
        f.write("empty.jsonl", "");
        f.write("headerless.jsonl", "{\"id\":\"a\"}\n");
        f.ingest.scan().unwrap();

        let ids: Vec<_> =
            f.store.sessions().unwrap().iter().map(|s| s.id.to_string()).collect();
        assert_eq!(ids, ["pi:good"]);
    }
}
