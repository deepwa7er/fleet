//! External change-log writes → live-query invalidations.
//!
//! `dw` and Skiff are separate authors of the append-only log. File locking
//! protects writes; this watcher makes a round appended by `dw` visible to an
//! already-open browser without a request, refresh, or client poll.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::ingest::Topic;

const DEBOUNCE: Duration = Duration::from_millis(50);
const FLOOR_SCAN: Duration = Duration::from_secs(15);

pub struct ChangeWatch {
    store: change::Store,
    topics: broadcast::Sender<Topic>,
}

impl ChangeWatch {
    pub fn new(store: change::Store, topics: broadcast::Sender<Topic>) -> Self {
        Self { store, topics }
    }

    pub fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("skiff-changes".to_owned())
            .spawn(move || self.pump())
            .expect("spawning the change watcher")
    }

    fn pump(self) {
        let root = self.store.root().to_path_buf();
        if let Err(error) = std::fs::create_dir_all(&root) {
            tracing::error!(dir = %root.display(), %error, "cannot create change directory");
        }
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events_tx.send(event);
        })
        .ok();
        let watch_failed = watcher.as_mut().is_some_and(|active| {
            active
                .watch(&root, RecursiveMode::Recursive)
                .inspect_err(|error| {
                    tracing::warn!(dir = %root.display(), %error, "change watch unavailable; floor scan remains active");
                })
                .is_err()
        });
        if watch_failed {
            watcher = None;
        }

        self.announce_floor();
        loop {
            match events_rx.recv_timeout(FLOOR_SCAN) {
                Ok(first) => {
                    let mut paths = event_paths(first);
                    while let Ok(event) = events_rx.recv_timeout(DEBOUNCE) {
                        paths.extend(event_paths(event));
                    }
                    let mut announced = false;
                    for topic in paths.iter().filter_map(|path| topic_for(&root, path)) {
                        announced = true;
                        let _ = self.topics.send(topic);
                    }
                    if announced {
                        let _ = self.topics.send(Topic::ChangeList);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => self.announce_floor(),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    watcher = None;
                    self.announce_floor();
                    std::thread::sleep(FLOOR_SCAN);
                }
            }
            // Keep the watcher alive for as long as it is healthy.
            let _ = &watcher;
        }
    }

    fn announce_floor(&self) {
        match self.store.list() {
            Ok(changes) => {
                for change in changes {
                    let _ = self.topics.send(Topic::Change {
                        repo: change.repo,
                        card: change.card,
                    });
                }
                let _ = self.topics.send(Topic::ChangeList);
            }
            Err(error) => tracing::warn!(%error, "change floor scan failed"),
        }
    }
}

fn event_paths(event: notify::Result<notify::Event>) -> HashSet<PathBuf> {
    match event {
        Ok(event) => event.paths.into_iter().collect(),
        Err(error) => {
            tracing::warn!(%error, "change watcher event failed");
            HashSet::new()
        }
    }
}

fn topic_for(root: &Path, path: &Path) -> Option<Topic> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = relative.components();
    let repo = parts.next()?.as_os_str().to_str()?.to_owned();
    let file = parts.next()?.as_os_str().to_str()?;
    if parts.next().is_some() {
        return None;
    }
    let card = file.strip_suffix(".jsonl")?.parse().ok()?;
    change::validate_repo(&repo).ok()?;
    change::validate_card(card).ok()?;
    Some(Topic::Change { repo, card })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_change_logs_below_one_repo_become_topics() {
        let root = Path::new("/state/changes");
        assert_eq!(
            topic_for(root, Path::new("/state/changes/fleet/81.jsonl")),
            Some(Topic::Change {
                repo: "fleet".to_owned(),
                card: 81,
            })
        );
        assert_eq!(
            topic_for(root, Path::new("/state/changes/fleet/81.lock")),
            None
        );
        assert_eq!(
            topic_for(root, Path::new("/state/changes/fleet/nested/81.jsonl")),
            None
        );
        assert_eq!(topic_for(root, Path::new("/outside/fleet/81.jsonl")), None);
    }
}
