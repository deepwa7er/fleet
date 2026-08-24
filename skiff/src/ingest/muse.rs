//! The muse adapter: Muse Code session logs → the domain model.
//!
//! A muse session is a *directory* — `sessions/YYYY/MM/DD/<uuid>/` under
//! muse's data dir — holding an append-only, event-sourced `session.jsonl`.
//! Every line is an envelope:
//!
//! ```jsonc
//! { "id", "stream", "sequence", "recorded_at", "record_type",
//!   "payload_type", "payload" }
//! ```
//!
//! Three differences from pi shape everything below:
//!
//! - **The log is flat, not a tree.** There is no `parentId` and no branching,
//!   so the transcript is simply the records in order and the leaf-path walk
//!   does not apply. (`leaf_path` still works — a chain of entries whose
//!   parent is the one before is a degenerate tree — which is why nothing
//!   above the adapter has to know the difference.)
//! - **`recorded_at` is microseconds**, where pi writes ISO-8601.
//! - **The model in force is cumulative.** It is established by records
//!   scattered through the log, so an incremental read starting mid-file does
//!   not know it — hence the carried source state.
//!
//! Reasoning is persisted *encrypted* (`reasoning_committed` carries
//! `encrypted_content`), so it cannot be rendered and is skipped. muse names
//! its own sessions and offers no rename.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::source::{Discovered, ParsedBatch, Source};
use crate::content::parse;
use crate::model::{
    Capabilities, Entry, Harness, Message, Part, Role, SessionKey, SessionSummary, ToolStatus,
};

pub const SOURCE: &str = "muse";

/// muse names its own sessions, has no orchestrator, and takes no model
/// command — so the client offers none of those controls.
pub const CAPABILITIES: Capabilities =
    Capabilities { rename: false, orchestrator: false, model: false };

/// A single tool dump must not balloon a whole transcript. Same cap as pi.
const TOOL_OUTPUT_LIMIT: usize = 2_000;

/// A session with no name of its own is titled by its first prompt, as muse's
/// own session index does — so no real session ever reads "untitled".
const TITLE_FALLBACK: usize = 60;

/// muse resolves its data dir per the XDG spec, and skiff scans exactly what
/// muse writes.
pub fn default_session_dir() -> PathBuf {
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home().join(".local/share"),
    };
    data.join("muse").join("sessions")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub struct Muse {
    root: PathBuf,
}

impl Muse {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Source for Muse {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn harness(&self) -> Harness {
        Harness::Muse
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn discover(&self) -> Result<Vec<Discovered>> {
        Ok(session_files(&self.root)
            .into_iter()
            .filter_map(|path| {
                // The session id is the directory holding session.jsonl.
                let id = path.parent()?.file_name()?.to_str()?;
                Some(Discovered { key: SessionKey::new(Harness::Muse, id), path })
            })
            .collect())
    }

    fn parse(&self, lines: &[String], first_line: i64, state: Option<&Value>) -> ParsedBatch {
        let mut model = state.and_then(|s| s.get("model")).and_then(Value::as_str).map(str::to_owned);
        let mut entries = Vec::new();

        for (offset, line) in lines.iter().enumerate() {
            let seq = first_line + offset as i64;
            let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
            let Some(payload_type) = record.get("payload_type").and_then(Value::as_str) else {
                continue;
            };

            // Model records establish what later messages were produced by.
            if let Some(configured) = configured_model(&record, payload_type) {
                model = Some(configured);
            }

            entries.push(Entry {
                seq,
                // The record's own id, so an entry keeps its identity across a
                // re-read.
                id: record
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("r{seq}")),
                // A flat log: each record's parent is the one before it, which
                // makes the shared leaf-path walk a no-op rather than a
                // special case.
                parent_id: (seq > 0).then(|| previous_id(&entries, seq)),
                mapped: map_record(&record, payload_type, model.as_deref()),
                raw: record,
            });
        }

        ParsedBatch { entries, state: model.map(|model| json!({ "model": model })) }
    }

