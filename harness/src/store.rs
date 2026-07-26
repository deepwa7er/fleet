//! Durable session state for `harness serve`: a SQLite store so a restart
//! (launchd, a laptop reboot, a rebuild) no longer ends every conversation.
//!
//! Two things persist, and they are deliberately different:
//!
//! - **messages** — the model's context. Restored verbatim, so a resumed
//!   session keeps its memory. Deleted by a reset, because discarding the
//!   context is exactly what a reset means.
//! - **events** — the browser's transcript. Append-only, and kept *across* a
//!   reset: the retained `Reset` event makes a replaying tab clear at the same
//!   point a live tab did, so the transcript stays a faithful record of the
//!   session rather than only its current context.
//!
//! The system prompt is not stored. It is composed fresh when a session is
//! restored, so edits to `~/.config/harness/system.md` or a project's
//! `AGENTS.md` take effect on the next start instead of being frozen into
//! every old session. That is also why [Store::messages] returns the history
//! *after* index 0 — the caller rebuilds index 0 itself.
//!
//! Writes go through one background task fed by an unbounded channel
//! ([spawn_writer]), so the agent loop never waits on SQLite, and the total
//! order of writes is the order the events happened — which is what makes
//! "clear the messages, then append new ones" safe to express as two records.

use fleet_common::store::open_migrated;
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Records drained from the channel per transaction. Large enough that a busy
/// turn commits in a handful of transactions, small enough to bound how much a
/// single commit holds.
const BATCH_MAX: usize = 512;

/// Append-only. Index = `user_version`; never edit an entry that has shipped
/// (`fleet_common::store::open_migrated` fingerprints applied migrations and
/// fails loudly if one changes).
pub const MIGRATIONS: &[&str] = &[
    // 0 — sessions and their two logs.
    r#"
    CREATE TABLE sessions (
        id         TEXT    PRIMARY KEY,
        cwd        TEXT    NOT NULL,
        model      TEXT    NOT NULL,
        created_at INTEGER NOT NULL
    );

    -- The model's context, minus the system prompt at index 0, which is
    -- composed fresh on restore rather than frozen at creation.
    CREATE TABLE messages (
        session_id TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        idx        INTEGER NOT NULL,
        body       TEXT    NOT NULL,
        PRIMARY KEY (session_id, idx)
    );

    -- The browser's transcript, in emission order. `seq` orders rows within a
    -- session and nothing more: it is renumbered on restore, so gaps left by
    -- coalescing streaming deltas are expected and harmless.
    CREATE TABLE events (
        session_id TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        seq        INTEGER NOT NULL,
        body       TEXT    NOT NULL,
        PRIMARY KEY (session_id, seq)
    );
    "#,
];

/// One durable write. Applied in channel order, which is emission order.
pub enum Write {
    /// Create (or re-record) a session.
    Session { id: String, cwd: String, model: String, created_at: u64 },
    /// Append one message to a session's context at `idx`.
    Message { session: String, idx: usize, body: String },
    /// Drop a session's context — the model's history was reset.
    ClearMessages { session: String },
    /// Append one transcript event.
    Event { session: String, seq: u64, body: String },
    /// Remove the session and, by cascade, both of its logs.
    Delete { session: String },
}

