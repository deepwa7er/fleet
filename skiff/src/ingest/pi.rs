//! The pi adapter: pi's v3 JSONL session files → the domain model.
//!
//! This is the only module in skiffd that knows what pi writes. Everything it
//! exports is in the vocabulary of `crate::model` (DW-004 §3).
//!
//! A pi session file is an append-only *tree*: line 1 is a session header,
//! and every later line is an entry linked to its predecessor by
//! `id`/`parentId`. The conversation is the chain from the newest entry back
//! to the root — see `model::leaf_path`, and note that entries on abandoned
//! branches stay in the file forever and must never surface.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::source::{Discovered, ParsedBatch, Source};
use crate::model::{Capabilities, Entry, Harness, SessionKey, SessionSummary, leaf_path};

pub const SOURCE: &str = "pi";

/// pi can rename a session, toggle its orchestrator, and switch model.
pub const CAPABILITIES: Capabilities =
    Capabilities { rename: true, orchestrator: true, model: true };

/// Where pi keeps its sessions, resolved the way pi itself resolves it so the
/// default deployment reads exactly what pi writes.
pub fn default_session_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PI_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    home().join(".pi").join("agent").join("sessions")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// A session id is the file's basename — pi's own naming, and the reason
/// resolution is a cheap name walk rather than a content scan.
pub fn key_for_file(path: &Path) -> Option<SessionKey> {
    let stem = path.file_stem()?.to_str()?;
    if stem.is_empty() {
        return None;
    }
    Some(SessionKey::new(Harness::Pi, stem))
}

/// Every `*.jsonl` under `dir`, recursively.
///
/// The layout depends on how pi was invoked — per-cwd subdirectories by
/// default, flat under `--session-dir` — so the walk must handle both. A
/// directory that cannot be read contributes nothing rather than failing the
/// scan: one unreadable subdirectory must not hide every other session.
pub fn session_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            walk(&path, out)?;
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

/// The pi adapter.
pub struct Pi {
    root: PathBuf,
}

impl Pi {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Source for Pi {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn harness(&self) -> Harness {
        Harness::Pi
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn discover(&self) -> Result<Vec<Discovered>> {
        Ok(session_files(&self.root)?
            .into_iter()
            .filter_map(|path| key_for_file(&path).map(|key| Discovered { key, path }))
            .collect())
    }

    fn parse(&self, lines: &[String], first_line: i64, state: Option<&Value>) -> ParsedBatch {
        let parsed = parse_lines(lines, first_line);
        ParsedBatch {
            entries: parsed.entries,
            // The header is line 1 only, so a resumed read has nothing new to
            // say about it and must not overwrite what is stored.
            state: parsed.header.map(|header| json!({ "header": header })),
        }
        .keeping(state)
    }

    fn summarize(
        &self,
        key: &SessionKey,
        state: Option<&Value>,
        entries: &[Entry],
    ) -> Option<SessionSummary> {
        // A `.jsonl` file with no session header is not a pi session — it is
        // some other file sharing the extension, or one pi created but has not
        // written yet. Either way it must not appear in the list.
        let header = state?.get("header")?;
        Some(summarize(key, Some(header), entries))
    }
}

impl ParsedBatch {
    /// Carry the stored state forward when this batch produced none.
    fn keeping(mut self, previous: Option<&Value>) -> Self {
        if self.state.is_none() {
            self.state = previous.cloned();
        }
        self
    }
}

/// What one batch of lines contained.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    /// The session header, present only in a batch that includes line 1.
    pub header: Option<Value>,
    pub entries: Vec<Entry>,
}

/// Parse a batch of lines whose first line is at index `first_line`.
///
/// `seq` is the line's index in the file, which makes it stable across reads:
/// re-reading a file from zero assigns every entry the same `seq` it had
/// before, so the store converges instead of duplicating.
///
/// Unparseable lines are skipped, not fatal. A session file is written by a
/// live process, and a corrupt or half-written line is a normal event; the
/// line's slot in the numbering is still consumed, so the entries around it
/// keep their identities.
pub fn parse_lines(lines: &[String], first_line: i64) -> Parsed {
    let mut parsed = Parsed::default();
    for (offset, line) in lines.iter().enumerate() {
        let seq = first_line + offset as i64;
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            // Line 1. Keep the first one seen: a file with two headers is
            // malformed, and the earlier one is the session's own.
            parsed.header.get_or_insert(value);
            continue;
        }
        let Some(id) = value.get("id").and_then(Value::as_str) else { continue };
        let mut entry = Entry {
            seq,
            id: id.to_owned(),
            parent_id: value.get("parentId").and_then(Value::as_str).map(str::to_owned),
            raw: value,
            mapped: None,
        };
        // Rendered here, once, because this is the only moment the entry is
        // new. See `Entry::mapped`.
        entry.mapped = super::pi_map::map_entry(&entry);
        parsed.entries.push(entry);
    }
    parsed
}

