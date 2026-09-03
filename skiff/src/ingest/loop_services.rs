//! Shared loop services for every pump (card #157).
//!
//! Three loops poll three truths — [`crate::ingest::Ingest`] tails session
//! files, [`crate::change_watch::ChangeWatch`] tails the change log, and
//! [`crate::ingest::opencode::OpencodeIngest`] polls `opencode serve` — but
//! the policies around the poll are one policy: watch with a burst debounce,
//! rescan on a floor interval even when nothing fired, record health with
//! diff-and-announce, and forget stale sessions with their topics. This module
//! is that policy's home, so the next pump copies none of it.
//!
//! The sync file pumps share [`WatchPolicy`]; the async OpenCode pump shares
//! the same [`FLOOR_SCAN`] and [`EVENT_DEBOUNCE`] constants directly, because
//! unifying a `std::mpsc` burst drain with a `tokio::select!` event loop under
//! one generic would obscure both call sites rather than converge them.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tokio::sync::broadcast;

use crate::model::{Harness, SessionKey, SessionSummary, SourceHealth};
use crate::store::Store;

use super::Topic;

/// Rescan even when the watcher reports nothing.
///
/// inotify cannot watch a directory that does not exist yet, silently drops
/// events under queue overflow, and does not work at all on some filesystems.
/// Without a floor, any of those turns into a list that is quietly and
/// permanently stale.
pub const FLOOR_SCAN: Duration = Duration::from_secs(15);

/// How long a burst of filesystem events may settle before a scan.
///
/// A live harness writes many lines per second; coalescing them into one pass
/// is the difference between a scan per line and a scan per burst.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);

/// How long OpenCode's event stream may settle before a rescan.
///
/// Separate from [`WATCH_DEBOUNCE`] deliberately: SSE frames arrive in network
/// bursts rather than filesystem bursts, and the two coalescing windows answer
/// different transports.
pub const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

/// A single tool dump must not balloon a whole transcript.
pub const TOOL_OUTPUT_LIMIT: usize = 2_000;

/// The process owner's home directory, falling back to `/`.
///
/// One home: `ingest::pi`, `ingest::muse`, `run`, and `config` previously each
/// resolved this inline.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Epoch milliseconds for health watermarks and prompt echoes.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Truncate tool output on a character boundary.
///
/// Slicing mid-codepoint would panic on any tool output containing non-ASCII;
/// the shared helper means the cap's comment ("same cap as pi") is a constant,
/// not a comment doing a constant's job.
pub fn truncate_tool_output(text: &str) -> String {
    if text.len() <= TOOL_OUTPUT_LIMIT {
        return text.to_owned();
    }
    let mut end = TOOL_OUTPUT_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Every named health row, typed so a typo cannot mint a new one.
///
/// The wire form stays the same short strings (`SourceHealth.source` is still
/// a `String` for the client), but constructors take this enum — the six
/// previously free-form sites (`"pi"`, `"muse"`, `"opencode"`,
/// `"opencode events"`, `"pi runner"`, `"muse runner"`) now resolve here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HealthSource {
    Pi,
    Muse,
    Opencode,
    OpencodeEvents,
    PiRunner,
    MuseRunner,
}

impl HealthSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            HealthSource::Pi => "pi",
            HealthSource::Muse => "muse",
            HealthSource::Opencode => "opencode",
            HealthSource::OpencodeEvents => "opencode events",
            HealthSource::PiRunner => "pi runner",
            HealthSource::MuseRunner => "muse runner",
        }
    }

    /// The file-tailed source for a harness. OpenCode has no file source —
    /// its pipeline is deliberate and separate — but the mapping is total so
    /// callers never fall back to a string literal.
    pub const fn for_harness(harness: Harness) -> Self {
        match harness {
            Harness::Pi => HealthSource::Pi,
            Harness::Muse => HealthSource::Muse,
            Harness::Opencode => HealthSource::Opencode,
        }
    }
}

/// Record one source's health, announcing only when it actually changed.
///
/// Returns whether the row changed. The compare-previous-then-announce logic
/// previously lived inline in both [`crate::ingest::Ingest::scan_source`] and
/// [`crate::ingest::opencode::OpencodeIngest::set_health`]; the runner health
/// sites announced unconditionally and now converge on the same policy.
pub fn record_health(
    store: &Store,
    topics: &broadcast::Sender<Topic>,
    source: HealthSource,
    error: Option<String>,
) -> Result<bool> {
    let health =
        SourceHealth { source: source.as_str().to_owned(), error, checked_ms: now_ms() };
    let previous = store
        .source_health()?
        .into_iter()
        .find(|item| item.source == health.source)
        .map(|item| item.error);
    store.set_source_health(&health)?;
    let changed = previous.as_ref() != Some(&health.error);
    if changed {
        // No subscribers is the normal state when nobody has the app open.
        let _ = topics.send(Topic::SourceHealth);
    }
    Ok(changed)
}

