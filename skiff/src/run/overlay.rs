//! The in-flight assistant message.
//!
//! pi streams a reply as deltas keyed by `contentIndex`. They are assembled
//! here into pi's own *file* representation and then mapped through the same
//! `pi_map::map_entry` the persisted entry goes through — so the live message
//! and the settled one are rendered by one code path, and cannot drift into
//! looking different from each other.
//!
//! ## Identity
//!
//! The overlay carries a **run id from the moment it opens**, and keeps it for
//! its whole life. The old bridge used a fixed `<pending>` placeholder and
//! swapped in the real entry id at settlement, which meant the message's
//! identity changed at exactly the moment a reader was most likely to be
//! interacting with it — the reason a reasoning disclosure needed a positional
//! key to survive settling (card #110). Here the client is told which run a
//! settled message came from, so nothing it keyed on has to change.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::ingest::pi_map;
use crate::model::{Entry, Message};

/// A run's identity, stable from `agent_start` to settlement.
pub type RunId = String;

pub struct Overlay {
    run: RunId,
    model: Option<String>,
    /// `contentIndex` → the block under construction. Ordered by index, never
    /// by arrival: tool calls and text interleave, so arrival order is not the
    /// reply's order.
    blocks: BTreeMap<i64, Value>,
    started_ms: i64,
}

impl Overlay {
    pub fn new(run: RunId, model: Option<String>, started_ms: i64) -> Self {
        Self { run, model, blocks: BTreeMap::new(), started_ms }
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    /// Apply one `assistantMessageEvent`. Answers whether anything changed.
    pub fn apply(&mut self, delta: &Value) -> bool {
        let index = delta.get("contentIndex").and_then(Value::as_i64).unwrap_or(0);
        let kind = delta.get("type").and_then(Value::as_str).unwrap_or_default();
        let text = || delta.get("delta").and_then(Value::as_str).unwrap_or_default().to_owned();
        // `*_end` carries the authoritative final content, which replaces
        // whatever the deltas accumulated — a dropped delta cannot leave the
        // settled text wrong.
        let content = || delta.get("content").and_then(Value::as_str).map(str::to_owned);

        match kind {
            "text_start" => {
                self.block(index, || json!({ "type": "text", "text": "" }));
                true
            }
            "text_delta" => {
                let block = self.block(index, || json!({ "type": "text", "text": "" }));
                append(block, "text", &text());
                true
            }
            "text_end" => {
                let block = self.block(index, || json!({ "type": "text", "text": "" }));
                if let Some(content) = content() {
                    block["text"] = Value::String(content);
                }
                true
            }
            "thinking_start" => {
                self.block(index, || json!({ "type": "thinking", "thinking": "" }));
                true
            }
            "thinking_delta" => {
                // The file format names this field `thinking`, not `text`. The
                // assembly must match, or `map_entry` renders it as prose
                // rather than as reasoning.
                let block = self.block(index, || json!({ "type": "thinking", "thinking": "" }));
                append(block, "thinking", &text());
                true
            }
            "thinking_end" => {
                let block = self.block(index, || json!({ "type": "thinking", "thinking": "" }));
                if let Some(content) = content() {
                    block["thinking"] = Value::String(content);
                }
                true
            }
            "toolcall_start" => {
                let seed = delta.get("toolCall").cloned().unwrap_or_else(|| json!({}));
                let mut block = json!({ "type": "toolCall" });
                if let (Some(block), Value::Object(seed)) = (block.as_object_mut(), seed) {
                    block.extend(seed);
                }
                self.blocks.insert(index, block);
                true
            }
            "toolcall_delta" => {
                let block = self.block(index, || {
                    json!({ "type": "toolCall", "id": "", "name": "", "arguments": "" })
                });
                append(block, "arguments", &text());
                true
            }
            "toolcall_end" => {
                let Some(call) = delta.get("toolCall") else { return false };
                let mut block = call.clone();
                block["type"] = Value::String("toolCall".to_owned());
                self.blocks.insert(index, block);
                true
            }
            _ => false,
        }
    }

    fn block(&mut self, index: i64, seed: impl FnOnce() -> Value) -> &mut Value {
        self.blocks.entry(index).or_insert_with(seed)
    }

    /// The overlay as a message, rendered exactly as the persisted entry will
    /// be.
    pub fn message(&self) -> Option<Message> {
        let entry = Entry {
            seq: i64::MAX,
            id: self.run.clone(),
            parent_id: None,
            raw: json!({
                "type": "message",
                "id": self.run,
                "timestamp": self.started_ms,
                "message": {
                    "role": "assistant",
                    "model": self.model,
                    "content": self.blocks.values().cloned().collect::<Vec<_>>(),
                },
            }),
            mapped: None,
        };
        let mut message = pi_map::map_entry(&entry)?;
        // Not recorded as finished, because it is not: pi records completion
        // when it persists the entry.
        message.completed_ms = None;
        Some(message)
    }

    /// Whether the overlay has anything worth showing yet.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

fn append(block: &mut Value, field: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    match block.get_mut(field) {
        Some(Value::String(existing)) => existing.push_str(text),
        _ => block[field] = Value::String(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Block, Inline};
    use crate::model::{Part, ToolStatus};

    fn overlay() -> Overlay {
        Overlay::new("run-1".into(), Some("sonnet".into()), 1_787_479_200_000)
    }

    fn text_of(part: &Part) -> String {
        let (Part::Text { blocks } | Part::Reasoning { blocks }) = part else { panic!("{part:?}") };
        blocks
            .iter()
            .map(|b| match b {
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
    fn text_deltas_accumulate_into_one_part() {
        let mut o = overlay();
        o.apply(&json!({ "type": "text_start", "contentIndex": 0 }));
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "hel" }));
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "lo" }));
        let message = o.message().unwrap();
        assert_eq!(text_of(&message.parts[0]), "hello");
    }

