//! The `session(id)` view: one session's transcript and live state.
//!
//! Assembly is the half of transcript-building that is *not* per-entry, and so
//! cannot happen at ingest:
//!
//! - **The leaf path.** A session file is a tree; which entries are in the
//!   conversation depends on every other entry, and changes when a branch is
//!   abandoned.
//! - **Folding.** A tool result belongs on the tool call that produced it,
//!   which is in an earlier message.
//!
//! Both are cheap — pointer-chasing and a hash lookup. The expensive work
//! (markdown, highlighting) already happened once, at ingest.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{Message, Part, SessionKey, SessionSummary, ToolStatus};
use crate::run::LiveState;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct SessionView {
    /// `None` when the session is not (or no longer) known — a file deleted
    /// while a pane was open. The client shows that, rather than an empty
    /// transcript that looks like a session with nothing in it.
    pub session: Option<SessionSummary>,
    /// The conversation, oldest first. Every message here is finished;
    /// liveness lives in `live`, never in a missing timestamp.
    pub messages: Vec<Message>,
    /// The in-flight reply, whether the harness is working, and any prompt
    /// that has been sent but has not yet reached the transcript.
    pub live: LiveState,
}

pub fn compute(store: &Store, id: &SessionKey, live: LiveState) -> Result<SessionView> {
    let session = store.sessions()?.into_iter().find(|s| s.id == *id);
    if session.is_none() {
        return Ok(SessionView { session: None, messages: Vec::new(), live });
    }
    Ok(SessionView { session, messages: transcript(&store.entries(id)?), live })
}

/// The conversation: the leaf branch, rendered, with tool results folded into
/// their calls.
pub fn transcript(entries: &[crate::model::Entry]) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    // call id -> (message index, part index) of the call awaiting its result.
    let mut awaiting: HashMap<String, (usize, usize)> = HashMap::new();

    for entry in crate::model::leaf_path(entries) {
        let Some(message) = entry.mapped.clone() else { continue };

        if let Some(folded) = fold(&mut messages, &mut awaiting, &message) {
            debug_assert!(folded, "fold answers whether it consumed the message");
            continue;
        }

        let index = messages.len();
        for (part_index, part) in message.parts.iter().enumerate() {
            if let Part::Tool { call_id, status: ToolStatus::Running, .. } = part
                && !call_id.is_empty()
            {
                awaiting.insert(call_id.clone(), (index, part_index));
            }
        }
        messages.push(message);
    }
    messages
}

