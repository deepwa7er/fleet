//! The domain model — harness-agnostic, and the only vocabulary anything
//! above the ingest layer speaks (DW-004 §3).
//!
//! Native harness formats die in `ingest`; nothing here knows that pi writes
//! a tree of `parentId`-linked entries or that muse names its own sessions.
//!
//! ## Integers on the wire
//!
//! Several fields below are `i64` in Rust and annotated `#[ts(type =
//! "number")]`. ts-rs defaults 64-bit integers to `bigint`, which would be a
//! lie here: `serde_json` writes a JSON number and `JSON.parse` produces a
//! `number`, so a `bigint` never actually arrives. The annotation is safe
//! because every such field is an epoch-millisecond timestamp or a counter,
//! and `Number.MAX_SAFE_INTEGER` milliseconds is roughly 285,000 years.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which agent CLI owns a session. Session ids are harness-qualified on the
/// wire (`pi:abc`), because two harnesses may hand out the same local id and
/// an unqualified id would silently resolve to the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "gen/")]
pub enum Harness {
    Pi,
    Muse,
    Opencode,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Pi => "pi",
            Harness::Muse => "muse",
            Harness::Opencode => "opencode",
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pi" => Ok(Harness::Pi),
            "muse" => Ok(Harness::Muse),
            "opencode" => Ok(Harness::Opencode),
            _ => Err(()),
        }
    }
}

/// A session's identity: its harness and that harness's own local id.
///
/// The wire form is `<harness>:<local>`. Parsing is strict — an unprefixed or
/// unknown-prefix id is not "probably pi", it is an unknown session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey {
    pub harness: Harness,
    pub local: String,
}

impl SessionKey {
    pub fn new(harness: Harness, local: impl Into<String>) -> Self {
        Self { harness, local: local.into() }
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.harness, self.local)
    }
}

impl FromStr for SessionKey {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // split_once, not splitn-and-hope: a local id may itself contain ':'.
        let (prefix, local) = s.split_once(':').ok_or(())?;
        if local.is_empty() {
            return Err(());
        }
        Ok(SessionKey { harness: prefix.parse()?, local: local.to_owned() })
    }
}

impl Serialize for SessionKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(|()| serde::de::Error::custom(format!("not a session id: {raw}")))
    }
}

/// On the wire a `SessionKey` is its `harness:local` string, so in TypeScript
/// it is `string` — declared nowhere, inlined everywhere, exactly as ts-rs
/// treats its own primitives.
impl TS for SessionKey {
    type WithoutGenerics = Self;

    fn name() -> String {
        "string".to_owned()
    }

    fn inline() -> String {
        Self::name()
    }

    fn inline_flattened() -> String {
        panic!("a session id is a string and cannot be flattened")
    }

    fn decl() -> String {
        panic!("a session id is a string and is never declared")
    }

    fn decl_concrete() -> String {
        panic!("a session id is a string and is never declared")
    }
}

/// What a harness can actually do, so the client renders exactly the controls
/// that session supports rather than offering verbs that will 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct Capabilities {
    pub rename: bool,
    pub orchestrator: bool,
    pub model: bool,
}

/// One row of the session list: everything the desk and the session list need
/// without touching the transcript.
///
/// Every field here is *derived* from the harness's own files. Nothing in this
/// struct is authored by skiff, so it is always safe to throw away and rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct SessionSummary {
    pub id: SessionKey,
    pub harness: Harness,
    pub capabilities: Capabilities,
    /// The session's name, when its harness has one. muse names its own and
    /// offers no rename, so this can be present but not editable.
    pub title: Option<String>,
    /// The working directory the session runs in, when the harness records it.
    pub directory: Option<String>,
    /// Milliseconds since the epoch. `i64`, not `u64`: JSON numbers are
    /// signed, and a clock-skewed pre-epoch timestamp should read as absurd
    /// rather than wrap to the far future.
    #[ts(type = "number | null")]
    pub created_ms: Option<i64>,
    #[ts(type = "number | null")]
    pub updated_ms: Option<i64>,
    pub model: Option<String>,
    pub orchestrator_active: bool,
}

