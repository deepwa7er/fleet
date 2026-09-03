//! pi entries → the domain's messages and parts.
//!
//! The counterpart to `pi::summarize`: where that reads the branch for the
//! session's headline facts, this maps one entry to the message it renders as.
//! Mapping is per-entry and pure, which is what makes it safe to do **once, at
//! ingest**: a session file is append-only, so an entry never changes, so its
//! mapping never needs invalidating.
//!
//! Assembling those messages into a transcript — the leaf-path walk and the
//! folding of tool results into their calls — is not per-entry, and lives in
//! `views::session`.
//!
//! ## Rendering policy for non-message entries
//!
//! | entry | renders as |
//! |---|---|
//! | compaction, branch summary, custom message | agent prose |
//! | `bashExecution` | nothing — a bare shell echo has no slot in a transcript, and its output reaches the reader through the assistant text that cites it |
//! | `custom`, `label`, `thinking_level_change` | nothing — no user-facing prose |
//! | `model_change`, `session_info` | nothing — metadata, surfaced on the session itself |

use serde_json::Value;

use super::loop_services::truncate_tool_output;
use crate::content::parse;
use crate::model::{Entry, Message, Part, Role, ToolStatus};

/// Map one entry to the message it renders as, or `None` when it renders
/// nothing.
pub fn map_entry(entry: &Entry) -> Option<Message> {
    let kind = entry.raw.get("type").and_then(Value::as_str);
    match kind {
        Some("message") => map_message(entry),
        Some("compaction") | Some("branch_summary") => {
            Some(prose(entry, entry.raw.get("summary").and_then(Value::as_str).unwrap_or_default()))
        }
        Some("custom_message") => {
            Some(prose(entry, &content_text(entry.raw.get("content"))))
        }
        _ => None,
    }
}

fn map_message(entry: &Entry) -> Option<Message> {
    let message = entry.raw.get("message")?;
    match message.get("role").and_then(Value::as_str)? {
        "user" => Some(chat(entry, message, Role::User)),
        "assistant" => Some(chat(entry, message, Role::Assistant)),
        "branchSummary" | "compactionSummary" => {
            Some(prose(entry, message.get("summary").and_then(Value::as_str).unwrap_or_default()))
        }
        "custom" => Some(prose(entry, &content_text(message.get("content")))),
        "toolResult" => Some(tool_result(entry, message)),
        // bashExecution and anything unrecognised render nothing.
        _ => None,
    }
}

fn chat(entry: &Entry, message: &Value, role: Role) -> Message {
    let created = super::pi::timestamp_ms(&entry.raw);
    Message {
        id: entry.id.clone(),
        role,
        agent: match role {
            Role::Assistant => message.get("model").and_then(Value::as_str).map(str::to_owned),
            _ => None,
        },
        created_ms: created,
        // A persisted entry is by definition finished: pi writes the assistant
        // entry at message_end. Only the live overlay lacks a completion.
        completed_ms: created,
        parts: content_parts(message.get("content")),
    }
}

/// Compaction, branch summaries, and custom messages render as agent-side
/// prose — completed, so nothing can mistake a finished summary for a live
/// stream.
fn prose(entry: &Entry, text: &str) -> Message {
    let created = super::pi::timestamp_ms(&entry.raw);
    Message {
        id: entry.id.clone(),
        role: Role::Assistant,
        agent: None,
        created_ms: created,
        completed_ms: created,
        parts: if text.is_empty() {
            Vec::new()
        } else {
            vec![Part::Text { blocks: parse(text) }]
        },
    }
}

fn tool_result(entry: &Entry, message: &Value) -> Message {
    let created = super::pi::timestamp_ms(&entry.raw);
    let status = if message.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        ToolStatus::Error
    } else {
        ToolStatus::Completed
    };
    Message {
        id: entry.id.clone(),
        role: Role::Tool,
        agent: None,
        created_ms: created,
        completed_ms: created,
        parts: vec![Part::Tool {
            call_id: message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: message.get("toolName").and_then(Value::as_str).unwrap_or_default().to_owned(),
            status,
            output: Some(truncate_tool_output(&content_text(message.get("content")))),
        }],
    }
}