/// Derive the session summary from the header and the full entry list.
///
/// `entries` must be every entry for the session, ordered by `seq` — the
/// summary reads the *leaf path* for anything the conversation determines, and
/// a partial list would describe a different conversation.
pub fn summarize(key: &SessionKey, header: Option<&Value>, entries: &[Entry]) -> SessionSummary {
    let path = leaf_path(entries);
    let created_ms = header.and_then(timestamp_ms);

    SessionSummary {
        id: key.clone(),
        harness: Harness::Pi,
        capabilities: CAPABILITIES,
        title: latest(&path, |e| {
            (e.raw.get("type").and_then(Value::as_str) == Some("session_info"))
                .then(|| e.raw.get("name").and_then(Value::as_str))
                .flatten()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        }),
        directory: header
            .and_then(|h| h.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        created_ms,
        // Activity is a property of the file, not of the conversation: an
        // entry on an abandoned branch still means pi wrote just now.
        updated_ms: entries.last().and_then(|e| timestamp_ms(&e.raw)).or(created_ms),
        model: model(&path),
        orchestrator_active: latest(&path, |e| {
            (e.raw.get("type").and_then(Value::as_str) == Some("custom")
                && e.raw.get("customType").and_then(Value::as_str) == Some("orchestrator-mode"))
            .then(|| e.raw.get("data")?.get("active")?.as_bool())
            .flatten()
        })
        .unwrap_or(false),
    }
}

/// The session's model: the last explicit switch on the branch, or failing
/// that the model of the last assistant message on it.
fn model(path: &[&Entry]) -> Option<String> {
    let switched = latest(path, |e| {
        (e.raw.get("type").and_then(Value::as_str) == Some("model_change"))
            .then(|| e.raw.get("modelId").and_then(Value::as_str))
            .flatten()
            .map(str::to_owned)
    });
    switched.or_else(|| {
        latest(path, |e| {
            let message = e.raw.get("message")?;
            (message.get("role").and_then(Value::as_str) == Some("assistant"))
                .then(|| message.get("model").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
    })
}

/// The most recent entry on the branch for which `f` answers, walking leaf
/// → root so the first match is the latest.
fn latest<T>(path: &[&Entry], f: impl Fn(&Entry) -> Option<T>) -> Option<T> {
    path.iter().rev().find_map(|entry| f(entry))
}

/// Entry timestamps are ISO-8601 strings in the file; a Unix-ms number is
/// tolerated because the message payload carries one in that form.
pub(super) fn timestamp_ms(value: &Value) -> Option<i64> {
    match value.get("timestamp")? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => iso8601_ms(s),
        _ => None,
    }
}

/// Parse the ISO-8601 form pi writes (`2026-08-23T10:11:12.345Z`) to epoch
/// milliseconds.
///
/// Deliberately narrow: pi writes UTC with a `Z`, and accepting offsets or
/// local times here would mean silently mis-ordering a transcript rather than
/// visibly declining to date it.
fn iso8601_ms(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let time = rest.strip_suffix('Z')?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (clock, millis) = match time.split_once('.') {
        Some((clock, frac)) => {
            let digits: String = frac.chars().take(3).collect();
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            // ".5" is 500ms, not 5ms.
            (clock, digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32))
        }
        None => (time, 0),
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    Some((days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second) * 1000
        + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so dating a transcript needs no calendar dependency.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_session_key_comes_from_the_file_basename() {
        assert_eq!(
            key_for_file(Path::new("/s/--home-x--/abc123.jsonl")),
            Some(SessionKey::new(Harness::Pi, "abc123"))
        );
    }

    #[test]
    fn parse_splits_the_header_from_the_entries() {
        let parsed = parse_lines(
            &lines(&[
                r#"{"type":"session","cwd":"/home/x","timestamp":"2026-08-23T10:00:00.000Z"}"#,
                r#"{"id":"a","timestamp":"2026-08-23T10:00:01.000Z"}"#,
                r#"{"id":"b","parentId":"a"}"#,
            ]),
            0,
        );
        assert_eq!(parsed.header.unwrap().get("cwd").unwrap(), "/home/x");
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[1].parent_id.as_deref(), Some("a"));
    }

    #[test]
    fn seq_is_the_line_index_so_a_re_read_converges() {
        let all = lines(&[r#"{"type":"session"}"#, r#"{"id":"a"}"#, r#"{"id":"b"}"#]);
        let full = parse_lines(&all, 0);
        let resumed = parse_lines(&all[2..], 2);
        assert_eq!(full.entries[1].seq, 2);
        assert_eq!(resumed.entries[0].seq, 2, "the same line must keep the same seq");
    }

    #[test]
    fn an_unparseable_line_is_skipped_without_shifting_its_neighbours() {
        let parsed = parse_lines(&lines(&[r#"{"id":"a"}"#, "{ half", r#"{"id":"c"}"#]), 0);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[1].seq, 2, "the corrupt line still consumes its slot");
    }

    #[test]
    fn a_line_without_a_string_id_is_not_an_entry() {
        let parsed = parse_lines(&lines(&[r#"{"type":"label","value":1}"#, r#"{"id":7}"#]), 0);
        assert!(parsed.entries.is_empty());
    }

    fn entry(seq: i64, id: &str, parent: Option<&str>, raw: Value) -> Entry {
        Entry { seq, id: id.to_owned(), parent_id: parent.map(str::to_owned), raw, mapped: None }
    }

    #[test]
    fn the_summary_reads_the_header_and_the_leaf_branch() {
        let header = serde_json::json!({
            "type": "session", "cwd": "/home/x", "timestamp": "2026-08-23T10:00:00.000Z"
        });
        let entries = vec![
            entry(1, "a", None, serde_json::json!({"id":"a","type":"session_info","name":"old"})),
            entry(2, "b", Some("a"), serde_json::json!({"id":"b","type":"session_info","name":"new"})),
            entry(
                3,
                "c",
                Some("b"),
                serde_json::json!({"id":"c","timestamp":"2026-08-23T10:05:00.000Z"}),
            ),
        ];
        let summary = summarize(&"pi:s".parse().unwrap(), Some(&header), &entries);

        assert_eq!(summary.title.as_deref(), Some("new"), "the latest name on the branch wins");
        assert_eq!(summary.directory.as_deref(), Some("/home/x"));
        assert_eq!(summary.created_ms, Some(1_787_479_200_000));
        assert_eq!(summary.updated_ms, Some(1_787_479_500_000));
        assert_eq!(summary.capabilities, CAPABILITIES);
    }

    #[test]
    fn the_summary_ignores_names_on_abandoned_branches() {
        let entries = vec![
            entry(1, "a", None, serde_json::json!({"id":"a"})),
            entry(2, "b", Some("a"), serde_json::json!({"id":"b","type":"session_info","name":"abandoned"})),
            entry(3, "c", Some("a"), serde_json::json!({"id":"c"})),
        ];
        let summary = summarize(&"pi:s".parse().unwrap(), None, &entries);
        assert_eq!(summary.title, None, "b is not on the leaf branch");
    }

    #[test]
    fn activity_counts_writes_on_any_branch() {
        // `b` was abandoned, but pi still wrote it just now — the session is
        // active, and the list must order it as such.
        let entries = vec![
            entry(1, "a", None, serde_json::json!({"id":"a","timestamp":"2026-08-23T10:00:00.000Z"})),
            entry(2, "b", Some("a"), serde_json::json!({"id":"b","timestamp":"2026-08-23T11:00:00.000Z"})),
        ];
        let summary = summarize(&"pi:s".parse().unwrap(), None, &entries);
        assert_eq!(summary.updated_ms, Some(1_787_482_800_000));
    }

    #[test]
    fn an_explicit_model_change_beats_the_last_assistant_message() {
        let entries = vec![
            entry(
                1,
                "a",
                None,
                serde_json::json!({"id":"a","type":"message","message":{"role":"assistant","model":"old"}}),
            ),
            entry(2, "b", Some("a"), serde_json::json!({"id":"b","type":"model_change","modelId":"new"})),
        ];
        let summary = summarize(&"pi:s".parse().unwrap(), None, &entries);
        assert_eq!(summary.model.as_deref(), Some("new"));
    }

    #[test]
    fn the_model_falls_back_to_the_last_assistant_message() {
        let entries = vec![entry(
            1,
            "a",
            None,
            serde_json::json!({"id":"a","type":"message","message":{"role":"assistant","model":"sonnet"}}),
        )];
        let summary = summarize(&"pi:s".parse().unwrap(), None, &entries);
        assert_eq!(summary.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn the_orchestrator_reads_the_last_toggle_on_the_branch() {
        let on = serde_json::json!({
            "id":"a","type":"custom","customType":"orchestrator-mode","data":{"active":true}
        });
        let off = serde_json::json!({
            "id":"b","type":"custom","customType":"orchestrator-mode","data":{"active":false}
        });
        let key: SessionKey = "pi:s".parse().unwrap();

        let entries = vec![entry(1, "a", None, on.clone())];
        assert!(summarize(&key, None, &entries).orchestrator_active);

        let entries = vec![entry(1, "a", None, on), entry(2, "b", Some("a"), off)];
        assert!(!summarize(&key, None, &entries).orchestrator_active);
    }

    #[test]
    fn an_empty_session_summarises_to_the_header_alone() {
        let header = serde_json::json!({
            "type":"session","cwd":"/home/x","timestamp":"2026-08-23T10:00:00.000Z"
        });
        let summary = summarize(&"pi:s".parse().unwrap(), Some(&header), &[]);
        assert_eq!(summary.updated_ms, summary.created_ms, "never written is never updated");
        assert_eq!(summary.title, None);
        assert_eq!(summary.model, None);
    }

    #[test]
    fn iso_timestamps_parse_to_epoch_millis() {
        assert_eq!(iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(iso8601_ms("2026-08-23T10:00:00.000Z"), Some(1_787_479_200_000));
        assert_eq!(iso8601_ms("2026-08-23T10:00:00Z"), Some(1_787_479_200_000));
        assert_eq!(iso8601_ms("2026-08-23T10:00:00.5Z"), Some(1_787_479_200_500));
        assert_eq!(iso8601_ms("2024-02-29T00:00:00.000Z"), Some(1_709_164_800_000));
    }

    #[test]
    fn timestamps_that_are_not_utc_iso_decline_rather_than_guess() {
        assert_eq!(iso8601_ms("2026-08-23T10:00:00+02:00"), None);
        assert_eq!(iso8601_ms("2026-08-23 10:00:00Z"), None);
        assert_eq!(iso8601_ms("2026-13-01T00:00:00.000Z"), None);
        assert_eq!(iso8601_ms(""), None);
    }

    #[test]
    fn a_numeric_timestamp_is_taken_as_epoch_millis() {
        let value = serde_json::json!({ "timestamp": 1_787_479_200_000i64 });
        assert_eq!(timestamp_ms(&value), Some(1_787_479_200_000));
    }
}