/// The sessions of one harness that a scan no longer sees.
pub fn stale_sessions(
    existing: &[SessionSummary],
    harness: Harness,
    seen: &HashSet<SessionKey>,
) -> Vec<SessionKey> {
    existing
        .iter()
        .filter(|summary| summary.harness == harness && !seen.contains(&summary.id))
        .map(|summary| summary.id.clone())
        .collect()
}

/// Forget stale sessions, announcing each one.
///
/// Both the file pump and the OpenCode pump converge here: forgetting is a
/// store delete plus `Session`/`SessionList` topics, never a silent drop.
pub fn forget_sessions(
    store: &Store,
    stale: &[SessionKey],
    changed: &mut HashSet<Topic>,
) -> Result<()> {
    for id in stale {
        store.forget_session(id)?;
        changed.insert(Topic::Session(id.clone()));
        changed.insert(Topic::SessionList);
    }
    Ok(())
}

/// What one settled wait produced.
pub enum Settled<T> {
    /// At least the first event, plus whatever else arrived within the
    /// debounce window.
    Events(Vec<T>),
    /// Nothing arrived before the floor expired.
    Floor,
    /// The event sender is gone; the floor scan remains correct, if slower.
    Disconnected,
}

/// The watch-debounce-floor policy for the sync file pumps.
///
/// `Ingest::pump` discards the drained burst (any write means rescan);
/// `ChangeWatch::pump` maps it to paths. Both block the same way and handle
/// the same three outcomes, so both wait here.
#[derive(Debug, Clone, Copy)]
pub struct WatchPolicy {
    pub debounce: Duration,
    pub floor: Duration,
}

impl WatchPolicy {
    pub const FILE: Self = Self { debounce: WATCH_DEBOUNCE, floor: FLOOR_SCAN };

    pub fn wait<T>(&self, events: &std::sync::mpsc::Receiver<T>) -> Settled<T> {
        match events.recv_timeout(self.floor) {
            Ok(first) => {
                let mut out = vec![first];
                while let Ok(next) = events.recv_timeout(self.debounce) {
                    out.push(next);
                }
                Settled::Events(out)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Settled::Floor,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Settled::Disconnected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_sources_are_the_historic_wire_strings() {
        assert_eq!(HealthSource::Pi.as_str(), "pi");
        assert_eq!(HealthSource::Muse.as_str(), "muse");
        assert_eq!(HealthSource::Opencode.as_str(), "opencode");
        assert_eq!(HealthSource::OpencodeEvents.as_str(), "opencode events");
        assert_eq!(HealthSource::PiRunner.as_str(), "pi runner");
        assert_eq!(HealthSource::MuseRunner.as_str(), "muse runner");
    }

    #[test]
    fn truncation_never_splits_a_codepoint() {
        let text = format!("{}é{}", "x".repeat(TOOL_OUTPUT_LIMIT - 1), "y".repeat(10));
        let cut = truncate_tool_output(&text);
        assert!(cut.len() <= TOOL_OUTPUT_LIMIT + '…'.len_utf8());
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_first_health_record_announces_and_a_repeat_does_not() {
        let store = Store::in_memory().unwrap();
        let (topics, mut rx) = broadcast::channel(16);
        assert!(
            record_health(&store, &topics, HealthSource::Pi, None).unwrap(),
            "the first record must announce"
        );
        assert!(rx.try_recv().is_ok());
        assert!(
            !record_health(&store, &topics, HealthSource::Pi, None).unwrap(),
            "an unchanged repeat must stay silent"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn stale_detection_is_scoped_to_one_harness() {
        use crate::model::{Capabilities, Harness};
        let summary = |harness: Harness, local: &str| SessionSummary {
            id: SessionKey::new(harness, local),
            harness,
            capabilities: Capabilities { rename: false, orchestrator: false, model: false },
            title: None,
            directory: None,
            created_ms: None,
            updated_ms: None,
            model: None,
            orchestrator_active: false,
        };
        let existing =
            vec![summary(Harness::Pi, "gone"), summary(Harness::Pi, "kept"), summary(Harness::Muse, "gone")];
        let seen: HashSet<SessionKey> =
            [SessionKey::new(Harness::Pi, "kept")].into_iter().collect();
        let stale = stale_sessions(&existing, Harness::Pi, &seen);
        assert_eq!(stale, vec![SessionKey::new(Harness::Pi, "gone")]);
    }
}
