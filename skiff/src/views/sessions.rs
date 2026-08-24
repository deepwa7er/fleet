//! The `sessions` view: every session across every harness, plus the health of
//! each source that produces them.
//!
//! Source health travels with the list rather than in a channel of its own,
//! because the two are only meaningful together: "no muse sessions" and "muse
//! is unreachable" look identical in a list, and the difference is the whole
//! point of surfacing health at all (DW-004 §4).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{SessionSummary, SourceHealth};
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct SessionsView {
    /// Most recently active first.
    pub sessions: Vec<SessionSummary>,
    pub sources: Vec<SourceHealth>,
}

pub fn compute(store: &Store) -> Result<SessionsView> {
    Ok(SessionsView { sessions: store.sessions()?, sources: store.source_health()? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Capabilities, Harness};
    use crate::store::SessionIngest;

    #[test]
    fn the_view_carries_the_list_and_the_health_of_its_sources() {
        let store = Store::in_memory().unwrap();
        let summary = SessionSummary {
            id: "pi:a".parse().unwrap(),
            harness: Harness::Pi,
            capabilities: Capabilities { rename: true, orchestrator: true, model: true },
            title: None,
            directory: None,
            created_ms: Some(1),
            updated_ms: Some(2),
            model: None,
            orchestrator_active: false,
        };
        store
            .ingest_session(SessionIngest { summary: &summary, state: None, entries: &[] })
            .unwrap();
        store
            .set_source_health(&SourceHealth {
                source: "muse".into(),
                error: Some("binary not found".into()),
                checked_ms: 3,
            })
            .unwrap();

        let view = compute(&store).unwrap();
        assert_eq!(view.sessions, vec![summary]);
        assert_eq!(view.sources[0].error.as_deref(), Some("binary not found"));
    }

    #[test]
    fn an_empty_store_is_an_empty_view_not_an_error() {
        let view = compute(&Store::in_memory().unwrap()).unwrap();
        assert!(view.sessions.is_empty());
        assert!(view.sources.is_empty());
    }
}