/// Map a message's content blocks to parts.
fn content_parts(content: Option<&Value>) -> Vec<Part> {
    normalize(content)
        .into_iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str)? {
            "text" => Some(Part::Text {
                blocks: parse(block.get("text").and_then(Value::as_str).unwrap_or_default()),
            }),
            "thinking" => Some(Part::Reasoning {
                blocks: parse(block.get("thinking").and_then(Value::as_str).unwrap_or_default()),
            }),
            "image" => Some(Part::File { filename: image_filename(&block) }),
            "toolCall" => Some(Part::Tool {
                call_id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                // A call starts running; its result folds in later and flips
                // this (see `views::session`).
                status: ToolStatus::Running,
                output: None,
            }),
            _ => None,
        })
        .collect()
}

/// pi stores images as base64 with no name, so the filename is synthesised
/// from the mime type and is display-only: `image/svg+xml` → `image.svg`.
fn image_filename(block: &Value) -> String {
    let mime = block.get("mimeType").and_then(Value::as_str).unwrap_or_default();
    let subtype = mime.split('/').nth(1).unwrap_or_default();
    let ext = subtype.split('+').next().filter(|e| !e.is_empty()).unwrap_or("img");
    format!("image.{ext}")
}

/// Content is either a bare string or an array of typed blocks.
fn normalize(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => {
            vec![serde_json::json!({ "type": "text", "text": text })]
        }
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str).is_some())
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// The text of a content value, for the places that want prose rather than
/// parts.
fn content_text(content: Option<&Value>) -> String {
    normalize(content)
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .map(|block| block.get("text").and_then(Value::as_str).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Block, Inline};

    fn entry(raw: serde_json::Value) -> Entry {
        Entry {
            seq: 1,
            id: raw.get("id").and_then(Value::as_str).unwrap_or("e").to_owned(),
            parent_id: None,
            raw,
            mapped: None,
        }
    }

    fn text_of(part: &Part) -> String {
        let (Part::Text { blocks } | Part::Reasoning { blocks }) = part else {
            panic!("{part:?}")
        };
        blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph { inlines } => inlines
                    .iter()
                    .map(|i| match i {
                        Inline::Text { text } => text.clone(),
                        other => format!("{other:?}"),
                    })
                    .collect::<String>(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_user_message_maps_to_parsed_text() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "timestamp": "2026-08-23T10:00:00.000Z",
            "message": { "role": "user", "content": "hello **world**" }
        })))
        .unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.id, "a");
        assert_eq!(message.created_ms, Some(1_787_479_200_000));
        assert_eq!(message.completed_ms, message.created_ms);
        assert_eq!(message.parts.len(), 1);
    }

    #[test]
    fn an_assistant_message_carries_its_model_and_splits_reasoning_from_text() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": {
                "role": "assistant", "model": "sonnet",
                "content": [
                    { "type": "thinking", "thinking": "let me think" },
                    { "type": "text", "text": "the answer" }
                ]
            }
        })))
        .unwrap();
        assert_eq!(message.agent.as_deref(), Some("sonnet"));
        assert!(matches!(message.parts[0], Part::Reasoning { .. }));
        assert_eq!(text_of(&message.parts[0]), "let me think");
        assert!(matches!(message.parts[1], Part::Text { .. }));
        assert_eq!(text_of(&message.parts[1]), "the answer");
    }

    #[test]
    fn a_user_message_never_claims_an_agent() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": { "role": "user", "model": "sonnet", "content": "hi" }
        })))
        .unwrap();
        assert_eq!(message.agent, None);
    }

    #[test]
    fn a_tool_call_starts_running_with_no_output() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": { "role": "assistant", "content": [
                { "type": "toolCall", "id": "call-1", "name": "bash" }
            ]}
        })))
        .unwrap();
        let Part::Tool { call_id, name, status, output } = &message.parts[0] else { panic!() };
        assert_eq!(call_id, "call-1");
        assert_eq!(name, "bash");
        assert_eq!(*status, ToolStatus::Running);
        assert_eq!(*output, None);
    }

    #[test]
    fn a_tool_result_maps_to_its_own_message_for_folding() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "r", "type": "message",
            "message": {
                "role": "toolResult", "toolCallId": "call-1", "toolName": "bash",
                "content": [{ "type": "text", "text": "output here" }]
            }
        })))
        .unwrap();
        assert_eq!(message.role, Role::Tool);
        let Part::Tool { call_id, status, output, .. } = &message.parts[0] else { panic!() };
        assert_eq!(call_id, "call-1");
        assert_eq!(*status, ToolStatus::Completed);
        assert_eq!(output.as_deref(), Some("output here"));
    }

    #[test]
    fn a_failed_tool_result_takes_the_error_status() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "r", "type": "message",
            "message": { "role": "toolResult", "toolCallId": "c", "isError": true, "content": "boom" }
        })))
        .unwrap();
        let Part::Tool { status, .. } = &message.parts[0] else { panic!() };
        assert_eq!(*status, ToolStatus::Error);
        assert!(status.is_failure());
    }

    #[test]
    fn tool_output_is_truncated_without_splitting_a_character() {
        let long = "é".repeat(4_000);
        let message = map_entry(&entry(serde_json::json!({
            "id": "r", "type": "message",
            "message": { "role": "toolResult", "toolCallId": "c", "content": long }
        })))
        .unwrap();
        let Part::Tool { output, .. } = &message.parts[0] else { panic!() };
        let output = output.as_deref().unwrap();
        assert!(output.ends_with('…'));
        assert!(
            output.len()
                <= crate::ingest::loop_services::TOOL_OUTPUT_LIMIT + '…'.len_utf8()
        );
    }

    #[test]
    fn tool_output_is_not_parsed_as_markdown() {
        // DW-001: monospace means a machine produced this. Program output that
        // happens to contain `#` is not a heading.
        let message = map_entry(&entry(serde_json::json!({
            "id": "r", "type": "message",
            "message": { "role": "toolResult", "toolCallId": "c", "content": "# not a heading" }
        })))
        .unwrap();
        let Part::Tool { output, .. } = &message.parts[0] else { panic!() };
        assert_eq!(output.as_deref(), Some("# not a heading"));
    }

    #[test]
    fn an_image_becomes_a_named_file_line() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": { "role": "user", "content": [
                { "type": "image", "mimeType": "image/svg+xml" }
            ]}
        })))
        .unwrap();
        assert_eq!(message.parts[0], Part::File { filename: "image.svg".into() });
    }

    #[test]
    fn an_image_without_a_mime_type_still_gets_a_name() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": { "role": "user", "content": [{ "type": "image" }] }
        })))
        .unwrap();
        assert_eq!(message.parts[0], Part::File { filename: "image.img".into() });
    }

    #[test]
    fn summaries_render_as_finished_agent_prose() {
        for raw in [
            serde_json::json!({ "id": "a", "type": "compaction", "summary": "what happened" }),
            serde_json::json!({ "id": "a", "type": "branch_summary", "summary": "what happened" }),
            serde_json::json!({ "id": "a", "type": "message",
                "message": { "role": "compactionSummary", "summary": "what happened" } }),
        ] {
            let message = map_entry(&entry(raw)).unwrap();
            assert_eq!(message.role, Role::Assistant);
            assert_eq!(message.agent, None);
            assert_eq!(text_of(&message.parts[0]), "what happened");
        }
    }

    #[test]
    fn entries_with_no_transcript_rendering_map_to_nothing() {
        for raw in [
            serde_json::json!({ "id": "a", "type": "model_change", "modelId": "x" }),
            serde_json::json!({ "id": "a", "type": "session_info", "name": "x" }),
            serde_json::json!({ "id": "a", "type": "custom", "customType": "orchestrator-mode" }),
            serde_json::json!({ "id": "a", "type": "label" }),
            serde_json::json!({ "id": "a", "type": "thinking_level_change" }),
            serde_json::json!({ "id": "a", "type": "message",
                "message": { "role": "bashExecution", "content": "ls" } }),
            serde_json::json!({ "id": "a" }),
        ] {
            assert_eq!(map_entry(&entry(raw)), None);
        }
    }

    #[test]
    fn an_empty_summary_maps_to_a_message_with_no_parts() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "compaction", "summary": ""
        })))
        .unwrap();
        assert!(message.parts.is_empty());
    }

    #[test]
    fn unknown_content_blocks_render_nothing_without_dropping_their_siblings() {
        let message = map_entry(&entry(serde_json::json!({
            "id": "a", "type": "message",
            "message": { "role": "assistant", "content": [
                { "type": "reasoning_signature", "value": "opaque" },
                { "type": "text", "text": "kept" }
            ]}
        })))
        .unwrap();
        assert_eq!(message.parts.len(), 1);
        assert_eq!(text_of(&message.parts[0]), "kept");
    }
}