/// A source that could not be read, surfaced rather than swallowed.
///
/// DW-004 §4: a harness whose binary or session directory is missing degrades
/// to a named error attached to that source. It is never a dead service and
/// never a silently short session list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct SourceHealth {
    pub source: String,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub checked_ms: i64,
}

/// One entry as it sits in a harness's session file: opaque payload, plus the
/// structural fields the tree walk needs.
///
/// The raw line is kept so the store can re-derive any projection without
/// re-reading (or re-finding) the file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Position in the file. Monotonic per session, assigned at ingest — the
    /// tree walk starts from the highest.
    pub seq: i64,
    pub id: String,
    pub parent_id: Option<String>,
    pub raw: serde_json::Value,
}

/// The conversation: the chain from the newest entry back to the root, in
/// chronological order (DW-004 §4).
///
/// A session file is a *tree*, not a log. Entries on abandoned branches stay
/// in the file forever and must never surface, so "the transcript" is this
/// walk and not "the entries in file order".
///
/// `entries` must be ordered by `seq` ascending. The cycle guard is not
/// defensive programming for its own sake: a corrupt `parentId` chain is a
/// plausible outcome of a crash mid-write, and it must not hang the reader.
pub fn leaf_path(entries: &[Entry]) -> Vec<&Entry> {
    let Some(leaf) = entries.last() else {
        return Vec::new();
    };
    let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(leaf);
    while let Some(entry) = current {
        if !seen.insert(entry.id.as_str()) {
            break;
        }
        chain.push(entry);
        current = entry.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: i64, id: &str, parent: Option<&str>) -> Entry {
        Entry {
            seq,
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            raw: serde_json::Value::Null,
        }
    }

    fn ids(path: Vec<&Entry>) -> Vec<&str> {
        path.into_iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn session_key_round_trips_through_its_wire_form() {
        let key: SessionKey = "pi:abc".parse().unwrap();
        assert_eq!(key, SessionKey::new(Harness::Pi, "abc"));
        assert_eq!(key.to_string(), "pi:abc");
    }

    #[test]
    fn session_key_keeps_colons_inside_the_local_id() {
        let key: SessionKey = "opencode:ses:1:2".parse().unwrap();
        assert_eq!(key.local, "ses:1:2");
    }

    #[test]
    fn session_key_rejects_unprefixed_and_unknown_prefixes() {
        assert!("abc".parse::<SessionKey>().is_err());
        assert!("claude:abc".parse::<SessionKey>().is_err());
        assert!("pi:".parse::<SessionKey>().is_err());
    }

    #[test]
    fn leaf_path_is_the_chain_from_the_newest_entry() {
        let entries =
            vec![entry(1, "a", None), entry(2, "b", Some("a")), entry(3, "c", Some("b"))];
        assert_eq!(ids(leaf_path(&entries)), ["a", "b", "c"]);
    }

    #[test]
    fn leaf_path_excludes_abandoned_branches() {
        // `b` was abandoned; `c` branched from `a` instead. The transcript is
        // a → c, and `b` must not surface even though it is still in the file.
        let entries =
            vec![entry(1, "a", None), entry(2, "b", Some("a")), entry(3, "c", Some("a"))];
        assert_eq!(ids(leaf_path(&entries)), ["a", "c"]);
    }

    #[test]
    fn leaf_path_stops_at_a_missing_parent_rather_than_inventing_a_root() {
        let entries = vec![entry(1, "b", Some("gone")), entry(2, "c", Some("b"))];
        assert_eq!(ids(leaf_path(&entries)), ["b", "c"]);
    }

    #[test]
    fn leaf_path_survives_a_corrupt_parent_cycle() {
        let entries = vec![entry(1, "a", Some("b")), entry(2, "b", Some("a"))];
        // Terminates, and returns the chain it could walk before repeating.
        assert_eq!(ids(leaf_path(&entries)), ["a", "b"]);
    }

    #[test]
    fn leaf_path_of_an_empty_session_is_empty() {
        assert!(leaf_path(&[]).is_empty());
    }
}