    #[test]
    fn the_overlay_carries_its_run_id_as_its_message_id() {
        let mut o = overlay();
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "x" }));
        assert_eq!(o.message().unwrap().id, "run-1");
    }

    #[test]
    fn the_overlay_is_not_recorded_as_finished() {
        let mut o = overlay();
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "x" }));
        let message = o.message().unwrap();
        assert_eq!(message.completed_ms, None);
        assert_eq!(message.created_ms, Some(1_787_479_200_000));
    }

    #[test]
    fn an_end_event_replaces_what_the_deltas_accumulated() {
        // The authoritative content, so a dropped delta cannot leave the
        // settled text subtly wrong.
        let mut o = overlay();
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "partial" }));
        o.apply(&json!({ "type": "text_end", "contentIndex": 0, "content": "the whole thing" }));
        assert_eq!(text_of(&o.message().unwrap().parts[0]), "the whole thing");
    }

    #[test]
    fn thinking_maps_to_reasoning_not_to_prose() {
        // The file format names the field `thinking`; getting that wrong would
        // render private reasoning as the reply itself.
        let mut o = overlay();
        o.apply(&json!({ "type": "thinking_delta", "contentIndex": 0, "delta": "hmm" }));
        let message = o.message().unwrap();
        assert!(matches!(message.parts[0], Part::Reasoning { .. }), "{:?}", message.parts[0]);
        assert_eq!(text_of(&message.parts[0]), "hmm");
    }

    #[test]
    fn blocks_are_ordered_by_content_index_not_by_arrival() {
        // Tool calls and text interleave; arrival order is not the reply's
        // order.
        let mut o = overlay();
        o.apply(&json!({ "type": "text_delta", "contentIndex": 2, "delta": "second" }));
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "first" }));
        let message = o.message().unwrap();
        assert_eq!(text_of(&message.parts[0]), "first");
        assert_eq!(text_of(&message.parts[1]), "second");
    }

    #[test]
    fn a_tool_call_streams_its_arguments_and_settles_authoritatively() {
        let mut o = overlay();
        o.apply(&json!({
            "type": "toolcall_start", "contentIndex": 0,
            "toolCall": { "id": "c1", "name": "bash" }
        }));
        o.apply(&json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"cmd\":" }));
        let message = o.message().unwrap();
        let Part::Tool { call_id, name, status, .. } = &message.parts[0] else { panic!() };
        assert_eq!(call_id, "c1");
        assert_eq!(name, "bash");
        assert_eq!(*status, ToolStatus::Running);

        o.apply(&json!({
            "type": "toolcall_end", "contentIndex": 0,
            "toolCall": { "id": "c1", "name": "bash", "arguments": "{\"cmd\":\"ls\"}" }
        }));
        let Part::Tool { call_id, .. } = &o.message().unwrap().parts[0] else { panic!() };
        assert_eq!(call_id, "c1");
    }

    #[test]
    fn an_empty_overlay_has_nothing_to_show() {
        let o = overlay();
        assert!(o.is_empty());
        assert!(o.message().unwrap().parts.is_empty());
    }

    #[test]
    fn unknown_delta_types_change_nothing() {
        let mut o = overlay();
        assert!(!o.apply(&json!({ "type": "citation_delta", "contentIndex": 0 })));
        assert!(o.is_empty());
    }

    #[test]
    fn the_model_reaches_the_message_as_its_agent() {
        let mut o = overlay();
        o.apply(&json!({ "type": "text_delta", "contentIndex": 0, "delta": "x" }));
        assert_eq!(o.message().unwrap().agent.as_deref(), Some("sonnet"));
    }
}