/// A session as it was found on disk.
pub struct StoredSession {
    pub id: String,
    pub cwd: String,
    pub model: String,
    pub created_at: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating the file and its parent directory if needed) and migrate.
    pub fn open(path: &Path) -> Result<Store, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
        let conn = open_migrated(path, MIGRATIONS)
            .map_err(|e| format!("could not open {}: {e}", path.display()))?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every stored session, oldest first.
    pub fn sessions(&self) -> Result<Vec<StoredSession>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, cwd, model, created_at FROM sessions ORDER BY created_at, id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredSession {
                    id: r.get(0)?,
                    cwd: r.get(1)?,
                    model: r.get(2)?,
                    created_at: r.get::<_, i64>(3)?.max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())
    }

    /// A session's context in order, excluding the system prompt (see the
    /// module docs). A row that no longer parses as JSON is dropped with a log
    /// line rather than failing the whole restore — one bad row should not
    /// cost the user every other session.
    pub fn messages(&self, session: &str) -> Result<Vec<Value>, String> {
        self.bodies("SELECT body FROM messages WHERE session_id = ?1 ORDER BY idx", session)
    }

    /// A session's transcript in order, as serialized event bodies. The caller
    /// owns the event format, so these stay opaque strings here.
    pub fn events(&self, session: &str) -> Result<Vec<String>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT body FROM events WHERE session_id = ?1 ORDER BY seq")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([session], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())
    }

    fn bodies(&self, sql: &str, session: &str) -> Result<Vec<Value>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([session], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.map_err(|e| e.to_string())?;
            match serde_json::from_str(&raw) {
                Ok(value) => out.push(value),
                Err(e) => crate::log(&format!("[store] skipping unparseable row for {session}: {e}")),
            }
        }
        Ok(out)
    }

    /// Apply a batch in one transaction. Every statement is idempotent, so a
    /// retry (or a duplicate record) can never fail the batch around it.
    ///
    /// Idempotence is spelled `ON CONFLICT … DO UPDATE`, never `INSERT OR
    /// REPLACE`: REPLACE resolves a conflict by *deleting* the existing row
    /// first, which fires this schema's `ON DELETE CASCADE` and would wipe a
    /// session's messages and events every time its row was re-recorded — as
    /// it is on every restore. An upsert updates in place and cascades nothing.
    fn apply(&self, batch: &[Write]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for write in batch {
            let result = match write {
                Write::Session { id, cwd, model, created_at } => tx.execute(
                    "INSERT INTO sessions (id, cwd, model, created_at) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(id) DO UPDATE SET \
                     cwd = excluded.cwd, model = excluded.model, created_at = excluded.created_at",
                    (id, cwd, model, *created_at as i64),
                ),
                Write::Message { session, idx, body } => tx.execute(
                    "INSERT INTO messages (session_id, idx, body) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(session_id, idx) DO UPDATE SET body = excluded.body",
                    (session, *idx as i64, body),
                ),
                Write::ClearMessages { session } => {
                    tx.execute("DELETE FROM messages WHERE session_id = ?1", [session])
                }
                Write::Event { session, seq, body } => tx.execute(
                    "INSERT INTO events (session_id, seq, body) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(session_id, seq) DO UPDATE SET body = excluded.body",
                    (session, *seq as i64, body),
                ),
                // Both logs go with it: `open_migrated` leaves foreign keys ON,
                // so the ON DELETE CASCADE in the schema does the rest.
                Write::Delete { session } => {
                    tx.execute("DELETE FROM sessions WHERE id = ?1", [session])
                }
            };
            result.map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

/// Handle for queueing durable writes. Cloneable and cheap; sending never
/// blocks the caller.
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::UnboundedSender<Write>,
    /// Set once the writer task is gone, so the resulting log line is a single
    /// clear report instead of one per dropped record.
    reported: std::sync::Arc<AtomicBool>,
}

impl Writer {
    pub fn send(&self, write: Write) {
        if self.tx.send(write).is_err() && !self.reported.swap(true, Ordering::Relaxed) {
            crate::log("[store] writer task is gone — sessions are no longer being persisted");
        }
    }
}

/// Start the single writer task. It drains up to [BATCH_MAX] queued records
/// per transaction, so a burst costs one commit rather than one per record,
/// and runs the commit on a blocking thread to keep SQLite off the runtime.
pub fn spawn_writer(store: std::sync::Arc<Store>) -> Writer {
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut batch = vec![first];
            while batch.len() < BATCH_MAX {
                match rx.try_recv() {
                    Ok(write) => batch.push(write),
                    Err(_) => break,
                }
            }
            let store = std::sync::Arc::clone(&store);
            let count = batch.len();
            let applied =
                tokio::task::spawn_blocking(move || store.apply(&batch)).await.unwrap_or_else(|e| {
                    Err(format!("writer task panicked: {e}"))
                });
            if let Err(e) = applied {
                // Losing durability is bad but not fatal: the live session in
                // memory is unaffected, so keep serving and say so loudly.
                crate::log(&format!("[store] write of {count} records failed: {e}"));
            }
        }
    });
    Writer { tx, reported: std::sync::Arc::new(AtomicBool::new(false)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Store {
        // A fresh in-memory DB per test: `open_migrated` runs the real
        // migrations, so these exercise the shipping schema.
        Store::open(Path::new(":memory:")).unwrap()
    }

    fn session(id: &str) -> Write {
        Write::Session {
            id: id.into(),
            cwd: "/tmp".into(),
            model: "k3".into(),
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn round_trips_a_session_with_both_logs() {
        let s = store();
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: json!({"role":"user"}).to_string() },
            Write::Message {
                session: "s1".into(),
                idx: 2,
                body: json!({"role":"assistant"}).to_string(),
            },
            Write::Event { session: "s1".into(), seq: 1, body: r#"{"type":"turn_start"}"#.into() },
            Write::Event { session: "s1".into(), seq: 9, body: r#"{"type":"stream_end"}"#.into() },
        ])
        .unwrap();

        let sessions = s.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, "/tmp");
        let messages = s.messages("s1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        // Sparse seqs are expected — coalescing leaves gaps; order is the point.
        assert_eq!(s.events("s1").unwrap().len(), 2);
    }

    #[test]
    fn reset_drops_the_context_but_keeps_the_transcript() {
        let s = store();
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: json!({"role":"user"}).to_string() },
            Write::Event { session: "s1".into(), seq: 1, body: r#"{"type":"turn_start"}"#.into() },
            Write::ClearMessages { session: "s1".into() },
            Write::Event { session: "s1".into(), seq: 2, body: r#"{"type":"reset"}"#.into() },
        ])
        .unwrap();
        assert!(s.messages("s1").unwrap().is_empty(), "reset clears the model's context");
        assert_eq!(s.events("s1").unwrap().len(), 2, "the transcript records the reset");
    }

    #[test]
    fn deleting_a_session_cascades_into_both_logs() {
        let s = store();
        s.apply(&[
            session("s1"),
            session("s2"),
            Write::Message { session: "s1".into(), idx: 1, body: "{}".into() },
            Write::Event { session: "s1".into(), seq: 1, body: "{}".into() },
            Write::Message { session: "s2".into(), idx: 1, body: "{}".into() },
        ])
        .unwrap();
        s.apply(&[Write::Delete { session: "s1".into() }]).unwrap();

        assert_eq!(s.sessions().unwrap().len(), 1);
        assert!(s.messages("s1").unwrap().is_empty());
        assert!(s.events("s1").unwrap().is_empty());
        assert_eq!(s.messages("s2").unwrap().len(), 1, "other sessions are untouched");
    }

    #[test]
    fn writes_are_idempotent() {
        let s = store();
        s.apply(&[session("s1")]).unwrap();
        // The same records again must not fail on the primary keys.
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: "{}".into() },
        ])
        .unwrap();
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: "{}".into() },
        ])
        .unwrap();
        assert_eq!(s.sessions().unwrap().len(), 1);
        assert_eq!(s.messages("s1").unwrap().len(), 1);
    }

    /// Re-recording a session row is what every restore does, and it must not
    /// disturb the logs hanging off it. `INSERT OR REPLACE` would: it deletes
    /// the conflicting row first, and this schema cascades on delete.
    #[test]
    fn re_recording_a_session_preserves_its_logs() {
        let s = store();
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: json!({"role":"user"}).to_string() },
            Write::Event { session: "s1".into(), seq: 1, body: r#"{"type":"turn_start"}"#.into() },
        ])
        .unwrap();
        // Exactly what restore_sessions does before resuming the session.
        s.apply(&[session("s1")]).unwrap();
        assert_eq!(s.messages("s1").unwrap().len(), 1, "context survives re-recording");
        assert_eq!(s.events("s1").unwrap().len(), 1, "transcript survives re-recording");
    }

    #[test]
    fn unparseable_message_row_is_skipped_not_fatal() {
        let s = store();
        s.apply(&[
            session("s1"),
            Write::Message { session: "s1".into(), idx: 1, body: "not json".into() },
            Write::Message { session: "s1".into(), idx: 2, body: json!({"role":"user"}).to_string() },
        ])
        .unwrap();
        let messages = s.messages("s1").unwrap();
        assert_eq!(messages.len(), 1, "the good row still restores");
        assert_eq!(messages[0]["role"], "user");
    }
}