/// Fold a tool-result message into the call it answers.
///
/// Answers `Some(true)` when the message was consumed. A result whose call is
/// not on this branch — the call was abandoned, or the file is mid-write —
/// is *not* consumed, and surfaces as its own message rather than vanishing:
/// a tool that ran is a fact, and the reader should see it even when the
/// transcript cannot say what asked for it.
fn fold(
    messages: &mut [Message],
    awaiting: &mut HashMap<String, (usize, usize)>,
    message: &Message,
) -> Option<bool> {
    let [Part::Tool { call_id, status, output, .. }] = message.parts.as_slice() else {
        return None;
    };
    if *status == ToolStatus::Running {
        return None;
    }
    let (message_index, part_index) = awaiting.remove(call_id)?;
    let target = messages.get_mut(message_index)?.parts.get_mut(part_index)?;
    let Part::Tool { status: call_status, output: call_output, .. } = target else {
        return None;
    };
    *call_status = *status;
    *call_output = output.clone();
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, Role};

    fn entry(seq: i64, id: &str, parent: Option<&str>, mapped: Option<Message>) -> Entry {
        Entry {
            seq,
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            raw: serde_json::Value::Null,
            mapped,
        }
    }

    fn message(id: &str, role: Role, parts: Vec<Part>) -> Message {
        Message {
            id: id.to_owned(),
            role,
            agent: None,
            created_ms: Some(1),
            completed_ms: Some(1),
            parts,
        }
    }

    fn call(id: &str) -> Part {
        Part::Tool {
            call_id: id.to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            output: None,
        }
    }

    fn result(id: &str, status: ToolStatus, output: &str) -> Part {
        Part::Tool {
            call_id: id.to_owned(),
            name: "bash".to_owned(),
            status,
            output: Some(output.to_owned()),
        }
    }

    #[test]
    fn the_transcript_is_the_leaf_branch_in_order() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::User, vec![]))),
            entry(2, "b", Some("a"), Some(message("b", Role::Assistant, vec![]))),
        ];
        let ids: Vec<_> = transcript(&entries).iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn entries_that_render_nothing_are_skipped_without_breaking_the_chain() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::User, vec![]))),
            entry(2, "meta", Some("a"), None),
            entry(3, "c", Some("meta"), Some(message("c", Role::Assistant, vec![]))),
        ];
        let ids: Vec<_> = transcript(&entries).iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn a_tool_result_folds_into_the_call_that_asked_for_it() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::Assistant, vec![call("c1")]))),
            entry(
                2,
                "r",
                Some("a"),
                Some(message("r", Role::Tool, vec![result("c1", ToolStatus::Completed, "done")])),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1, "the result is not a message of its own");
        let Part::Tool { status, output, .. } = &messages[0].parts[0] else { panic!() };
        assert_eq!(*status, ToolStatus::Completed);
        assert_eq!(output.as_deref(), Some("done"));
    }

    #[test]
    fn a_failed_result_marks_its_call_failed() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::Assistant, vec![call("c1")]))),
            entry(
                2,
                "r",
                Some("a"),
                Some(message("r", Role::Tool, vec![result("c1", ToolStatus::Error, "boom")])),
            ),
        ];
        let Part::Tool { status, .. } = &transcript(&entries)[0].parts[0] else { panic!() };
        assert_eq!(*status, ToolStatus::Error);
    }

    #[test]
    fn a_result_whose_call_is_not_on_the_branch_surfaces_on_its_own() {
        // The call was abandoned, or the file is mid-write. A tool that ran is
        // a fact; dropping it would be the transcript lying by omission.
        let entries = vec![entry(
            1,
            "r",
            None,
            Some(message("r", Role::Tool, vec![result("orphan", ToolStatus::Completed, "out")])),
        )];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Tool);
    }

    #[test]
    fn each_call_takes_only_its_own_result() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1"), call("c2")])),
            ),
            entry(
                2,
                "r2",
                Some("a"),
                Some(message("r2", Role::Tool, vec![result("c2", ToolStatus::Completed, "two")])),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1);
        let Part::Tool { status, .. } = &messages[0].parts[0] else { panic!() };
        assert_eq!(*status, ToolStatus::Running, "c1 has not answered yet");
        let Part::Tool { output, .. } = &messages[0].parts[1] else { panic!() };
        assert_eq!(output.as_deref(), Some("two"));
    }

    #[test]
    fn a_second_result_for_one_call_does_not_overwrite_the_first() {
        // Only the first result claims the call; a duplicate is surfaced
        // rather than silently replacing what the reader already saw.
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::Assistant, vec![call("c1")]))),
            entry(
                2,
                "r",
                Some("a"),
                Some(message("r", Role::Tool, vec![result("c1", ToolStatus::Completed, "first")])),
            ),
            entry(
                3,
                "r2",
                Some("r"),
                Some(message("r2", Role::Tool, vec![result("c1", ToolStatus::Error, "second")])),
            ),
        ];
        let messages = transcript(&entries);
        let Part::Tool { output, .. } = &messages[0].parts[0] else { panic!() };
        assert_eq!(output.as_deref(), Some("first"));
        assert_eq!(messages.len(), 2, "the duplicate surfaces on its own");
    }

    #[test]
    fn a_result_on_an_abandoned_branch_does_not_fold_into_a_live_call() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::Assistant, vec![call("c1")]))),
            entry(
                2,
                "abandoned",
                Some("a"),
                Some(message("x", Role::Tool, vec![result("c1", ToolStatus::Completed, "no")])),
            ),
            entry(3, "c", Some("a"), Some(message("c", Role::User, vec![]))),
        ];
        let messages = transcript(&entries);
        let Part::Tool { status, .. } = &messages[0].parts[0] else { panic!() };
        assert_eq!(*status, ToolStatus::Running, "the abandoned result must not fold");
    }

    #[test]
    fn an_empty_session_has_an_empty_transcript() {
        assert!(transcript(&[]).is_empty());
    }

    #[test]
    fn an_unknown_session_is_named_as_absent_rather_than_empty() {
        let store = Store::in_memory().unwrap();
        let view =
            compute(&store, &"pi:nope".parse().unwrap(), LiveState::default()).unwrap();
        assert_eq!(view.session, None);
        assert!(view.messages.is_empty());
    }
}
