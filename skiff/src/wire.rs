//! The client protocol (DW-004 §7).
//!
//! One WebSocket per client, multiplexed: subscriptions and commands go up,
//! snapshots and deltas come down. Multi-pane means several concurrent
//! subscriptions is the base case, not an optimisation.
//!
//! **Reconnect re-subscribes and takes fresh snapshots. There is no replay
//! buffer.** The snapshot-on-every-connect *is* the convergence guarantee, and
//! it is why there is no position protocol to get wrong. A resubscription
//! always uses a new `sub` id, so deltas still in flight for the dead id are
//! discarded without any sequence reasoning at all.
//!
//! Every type here is exported to TypeScript by `ts-rs`; `tests/types.rs`
//! fails if the checked-in client types have drifted from these.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::views::{ViewData, ViewSpec};

/// A subscription's id, chosen by the client.
pub type SubId = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ClientFrame {
    /// Open a subscription. The server answers with a snapshot, then keeps it
    /// current until it is closed.
    Subscribe { sub: SubId, view: ViewSpec },
    /// Close a subscription. Idempotent: closing an unknown id is not an
    /// error, because a client tearing down a pane should not have to know
    /// whether its subscribe had been processed yet.
    Unsubscribe { sub: SubId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum ServerFrame {
    /// Sent once on connect. `readModel` changes whenever the server's derived
    /// schema does, which is a signal to the client that anything it cached
    /// across a reconnect is worthless.
    ///
    /// The variant carries its own `rename_all`: on an enum, serde's
    /// enum-level `rename_all` renames the *variants*, not their fields.
    #[serde(rename_all = "camelCase")]
    Hello {
        #[ts(type = "number")]
        read_model: i64,
    },
    /// A view, whole. `seq` counts frames within one subscription; it is a
    /// self-describing invariant for debugging, never a resume token.
    Snapshot {
        sub: SubId,
        #[ts(type = "number")]
        seq: u64,
        data: ViewData,
    },
    /// A subscription could not be served, or a frame could not be understood.
    /// `sub` is absent when the failure was not attributable to one.
    Error { sub: Option<SubId>, error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscribe_frame_round_trips() {
        let frame = ClientFrame::Subscribe { sub: 7, view: ViewSpec::Sessions };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(json, r#"{"t":"subscribe","sub":7,"view":{"kind":"sessions"}}"#);
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn an_unsubscribe_frame_round_trips() {
        let json = r#"{"t":"unsubscribe","sub":7}"#;
        assert_eq!(
            serde_json::from_str::<ClientFrame>(json).unwrap(),
            ClientFrame::Unsubscribe { sub: 7 }
        );
    }

    #[test]
    fn an_unknown_frame_type_is_rejected_rather_than_ignored() {
        assert!(serde_json::from_str::<ClientFrame>(r#"{"t":"nonsense"}"#).is_err());
    }

    #[test]
    fn server_frames_are_tagged_for_the_client_to_narrow_on() {
        let hello = serde_json::to_value(ServerFrame::Hello { read_model: 1 }).unwrap();
        assert_eq!(hello["t"], "hello");
        assert_eq!(hello["readModel"], 1);

        let error =
            serde_json::to_value(ServerFrame::Error { sub: None, error: "nope".into() }).unwrap();
        assert_eq!(error["t"], "error");
        assert_eq!(error["sub"], serde_json::Value::Null);
    }
}