    fn summarize(
        &self,
        key: &SessionKey,
        state: Option<&Value>,
        entries: &[Entry],
    ) -> Option<SessionSummary> {
        // A session directory muse created but has not written to yet is not
        // a session to list.
        if entries.is_empty() {
            return None;
        }

        let mut title = None;
        let mut first_prompt = None;
        let mut directory = None;
        // The stored state is the model as of the last read, which is the
        // right starting point when this batch established none of its own.
        let mut model = state.and_then(|s| s.get("model")).and_then(Value::as_str).map(str::to_owned);

        for entry in entries {
            let Some(payload_type) = entry.raw.get("payload_type").and_then(Value::as_str) else {
                continue;
            };
            match payload_type {
                "session.name.changed" => {
                    if let Some(name) =
                        entry.raw.get("payload").and_then(|p| p.get("new_name")).and_then(Value::as_str)
                    {
                        title = Some(name.to_owned());
                    }
                }
                "runtime.session.metadata" => {
                    let record = entry.raw.get("payload").and_then(|p| p.get("record"));
                    if let Some(root) =
                        record.and_then(|r| r.get("workspace_root")).and_then(Value::as_str)
                    {
                        directory = Some(root.to_owned());
                    }
                }
                _ => {}
            }
            if let Some(configured) = configured_model(&entry.raw, payload_type) {
                model = Some(configured);
            }
            if first_prompt.is_none()
                && let Some(event) = run_event(&entry.raw)
                && event.get("kind").and_then(Value::as_str) == Some("started")
                && let Some(prompt) = event.get("prompt").and_then(Value::as_str)
                && !prompt.trim().is_empty()
            {
                first_prompt = Some(prompt.trim().to_owned());
            }
        }

        // TUI-created sessions carry no name record; muse's own index titles
        // those by their first prompt, and so does this.
        let title = title.or_else(|| first_prompt.map(truncate_title));

        let created = entries.first().and_then(|e| timestamp_ms(&e.raw));
        Some(SessionSummary {
            id: key.clone(),
            harness: Harness::Muse,
            capabilities: CAPABILITIES,
            title,
            directory,
            created_ms: created,
            updated_ms: entries.last().and_then(|e| timestamp_ms(&e.raw)).or(created),
            model,
            orchestrator_active: false,
        })
    }
}

fn previous_id(entries: &[Entry], seq: i64) -> String {
    entries
        .last()
        .filter(|previous| previous.seq < seq)
        .map(|previous| previous.id.clone())
        // The first entry of an incremental batch has no predecessor in this
        // batch. Naming the line before it reconnects the chain to what is
        // already stored.
        .unwrap_or_else(|| format!("r{}", seq - 1))
}

/// Every `session.jsonl`, at exactly the depth muse writes them.
///
/// The fixed depth is what excludes subagent sessions: they nest deeper
/// (`<uuid>/subagent/<child>/session.jsonl`) and are run internals, not
/// conversations to list.
fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for year in dirs(root) {
        for month in dirs(&year) {
            for day in dirs(&month) {
                for session in dirs(&day) {
                    let file = session.join("session.jsonl");
                    if file.is_file() {
                        out.push(file);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Subdirectories of `dir`. An unreadable directory contributes nothing rather
/// than failing the scan — one bad day must not hide every other session.
fn dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// The model a record puts in force, if it puts one in force.
fn configured_model(record: &Value, payload_type: &str) -> Option<String> {
    let field = match payload_type {
        "runtime.session.metadata" | "run.model.configured" => "record",
        _ => return None,
    };
    record
        .get("payload")?
        .get(field)?
        .get("model_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The run event a record wraps, if it wraps one.
///
/// The durable file wraps them as `payload_type: "runtime.session"` with
/// `payload.kind: "run"`. The same events appear *unwrapped* on
/// `muse exec --json` stdout — which is why the live path reads them
/// separately, and why this function only serves the file.
fn run_event(record: &Value) -> Option<&Value> {
    if record.get("payload_type").and_then(Value::as_str)? != "runtime.session" {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("kind").and_then(Value::as_str)? != "run" {
        return None;
    }
    payload.get("event")
}

/// `recorded_at` is microseconds since the epoch; the domain carries
/// milliseconds.
fn timestamp_ms(record: &Value) -> Option<i64> {
    Some(record.get("recorded_at")?.as_i64()? / 1000)
}

fn truncate_title(prompt: String) -> String {
    if prompt.chars().count() <= TITLE_FALLBACK {
        return prompt;
    }
    let cut: String = prompt.chars().take(TITLE_FALLBACK).collect();
    format!("{cut}…")
}

fn truncate_output(text: &str) -> String {
    if text.len() <= TOOL_OUTPUT_LIMIT {
        return text.to_owned();
    }
    let mut end = TOOL_OUTPUT_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Map one record to the message it renders as, or `None` when it renders
/// nothing — task lifecycle, context diagnostics, subagent control, cron, and
/// the encrypted reasoning records.
fn map_record(record: &Value, payload_type: &str, model: Option<&str>) -> Option<Message> {
    let _ = payload_type;
    let event = run_event(record)?;
    let created = timestamp_ms(record);
    let record_id = record.get("id").and_then(Value::as_str).unwrap_or_default();
    let message_id = |fallback: &str| {
        event
            .get("message_id")
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };

    match event.get("kind").and_then(Value::as_str)? {
        "started" => Some(Message {
            id: record_id.to_owned(),
            role: Role::User,
            agent: None,
            created_ms: created,
            completed_ms: created,
            parts: vec![Part::Text {
                blocks: parse(event.get("prompt").and_then(Value::as_str).unwrap_or_default()),
            }],
        }),
        "assistant_message_committed" => Some(Message {
            id: message_id(record_id),
            role: Role::Assistant,
            agent: model.map(str::to_owned),
            created_ms: created,
            completed_ms: created,
            parts: vec![Part::Text {
                blocks: parse(event.get("text").and_then(Value::as_str).unwrap_or_default()),
            }],
        }),
        "assistant_tool_calls_committed" => Some(Message {
            id: message_id(record_id),
            role: Role::Assistant,
            agent: model.map(str::to_owned),
            created_ms: created,
            completed_ms: created,
            parts: event
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .map(|call| Part::Tool {
                            call_id: call
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            status: ToolStatus::Running,
                            output: None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "tool_result_batch_committed" => Some(Message {
            id: record_id.to_owned(),
            role: Role::Tool,
            agent: None,
            created_ms: created,
            completed_ms: created,
            parts: event
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .map(|result| Part::Tool {
                            call_id: result
                                .get("tool_call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: String::new(),
                            status: ToolStatus::Completed,
                            output: Some(truncate_output(
                                result.get("text").and_then(Value::as_str).unwrap_or_default(),
                            )),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        // reasoning_committed carries `encrypted_content`: muse persists
        // reasoning encrypted, so there is nothing to render.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Block, Inline};

    /// One envelope, as muse writes it.
    fn record(id: &str, at_us: i64, payload_type: &str, payload: Value) -> String {
        serde_json::json!({
            "id": id,
            "stream": "session",
            "sequence": 1,
            "recorded_at": at_us,
            "record_type": "event",
            "payload_type": payload_type,
            "payload": payload,
        })
        .to_string()
    }

    /// A run event, wrapped the way the durable file wraps them.
    fn run(id: &str, at_us: i64, event: Value) -> String {
        record(id, at_us, "runtime.session", serde_json::json!({ "kind": "run", "event": event }))
    }

    fn muse() -> Muse {
        Muse::new(PathBuf::from("/nonexistent"))
    }

    fn parse_all(lines: &[String]) -> ParsedBatch {
        muse().parse(lines, 0, None)
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
    fn a_prompt_becomes_a_user_message() {
        let lines =
            vec![run("r1", 1_787_479_200_000_000, serde_json::json!({ "kind": "started", "prompt": "hello" }))];
        let parsed = parse_all(&lines);
        let message = parsed.entries[0].mapped.as_ref().unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(text_of(&message.parts[0]), "hello");
    }

    #[test]
    fn microsecond_timestamps_become_milliseconds() {
        // muse records microseconds; the domain carries milliseconds. Getting
        // this wrong would date every session a thousand years out.
        let lines =
            vec![run("r1", 1_787_479_200_000_000, serde_json::json!({ "kind": "started", "prompt": "x" }))];
        let message = parse_all(&lines).entries[0].mapped.clone().unwrap();
        assert_eq!(message.created_ms, Some(1_787_479_200_000));
    }

    #[test]
    fn an_assistant_message_carries_the_model_in_force() {
        let lines = vec![
            record("r1", 1, "runtime.session.metadata", serde_json::json!({
                "record": { "model_id": "muse-1", "workspace_root": "/w" }
            })),
            run("r2", 2, serde_json::json!({
                "kind": "assistant_message_committed", "message_id": "m1", "text": "the answer"
            })),
        ];
        let parsed = parse_all(&lines);
        let message = parsed.entries[1].mapped.as_ref().unwrap();
        assert_eq!(message.id, "m1");
        assert_eq!(message.agent.as_deref(), Some("muse-1"));
        assert_eq!(text_of(&message.parts[0]), "the answer");
    }

    #[test]
    fn a_later_model_record_takes_effect_for_later_messages_only() {
        let lines = vec![
            record("r1", 1, "runtime.session.metadata", serde_json::json!({ "record": { "model_id": "old" } })),
            run("r2", 2, serde_json::json!({ "kind": "assistant_message_committed", "text": "a" })),
            record("r3", 3, "run.model.configured", serde_json::json!({ "record": { "model_id": "new" } })),
            run("r4", 4, serde_json::json!({ "kind": "assistant_message_committed", "text": "b" })),
        ];
        let parsed = parse_all(&lines);
        let agent = |i: usize| parsed.entries[i].mapped.as_ref().unwrap().agent.clone();
        assert_eq!(agent(1).as_deref(), Some("old"));
        assert_eq!(agent(3).as_deref(), Some("new"), "the model in force moves forward");
    }

    #[test]
    fn the_model_in_force_survives_an_incremental_read() {
        // This is why the source carries state: the record that established
        // the model is behind the watermark, so a batch read from the middle
        // would otherwise attribute the message to nobody.
        let first = vec![record("r1", 1, "runtime.session.metadata", serde_json::json!({
            "record": { "model_id": "muse-1" }
        }))];
        let carried = parse_all(&first).state;
        assert_eq!(carried, Some(serde_json::json!({ "model": "muse-1" })));

        let second =
            vec![run("r2", 2, serde_json::json!({ "kind": "assistant_message_committed", "text": "x" }))];
        let parsed = muse().parse(&second, 1, carried.as_ref());
        assert_eq!(parsed.entries[0].mapped.as_ref().unwrap().agent.as_deref(), Some("muse-1"));
    }

    #[test]
    fn tool_calls_and_their_results_fold_through_the_shared_assembly() {
        let lines = vec![
            run("r1", 1, serde_json::json!({
                "kind": "assistant_tool_calls_committed", "message_id": "m1",
                "tool_calls": [{ "call_id": "c1", "name": "shell" }]
            })),
            run("r2", 2, serde_json::json!({
                "kind": "tool_result_batch_committed",
                "results": [{ "tool_call_id": "c1", "text": "output" }]
            })),
        ];
        let entries = parse_all(&lines).entries;
        // Folding is the shared, harness-agnostic step (views::session).
        let messages = crate::views::transcript(&entries);
        assert_eq!(messages.len(), 1, "the result folded into its call");
        let Part::Tool { call_id, name, status, output } = &messages[0].parts[0] else { panic!() };
        assert_eq!(call_id, "c1");
        assert_eq!(name, "shell");
        assert_eq!(*status, ToolStatus::Completed);
        assert_eq!(output.as_deref(), Some("output"));
    }

    #[test]
    fn encrypted_reasoning_renders_nothing() {
        // muse persists reasoning encrypted, so there is nothing to show —
        // and a record that rendered its ciphertext would be worse than none.
        let lines = vec![run("r1", 1, serde_json::json!({
            "kind": "reasoning_committed", "encrypted_content": "AAAA=="
        }))];
        assert_eq!(parse_all(&lines).entries[0].mapped, None);
    }

    #[test]
    fn records_with_no_transcript_rendering_map_to_nothing() {
        let lines = vec![
            record("r1", 1, "task.lifecycle.started", serde_json::json!({})),
            record("r2", 2, "context.diagnostics", serde_json::json!({})),
            record("r3", 3, "runtime.session.metadata", serde_json::json!({ "record": {} })),
        ];
        for entry in parse_all(&lines).entries {
            assert_eq!(entry.mapped, None, "{}", entry.id);
        }
    }

    #[test]
    fn the_flat_log_chains_so_the_shared_leaf_walk_keeps_every_record() {
        // muse has no branching. Chaining each record to the one before makes
        // the tree walk a no-op rather than a special case above the adapter.
        let lines: Vec<String> = (0..4)
            .map(|i| run(&format!("r{i}"), i, serde_json::json!({ "kind": "started", "prompt": "p" })))
            .collect();
        let entries = parse_all(&lines).entries;
        assert_eq!(crate::model::leaf_path(&entries).len(), 4);
    }

    #[test]
    fn an_incremental_batch_reconnects_to_what_is_already_stored() {
        // The first entry of a resumed batch has no predecessor in the batch;
        // naming the line before it keeps the chain unbroken, so the leaf walk
        // does not silently drop everything already ingested.
        let earlier = parse_all(&[run("r0", 0, serde_json::json!({ "kind": "started", "prompt": "a" }))]);
        let later = muse().parse(
            &[run("r1", 1, serde_json::json!({ "kind": "started", "prompt": "b" }))],
            1,
            None,
        );
        let mut all = earlier.entries;
        all.extend(later.entries);
        assert_eq!(crate::model::leaf_path(&all).len(), 2, "the chain survived the batch boundary");
    }

    #[test]
    fn a_named_session_uses_its_name() {
        let lines = vec![
            run("r1", 1, serde_json::json!({ "kind": "started", "prompt": "the first prompt" })),
            record("r2", 2, "session.name.changed", serde_json::json!({ "new_name": "a real name" })),
        ];
        let entries = parse_all(&lines).entries;
        let summary = muse().summarize(&"muse:s".parse().unwrap(), None, &entries).unwrap();
        assert_eq!(summary.title.as_deref(), Some("a real name"));
    }

    #[test]
    fn an_unnamed_session_is_titled_by_its_first_prompt() {
        // TUI-created sessions carry no name record; muse's own index titles
        // those by the first prompt, and so does this, so no real session ever
        // reads "untitled".
        let lines = vec![
            run("r1", 1, serde_json::json!({ "kind": "started", "prompt": "  fix the parser  " })),
            run("r2", 2, serde_json::json!({ "kind": "started", "prompt": "and then this" })),
        ];
        let entries = parse_all(&lines).entries;
        let summary = muse().summarize(&"muse:s".parse().unwrap(), None, &entries).unwrap();
        assert_eq!(summary.title.as_deref(), Some("fix the parser"), "trimmed, and the FIRST one");
    }

    #[test]
    fn a_long_first_prompt_is_truncated_on_a_character_boundary() {
        let prompt = "é".repeat(200);
        let lines = vec![run("r1", 1, serde_json::json!({ "kind": "started", "prompt": prompt }))];
        let entries = parse_all(&lines).entries;
        let summary = muse().summarize(&"muse:s".parse().unwrap(), None, &entries).unwrap();
        let title = summary.title.unwrap();
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), TITLE_FALLBACK + 1);
    }

    #[test]
    fn the_summary_reads_the_workspace_and_the_model() {
        let lines = vec![
            record("r1", 1_000, "runtime.session.metadata", serde_json::json!({
                "record": { "model_id": "muse-1", "workspace_root": "/w" }
            })),
            run("r2", 9_000, serde_json::json!({ "kind": "started", "prompt": "x" })),
        ];
        let entries = parse_all(&lines).entries;
        let summary = muse().summarize(&"muse:s".parse().unwrap(), None, &entries).unwrap();
        assert_eq!(summary.directory.as_deref(), Some("/w"));
        assert_eq!(summary.model.as_deref(), Some("muse-1"));
        assert_eq!(summary.created_ms, Some(1));
        assert_eq!(summary.updated_ms, Some(9));
        assert_eq!(summary.capabilities, CAPABILITIES);
        assert!(!summary.capabilities.rename, "muse names its own sessions");
    }

    #[test]
    fn a_session_directory_with_nothing_written_is_not_a_session() {
        assert_eq!(muse().summarize(&"muse:s".parse().unwrap(), None, &[]), None);
    }

    #[test]
    fn an_unparseable_line_is_skipped_without_shifting_its_neighbours() {
        let lines = vec![
            run("r0", 1, serde_json::json!({ "kind": "started", "prompt": "a" })),
            "{ half".to_owned(),
            run("r2", 3, serde_json::json!({ "kind": "started", "prompt": "b" })),
        ];
        let entries = parse_all(&lines).entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].seq, 2, "the corrupt line still consumes its slot");
    }

    #[test]
    fn discovery_finds_sessions_at_muses_dated_depth_and_skips_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let session = root.join("2026/08/23/abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("session.jsonl"), "").unwrap();

        // Subagent sessions nest one level deeper. They are run internals, not
        // conversations to list.
        let subagent = session.join("subagent/child");
        std::fs::create_dir_all(&subagent).unwrap();
        std::fs::write(subagent.join("session.jsonl"), "").unwrap();

        // A directory muse created but has not written to yet.
        std::fs::create_dir_all(root.join("2026/08/23/empty")).unwrap();

        let found = Muse::new(root.to_path_buf()).discover().unwrap();
        let ids: Vec<_> = found.iter().map(|d| d.key.to_string()).collect();
        assert_eq!(ids, ["muse:abc"]);
    }
}
