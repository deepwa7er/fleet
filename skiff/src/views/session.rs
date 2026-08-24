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

use crate::model::{Message, ModelCatalog, Part, Role, SessionKey, SessionSummary, ToolStatus};
use crate::run::LiveState;
use crate::store::Store;
use change::{ChangeRef, ChangeService};

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
    /// Available models for the picker. Empty for harnesses without the model
    /// capability; an enumeration failure is isolated here rather than
    /// failing the transcript subscription.
    pub models: ModelCatalog,
    /// The most recently active durable change bound to this session.
    pub change: Option<ChangeRef>,
}

pub fn compute(
    store: &Store,
    changes: &ChangeService,
    id: &SessionKey,
    live: LiveState,
    models: ModelCatalog,
) -> Result<SessionView> {
    let session = store.sessions()?.into_iter().find(|s| s.id == *id);
    if session.is_none() {
        return Ok(SessionView {
            session: None,
            messages: Vec::new(),
            live,
            models,
            change: changes.store().bound_to(&id.to_string())?,
        });
    }
    Ok(SessionView {
        session,
        messages: transcript(&store.entries(id)?),
        live,
        models,
        change: changes.store().bound_to(&id.to_string())?,
    })
}

/// The conversation: the leaf branch, rendered, with tool results folded into
/// their calls.
pub fn transcript(entries: &[crate::model::Entry]) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    // call id -> (message index, part index) of the call awaiting its result.
    let mut awaiting: HashMap<String, (usize, usize)> = HashMap::new();

    for entry in crate::model::leaf_path(entries) {
        let Some(mut message) = entry.mapped.clone() else {
            continue;
        };

        if message.role == Role::Tool {
            message.parts = fold(&mut messages, &mut awaiting, message.parts);
            // Every result found its call, so there is nothing left to show.
            if message.parts.is_empty() {
                continue;
            }
        }

        let index = messages.len();
        for (part_index, part) in message.parts.iter().enumerate() {
            if let Part::Tool {
                call_id,
                status: ToolStatus::Running,
                ..
            } = part
                && !call_id.is_empty()
            {
                awaiting.insert(call_id.clone(), (index, part_index));
            }
        }
        messages.push(message);
    }
    messages
}

