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

mod sessions;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ingest::Topic;
use crate::store::Store;

pub use sessions::SessionsView;

/// Which view a subscription is for, with its parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ViewSpec {
    /// Every session across every harness, with each source's health.
    Sessions,
}

impl ViewSpec {
    /// Whether this view must recompute because `topic` fired.
    ///
    /// Keep this exhaustive over `Topic` rather than using a catch-all: a new
    /// topic should force every view to state its relationship to it, which is
    /// exactly the review moment that stops a view going quietly stale.
    pub fn affected_by(&self, topic: &Topic) -> bool {
        match self {
            ViewSpec::Sessions => match topic {
                Topic::SessionList | Topic::SourceHealth => true,
                Topic::Session(_) => false,
            },
        }
    }

    /// Recompute the view. Blocking — this reads SQLite, so an async caller
    /// must run it on a blocking task.
    pub fn compute(&self, store: &Store) -> Result<ViewData> {
        match self {
            ViewSpec::Sessions => Ok(ViewData::Sessions(sessions::compute(store)?)),
        }
    }
}

/// A view's data, tagged so the client can narrow on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ViewData {
    Sessions(SessionsView),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sessions_view_wakes_for_the_list_and_for_source_health() {
        let view = ViewSpec::Sessions;
        assert!(view.affected_by(&Topic::SessionList));
        assert!(view.affected_by(&Topic::SourceHealth));
    }

    #[test]
    fn the_sessions_view_ignores_a_single_session_changing() {
        // One session's entries growing does not change the list; the summary
        // change that would is announced as SessionList.
        assert!(!ViewSpec::Sessions.affected_by(&Topic::Session("pi:a".parse().unwrap())));
    }
}
