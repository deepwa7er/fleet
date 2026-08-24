//! The derived read model (DW-004 §5).
//!
//! Everything in this database is derived from files some other program owns.
//! Nothing here is authored by skiff, so two problems that usually make a
//! cache painful simply do not arise:
//!
//! - **Migrations.** The schema carries a version in `PRAGMA user_version`; on
//!   mismatch every table is dropped and re-ingested. There are no migration
//!   scripts and there never will be.
//! - **Corruption.** "Throw it away and rebuild" is always a legal answer.
//!
//! Authored state — annotations, round notes, the change state machine — does
//! *not* live here. It lives in the append-only change log, which this store
//! only ever holds a projection of.
//!
//! `rusqlite` is synchronous, so every method here is synchronous. Callers in
//! an async context must run them on a blocking task; nothing in this module
//! holds the connection across an await point, because it cannot.

mod schema;

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Capabilities, Entry, Harness, SessionKey, SessionSummary, SourceHealth};

pub use schema::SCHEMA_VERSION;

/// Where a source left off in one file.
///
/// The identity is `(inode, offset)`: a file whose inode changed is a
/// different file wearing the same name, and a file shorter than the offset
/// was rewritten. Either way the cursor is meaningless and the read restarts
/// from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub inode: u64,
    pub offset: u64,
    pub lines: i64,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open (or create) the store at `path`, rebuilding it if the schema
    /// version on disk is not the one this binary knows.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating the store directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening the store at {}", path.display()))?;
        Self::from_connection(conn)
    }

    /// An in-memory store, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL so a long read (a view recompute) never blocks the ingest
        // writer. NORMAL sync is right for derived data: losing the last
        // transaction to a power cut costs a re-ingest, not a fact.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::ensure(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run `f` with the connection. The lock is poisoned only if a previous
    /// caller panicked mid-statement; that leaves the database consistent (the
    /// transaction rolls back) but the process's view of it suspect, so the
    /// panic is propagated rather than papered over.
    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().expect("store connection poisoned by a panicking caller");
        f(&conn)
    }

    // --- cursors ------------------------------------------------------------

    pub fn cursor(&self, source: &str, key: &str) -> Result<Option<Cursor>> {
        self.with(|conn| {
            let row = conn
                .query_row(
                    "SELECT inode, offset, lines FROM source_cursor WHERE source = ?1 AND key = ?2",
                    params![source, key],
                    |row| {
                        Ok(Cursor {
                            inode: row.get::<_, i64>(0)? as u64,
                            offset: row.get::<_, i64>(1)? as u64,
                            lines: row.get(2)?,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
    }

    pub fn set_cursor(&self, source: &str, key: &str, cursor: Cursor) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT INTO source_cursor (source, key, inode, offset, lines)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source, key) DO UPDATE
                   SET inode = excluded.inode, offset = excluded.offset, lines = excluded.lines",
                params![source, key, cursor.inode as i64, cursor.offset as i64, cursor.lines],
            )?;
            Ok(())
        })
    }

    // --- entries ------------------------------------------------------------

    /// Persist one source's read of a session: its summary, optionally its
    /// source state, and any new entries — in a single transaction.
    ///
    /// Taking the summary as an argument rather than recomputing it here keeps
    /// this module free of harness knowledge: the adapter derives the summary
    /// from the same entries it just parsed, and the store only persists.
    pub fn ingest_session(&self, batch: SessionIngest<'_>) -> Result<()> {
        self.with(|conn| {
            let tx = conn.unchecked_transaction()?;
            // The summary row first: `entry` references it, and foreign keys
            // are on.
            upsert_summary(&tx, batch.summary, batch.state)?;
            {
                let mut insert = tx.prepare_cached(
                    "INSERT INTO entry (session_id, seq, id, parent_id, raw, mapped)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(session_id, seq) DO UPDATE
                       SET id = excluded.id,
                           parent_id = excluded.parent_id,
                           raw = excluded.raw,
                           mapped = excluded.mapped",
                )?;
                let id = batch.summary.id.to_string();
                for entry in batch.entries {
                    let mapped = entry
                        .mapped
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .context("serialising a rendered message")?;
                    insert.execute(params![
                        id,
                        entry.seq,
                        entry.id,
                        entry.parent_id,
                        entry.raw.to_string(),
                        mapped,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Drop every entry for a session, keeping its summary row. Used when a
    /// file was rewritten under the same name and the read restarts from zero.
    pub fn clear_entries(&self, session: &SessionKey) -> Result<()> {
        self.with(|conn| {
            conn.execute("DELETE FROM entry WHERE session_id = ?1", params![session.to_string()])?;
            Ok(())
        })
    }

    /// What the source last asked to remember about this session.
    pub fn source_state(&self, session: &SessionKey) -> Result<Option<serde_json::Value>> {
        self.with(|conn| {
            let raw: Option<Option<String>> = conn
                .query_row(
                    "SELECT source_state FROM session WHERE id = ?1",
                    params![session.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(raw.flatten().and_then(|raw| serde_json::from_str(&raw).ok()))
        })
    }

    /// Every entry for a session, ordered by `seq` — the input `leaf_path`
    /// expects.
    pub fn entries(&self, session: &SessionKey) -> Result<Vec<Entry>> {
        self.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT seq, id, parent_id, raw, mapped FROM entry
                 WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![session.to_string()], |row| {
                let raw: String = row.get(3)?;
                let mapped: Option<String> = row.get(4)?;
                Ok(Entry {
                    seq: row.get(0)?,
                    id: row.get(1)?,
                    parent_id: row.get(2)?,
                    // A row this process wrote as JSON is not expected to fail
                    // to parse; if it does the store is corrupt, and an empty
                    // value is a truthful "this entry carries nothing" rather
                    // than a crash that takes the whole session list down.
                    raw: serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null),
                    mapped: mapped.and_then(|m| serde_json::from_str(&m).ok()),
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Forget a session entirely — its entries and its summary row. Used when
    /// a session file disappears.
    pub fn forget_session(&self, session: &SessionKey) -> Result<()> {
        self.with(|conn| {
            let id = session.to_string();
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM entry WHERE session_id = ?1", params![id])?;
            tx.execute("DELETE FROM session WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
    }

    // --- sessions -----------------------------------------------------------

    /// The session list, most recently active first.
    pub fn sessions(&self) -> Result<Vec<SessionSummary>> {
        self.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, harness, title, directory, created_ms, updated_ms, model,
                        orchestrator_active, cap_rename, cap_orchestrator, cap_model
                 FROM session
                 ORDER BY COALESCE(updated_ms, created_ms, 0) DESC, id",
            )?;
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let harness: String = row.get(1)?;
                Ok((id, harness, row_summary(row)?))
            })?;

            let mut out = Vec::new();
            for row in rows {
                let (id, harness, rest) = row?;
                // A row whose id or harness no longer parses was written by a
                // different schema version; the store is derived, so skipping
                // it is correct and the next rebuild removes it.
                let (Ok(id), Ok(harness)) = (id.parse(), harness.parse::<Harness>()) else {
                    continue;
                };
                out.push(rest(id, harness));
            }
            Ok(out)
        })
    }

    // --- source health ------------------------------------------------------

    pub fn set_source_health(&self, health: &SourceHealth) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT INTO source_health (source, error, checked_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(source) DO UPDATE
                   SET error = excluded.error, checked_ms = excluded.checked_ms",
                params![health.source, health.error, health.checked_ms],
            )?;
            Ok(())
        })
    }

    pub fn source_health(&self) -> Result<Vec<SourceHealth>> {
        self.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT source, error, checked_ms FROM source_health ORDER BY source",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(SourceHealth {
                    source: row.get(0)?,
                    error: row.get(1)?,
                    checked_ms: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}

/// The half of a summary row that does not depend on the parsed id/harness,
/// returned as a constructor so the caller can skip unparseable rows without
/// having read the rest twice.
type SummaryRest = Box<dyn FnOnce(SessionKey, Harness) -> SessionSummary>;

fn row_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRest> {
    let title: Option<String> = row.get(2)?;
    let directory: Option<String> = row.get(3)?;
    let created_ms: Option<i64> = row.get(4)?;
    let updated_ms: Option<i64> = row.get(5)?;
    let model: Option<String> = row.get(6)?;
    let orchestrator_active: bool = row.get(7)?;
    let capabilities =
        Capabilities { rename: row.get(8)?, orchestrator: row.get(9)?, model: row.get(10)? };
    Ok(Box::new(move |id, harness| SessionSummary {
        id,
        harness,
        capabilities,
        title,
        directory,
        created_ms,
        updated_ms,
        model,
        orchestrator_active,
    }))
}

/// One source's read of one session, persisted atomically.
pub struct SessionIngest<'a> {
    pub summary: &'a SessionSummary,
    /// What the source wants remembered for the next read. `None` leaves the
    /// stored value alone, which is what an incremental read that learned
    /// nothing new should do.
    pub state: Option<&'a serde_json::Value>,
    pub entries: &'a [Entry],
}

fn upsert_summary(
    conn: &Connection,
    s: &SessionSummary,
    state: Option<&serde_json::Value>,
) -> Result<()> {
    // COALESCE, not excluded: a read that learned nothing new passes `None`,
    // and must not blank what an earlier read stored.
    conn.execute(
        "INSERT INTO session (id, harness, title, directory, created_ms, updated_ms, model,
                              orchestrator_active, source_state,
                              cap_rename, cap_orchestrator, cap_model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
           harness = excluded.harness,
           title = excluded.title,
           directory = excluded.directory,
           created_ms = excluded.created_ms,
           updated_ms = excluded.updated_ms,
           model = excluded.model,
           orchestrator_active = excluded.orchestrator_active,
           source_state = COALESCE(excluded.source_state, session.source_state),
           cap_rename = excluded.cap_rename,
           cap_orchestrator = excluded.cap_orchestrator,
           cap_model = excluded.cap_model",
        params![
            s.id.to_string(),
            s.harness.as_str(),
            s.title,
            s.directory,
            s.created_ms,
            s.updated_ms,
            s.model,
            s.orchestrator_active,
            state.map(|s| s.to_string()),
            s.capabilities.rename,
            s.capabilities.orchestrator,
            s.capabilities.model,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, updated: i64) -> SessionSummary {
        SessionSummary {
            id: id.parse().unwrap(),
            harness: Harness::Pi,
            capabilities: Capabilities { rename: true, orchestrator: true, model: true },
            title: Some("a session".to_owned()),
            directory: Some("/home/x".to_owned()),
            created_ms: Some(1),
            updated_ms: Some(updated),
            model: Some("sonnet".to_owned()),
            orchestrator_active: false,
        }
    }

    fn entry(seq: i64, id: &str, parent: Option<&str>) -> Entry {
        Entry {
            seq,
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            raw: serde_json::json!({ "id": id }),
            mapped: None,
        }
    }

    fn ingest(store: &Store, summary: &SessionSummary, entries: &[Entry]) {
        store.ingest_session(SessionIngest { summary, state: None, entries }).unwrap();
    }

    #[test]
    fn entries_round_trip_in_seq_order() {
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "pi:a".parse().unwrap();
        ingest(
            &store,
            &summary("pi:a", 10),
            &[entry(2, "b", Some("a")), entry(1, "a", None)],
        );

        let got = store.entries(&key).unwrap();
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(got[1].parent_id.as_deref(), Some("a"));
        assert_eq!(got[0].raw, serde_json::json!({ "id": "a" }));
    }

    #[test]
    fn appending_the_same_seq_replaces_rather_than_duplicating() {
        // A re-read after a cursor reset must converge, not double the file.
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "pi:a".parse().unwrap();
        ingest(&store, &summary("pi:a", 1), &[entry(1, "a", None)]);
        ingest(&store, &summary("pi:a", 2), &[entry(1, "a", None)]);
        assert_eq!(store.entries(&key).unwrap().len(), 1);
    }

    #[test]
    fn sessions_are_listed_most_recently_active_first() {
        let store = Store::in_memory().unwrap();
        for (id, updated) in [("pi:a", 10), ("pi:b", 30), ("pi:c", 20)] {
            ingest(&store, &summary(id, updated), &[]);
        }
        let ids: Vec<_> = store.sessions().unwrap().iter().map(|s| s.id.to_string()).collect();
        assert_eq!(ids, ["pi:b", "pi:c", "pi:a"]);
    }

    #[test]
    fn a_summary_survives_the_round_trip_intact() {
        let store = Store::in_memory().unwrap();
        let want = summary("pi:a", 10);
        ingest(&store, &want, &[]);
        assert_eq!(store.sessions().unwrap(), vec![want]);
    }

    #[test]
    fn an_incremental_read_does_not_blank_the_stored_source_state() {
        // pi's header is line 1 of the file, and a read that resumes from a
        // byte watermark never sees it again — so it must survive an upsert
        // that has nothing new to say about it.
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "pi:a".parse().unwrap();
        let state = serde_json::json!({ "header": { "cwd": "/home/x" } });
        store
            .ingest_session(SessionIngest {
                summary: &summary("pi:a", 1),
                state: Some(&state),
                entries: &[],
            })
            .unwrap();
        ingest(&store, &summary("pi:a", 2), &[entry(1, "a", None)]);
        assert_eq!(store.source_state(&key).unwrap(), Some(state));
    }

    #[test]
    fn a_later_read_can_replace_the_source_state() {
        // muse's carried model changes as records establish a new one.
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "muse:a".parse().unwrap();
        let mut summary = summary("muse:a", 1);
        summary.harness = Harness::Muse;
        for model in ["old", "new"] {
            let state = serde_json::json!({ "model": model });
            store
                .ingest_session(SessionIngest {
                    summary: &summary,
                    state: Some(&state),
                    entries: &[],
                })
                .unwrap();
        }
        assert_eq!(store.source_state(&key).unwrap(), Some(serde_json::json!({ "model": "new" })));
    }

    #[test]
    fn an_unknown_session_has_no_source_state() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.source_state(&"pi:nope".parse().unwrap()).unwrap(), None);
    }

    #[test]
    fn clearing_entries_keeps_the_session_row() {
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "pi:a".parse().unwrap();
        ingest(&store, &summary("pi:a", 1), &[entry(1, "a", None)]);
        store.clear_entries(&key).unwrap();
        assert!(store.entries(&key).unwrap().is_empty());
        assert_eq!(store.sessions().unwrap().len(), 1);
    }

    #[test]
    fn forgetting_a_session_removes_its_entries_too() {
        let store = Store::in_memory().unwrap();
        let key: SessionKey = "pi:a".parse().unwrap();
        ingest(&store, &summary("pi:a", 1), &[entry(1, "a", None)]);
        store.forget_session(&key).unwrap();
        assert!(store.sessions().unwrap().is_empty());
        assert!(store.entries(&key).unwrap().is_empty());
    }

    #[test]
    fn a_cursor_is_remembered_per_source_and_key() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.cursor("pi", "/a.jsonl").unwrap(), None);
        let cursor = Cursor { inode: 7, offset: 128, lines: 4 };
        store.set_cursor("pi", "/a.jsonl", cursor).unwrap();
        store.set_cursor("pi", "/b.jsonl", Cursor { inode: 8, offset: 1, lines: 1 }).unwrap();
        assert_eq!(store.cursor("pi", "/a.jsonl").unwrap(), Some(cursor));
    }

    #[test]
    fn source_health_records_both_a_failure_and_its_recovery() {
        let store = Store::in_memory().unwrap();
        let failing =
            SourceHealth { source: "pi".into(), error: Some("no such dir".into()), checked_ms: 1 };
        store.set_source_health(&failing).unwrap();
        assert_eq!(store.source_health().unwrap(), vec![failing]);

        let healthy = SourceHealth { source: "pi".into(), error: None, checked_ms: 2 };
        store.set_source_health(&healthy).unwrap();
        assert_eq!(store.source_health().unwrap(), vec![healthy]);
    }
}