/// Fold each tool result into the call it answers, returning the ones that
/// found no call.
///
/// **A result message may carry several results.** pi emits one per message,
/// but muse commits them in batches (`tool_result_batch_committed`), so
/// folding has to be per *part* — assuming one part per message silently left
/// every multi-result batch unfolded, which real muse sessions are full of.
///
/// A result whose call is not on this branch — the call was abandoned, or the
/// file is mid-write — is kept and surfaces on its own. A tool that ran is a
/// fact, and the reader should see it even when the transcript cannot say
/// what asked for it.
fn fold(
    messages: &mut [Message],
    awaiting: &mut HashMap<String, (usize, usize)>,
    parts: Vec<Part>,
) -> Vec<Part> {
    parts
        .into_iter()
        .filter(|part| {
            let Part::Tool {
                call_id,
                status,
                output,
                ..
            } = part
            else {
                return true;
            };
            if *status == ToolStatus::Running {
                return true;
            }
            let Some((message_index, part_index)) = awaiting.remove(call_id) else {
                return true;
            };
            let Some(target) = messages
                .get_mut(message_index)
                .and_then(|m| m.parts.get_mut(part_index))
            else {
                return true;
            };
            let Part::Tool {
                status: call_status,
                output: call_output,
                ..
            } = target
            else {
                return true;
            };
            *call_status = *status;
            *call_output = output.clone();
            false
        })
        .collect()
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
            entry(
                2,
                "b",
                Some("a"),
                Some(message("b", Role::Assistant, vec![])),
            ),
        ];
        let ids: Vec<_> = transcript(&entries).iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn entries_that_render_nothing_are_skipped_without_breaking_the_chain() {
        let entries = vec![
            entry(1, "a", None, Some(message("a", Role::User, vec![]))),
            entry(2, "meta", Some("a"), None),
            entry(
                3,
                "c",
                Some("meta"),
                Some(message("c", Role::Assistant, vec![])),
            ),
        ];
        let ids: Vec<_> = transcript(&entries).iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn a_tool_result_folds_into_the_call_that_asked_for_it() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1")])),
            ),
            entry(
                2,
                "r",
                Some("a"),
                Some(message(
                    "r",
                    Role::Tool,
                    vec![result("c1", ToolStatus::Completed, "done")],
                )),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1, "the result is not a message of its own");
        let Part::Tool { status, output, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(*status, ToolStatus::Completed);
        assert_eq!(output.as_deref(), Some("done"));
    }

    #[test]
    fn a_failed_result_marks_its_call_failed() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1")])),
            ),
            entry(
                2,
                "r",
                Some("a"),
                Some(message(
                    "r",
                    Role::Tool,
                    vec![result("c1", ToolStatus::Error, "boom")],
                )),
            ),
        ];
        let Part::Tool { status, .. } = &transcript(&entries)[0].parts[0] else {
            panic!()
        };
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
            Some(message(
                "r",
                Role::Tool,
                vec![result("orphan", ToolStatus::Completed, "out")],
            )),
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
                Some(message(
                    "r2",
                    Role::Tool,
                    vec![result("c2", ToolStatus::Completed, "two")],
                )),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1);
        let Part::Tool { status, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(*status, ToolStatus::Running, "c1 has not answered yet");
        let Part::Tool { output, .. } = &messages[0].parts[1] else {
            panic!()
        };
        assert_eq!(output.as_deref(), Some("two"));
    }

    #[test]
    fn a_second_result_for_one_call_does_not_overwrite_the_first() {
        // Only the first result claims the call; a duplicate is surfaced
        // rather than silently replacing what the reader already saw.
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1")])),
            ),
            entry(
                2,
                "r",
                Some("a"),
                Some(message(
                    "r",
                    Role::Tool,
                    vec![result("c1", ToolStatus::Completed, "first")],
                )),
            ),
            entry(
                3,
                "r2",
                Some("r"),
                Some(message(
                    "r2",
                    Role::Tool,
                    vec![result("c1", ToolStatus::Error, "second")],
                )),
            ),
        ];
        let messages = transcript(&entries);
        let Part::Tool { output, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(output.as_deref(), Some("first"));
        assert_eq!(messages.len(), 2, "the duplicate surfaces on its own");
    }

    #[test]
    fn a_result_on_an_abandoned_branch_does_not_fold_into_a_live_call() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1")])),
            ),
            entry(
                2,
                "abandoned",
                Some("a"),
                Some(message(
                    "x",
                    Role::Tool,
                    vec![result("c1", ToolStatus::Completed, "no")],
                )),
            ),
            entry(3, "c", Some("a"), Some(message("c", Role::User, vec![]))),
        ];
        let messages = transcript(&entries);
        let Part::Tool { status, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(
            *status,
            ToolStatus::Running,
            "the abandoned result must not fold"
        );
    }

    #[test]
    fn a_batch_of_results_folds_each_into_its_own_call() {
        // muse commits results in batches. Assuming one result per message —
        // which is true of pi — left every multi-result batch unfolded, and
        // real muse sessions are full of them.
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1"), call("c2")])),
            ),
            entry(
                2,
                "r",
                Some("a"),
                Some(message(
                    "r",
                    Role::Tool,
                    vec![
                        result("c1", ToolStatus::Completed, "one"),
                        result("c2", ToolStatus::Error, "two"),
                    ],
                )),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 1, "the whole batch folded");
        let Part::Tool { output, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(output.as_deref(), Some("one"));
        let Part::Tool { status, output, .. } = &messages[0].parts[1] else {
            panic!()
        };
        assert_eq!(*status, ToolStatus::Error);
        assert_eq!(output.as_deref(), Some("two"));
    }

    #[test]
    fn a_partly_matched_batch_keeps_only_what_did_not_fold() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                Some(message("a", Role::Assistant, vec![call("c1")])),
            ),
            entry(
                2,
                "r",
                Some("a"),
                Some(message(
                    "r",
                    Role::Tool,
                    vec![
                        result("c1", ToolStatus::Completed, "matched"),
                        result("orphan", ToolStatus::Completed, "unmatched"),
                    ],
                )),
            ),
        ];
        let messages = transcript(&entries);
        assert_eq!(messages.len(), 2);
        let Part::Tool { output, .. } = &messages[0].parts[0] else {
            panic!()
        };
        assert_eq!(output.as_deref(), Some("matched"));
        assert_eq!(
            messages[1].parts.len(),
            1,
            "only the orphan survives on its own"
        );
        let Part::Tool { call_id, .. } = &messages[1].parts[0] else {
            panic!()
        };
        assert_eq!(call_id, "orphan");
    }

    #[test]
    fn an_empty_session_has_an_empty_transcript() {
        assert!(transcript(&[]).is_empty());
    }

    #[test]
    fn an_unknown_session_is_named_as_absent_rather_than_empty() {
        let store = Store::in_memory().unwrap();
        let changes = tempfile::tempdir().unwrap();
        let repos = tempfile::tempdir().unwrap();
        let changes = ChangeService::new(
            change::Store::new(changes.path()),
            repos.path(),
            change::Jj::new("jj"),
        );
        let view = compute(
            &store,
            &changes,
            &"pi:nope".parse().unwrap(),
            LiveState::default(),
            ModelCatalog::default(),
        )
        .unwrap();
        assert_eq!(view.session, None);
        assert!(view.messages.is_empty());
    }

    #[test]
    fn a_session_view_includes_its_durable_change_binding() {
        let store = Store::in_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let repos = tempfile::tempdir().unwrap();
        let changes = ChangeService::new(
            change::Store::new(root.path()),
            repos.path(),
            change::Jj::new("jj"),
        );
        changes
            .store()
            .create("fleet", 124, Some("Rust Skiff"), Some("pi:abc"))
            .unwrap();

        let view = compute(
            &store,
            &changes,
            &"pi:abc".parse().unwrap(),
            LiveState::default(),
            ModelCatalog::default(),
        )
        .unwrap();

        let change = view.change.expect("bound change");
        assert_eq!(change.repo, "fleet");
        assert_eq!(change.card, 124);
        assert_eq!(change.title.as_deref(), Some("Rust Skiff"));
    }
}
