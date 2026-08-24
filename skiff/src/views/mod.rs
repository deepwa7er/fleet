//! Views: the closed set of live queries (DW-004 §6).
//!
//! Every read the client performs is a subscription to one of these. There is
//! no request/response read path, so cold load and live update are the same
//! mechanism and cannot disagree with each other.
//!
//! The set is **closed and hand-written** on purpose. A generic reactive-query
//! engine is a swamp; a handful of views, each naming exactly what invalidates
//! it, stays legible indefinitely.
//!
//! **Snapshot-only is the default.** A view recomputes and re-sends itself
//! whole when one of its topics fires. Only the session transcript can't
//! afford that, and only it gets a delta protocol — because it is the only
//! view where the interesting update is a few hundred bytes appended to
//! something large, many times a second.

mod session;
mod sessions;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ingest::Topic;
use crate::model::SessionKey;
use crate::run::LiveState;
use crate::store::Store;

pub use session::{SessionView, transcript};
pub use sessions::SessionsView;

/// Which view a subscription is for, with its parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ViewSpec {
    /// Every session across every harness, with each source's health.
    Sessions,
    /// One session: its transcript and live state.
    Session { id: SessionKey },
}

/// What a subscription must send because a topic fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    /// Recompute and re-send the view whole.
    Snapshot,
    /// Send only the session's live state. Touches no SQLite and carries only
    /// the in-flight message — which is the whole reason `Topic::Run` is
    /// separate from `Topic::Session`.
    Live,
}

impl ViewSpec {
    /// How this view must update because `topic` fired, or `None` if it is
    /// unaffected.
    ///
    /// Kept exhaustive over `Topic` rather than using a catch-all: a new topic
    /// should force every view to state its relationship to it, which is
    /// exactly the review moment that stops a view going quietly stale. It
    /// already worked once — adding `Topic::Run` failed to compile here until
    /// both views had an answer for it.
    pub fn update_for(&self, topic: &Topic) -> Option<Update> {
        match self {
            ViewSpec::Sessions => match topic {
                Topic::SessionList | Topic::SourceHealth => Some(Update::Snapshot),
                // A session's transcript or its in-flight reply changes
                // nothing in the list; the summary change that would is
                // announced as `SessionList`.
                Topic::Session(_) | Topic::Run(_) => None,
            },
            ViewSpec::Session { id } => match topic {
                // Everything this view renders from the store — the transcript
                // and the summary in its header — changes only when this
                // session does, and the ingest announces `Session(id)`
                // alongside `SessionList` for exactly that reason. Waking on
                // `SessionList` too would re-send this session's whole
                // transcript every time an *unrelated* session appended a
                // line, which on a busy desktop is most of the time.
                Topic::Session(changed) if changed == id => Some(Update::Snapshot),
                Topic::Run(changed) if changed == id => Some(Update::Live),
                Topic::Session(_) | Topic::Run(_) => None,
                Topic::SessionList | Topic::SourceHealth => None,
            },
        }
    }

    /// Recompute the view. Blocking — this reads SQLite, so an async caller
    /// must run it on a blocking task.
    ///
    /// `live` is passed in rather than fetched here because it comes from the
    /// run registry, which is async; keeping this function blocking-only is
    /// what lets it run on a blocking task without a runtime handle.
    pub fn compute(&self, store: &Store, live: LiveState) -> Result<ViewData> {
        match self {
            ViewSpec::Sessions => Ok(ViewData::Sessions(sessions::compute(store)?)),
            ViewSpec::Session { id } => {
                Ok(ViewData::Session(Box::new(session::compute(store, id, live)?)))
            }
        }
    }

    /// The session this view watches, if it watches one.
    pub fn session(&self) -> Option<&SessionKey> {
        match self {
            ViewSpec::Sessions => None,
            ViewSpec::Session { id } => Some(id),
        }
    }
}

/// A view's data, tagged so the client can narrow on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ViewData {
    Sessions(SessionsView),
    /// Boxed: a session view carries a whole transcript, and an unboxed
    /// variant would make every `ViewData` — including a small session list —
    /// as large as the largest one. Transparent on the wire and in TypeScript.
    Session(Box<SessionView>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> SessionKey {
        "pi:a".parse().unwrap()
    }

    fn b() -> SessionKey {
        "pi:b".parse().unwrap()
    }

    #[test]
    fn the_sessions_view_wakes_for_the_list_and_for_source_health() {
        let view = ViewSpec::Sessions;
        assert_eq!(view.update_for(&Topic::SessionList), Some(Update::Snapshot));
        assert_eq!(view.update_for(&Topic::SourceHealth), Some(Update::Snapshot));
    }

    #[test]
    fn the_sessions_view_ignores_transcripts_and_live_replies() {
        // Neither changes the list, and a streaming reply fires many times a
        // second — waking every open list for it would be the expensive
        // mistake this separation exists to avoid.
        let view = ViewSpec::Sessions;
        assert_eq!(view.update_for(&Topic::Session(a())), None);
        assert_eq!(view.update_for(&Topic::Run(a())), None);
    }

    #[test]
    fn a_session_view_snapshots_for_its_transcript_and_deltas_for_its_run() {
        let view = ViewSpec::Session { id: a() };
        assert_eq!(view.update_for(&Topic::Session(a())), Some(Update::Snapshot));
        assert_eq!(view.update_for(&Topic::Run(a())), Some(Update::Live));
    }

    #[test]
    fn a_session_view_ignores_every_other_session() {
        let view = ViewSpec::Session { id: a() };
        assert_eq!(view.update_for(&Topic::Session(b())), None);
        assert_eq!(view.update_for(&Topic::Run(b())), None);
    }

    #[test]
    fn a_session_view_ignores_the_list_and_source_health() {
        // The transcript is large. Waking it for `SessionList` would re-send
        // the whole thing whenever any other session appended a line.
        let view = ViewSpec::Session { id: a() };
        assert_eq!(view.update_for(&Topic::SessionList), None);
        assert_eq!(view.update_for(&Topic::SourceHealth), None);
    }
}
