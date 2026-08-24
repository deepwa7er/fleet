//! The derived schema, and the rebuild that replaces migrations.
//!
//! Bump `SCHEMA_VERSION` whenever any statement below changes. On mismatch the
//! whole database is dropped and re-ingested — which is legal precisely
//! because nothing in it is authored (DW-004 §5). Do not add a migration path
//! here; if a table ever holds something that cannot be re-derived, it belongs
//! in the change log instead, not in this file.

use anyhow::Result;
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE session (
    id                  TEXT PRIMARY KEY,
    harness             TEXT    NOT NULL,
    title               TEXT,
    directory           TEXT,
    created_ms          INTEGER,
    updated_ms          INTEGER,
    model               TEXT,
    orchestrator_active INTEGER NOT NULL DEFAULT 0,
    -- The source's own session header (pi's line 1: cwd, created-at). Kept
    -- because it appears exactly once, at the top of the file, and an
    -- incremental read that starts from a byte watermark never sees it again.
    header_raw          TEXT,
    cap_rename          INTEGER NOT NULL DEFAULT 0,
    cap_orchestrator    INTEGER NOT NULL DEFAULT 0,
    cap_model           INTEGER NOT NULL DEFAULT 0
);

-- Ordering key for the session list and the desk.
CREATE INDEX session_by_activity ON session (updated_ms DESC);

CREATE TABLE entry (
    session_id TEXT    NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    id         TEXT    NOT NULL,
    parent_id  TEXT,
    raw        TEXT    NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

-- The tree walk resolves parents by id within one session.
CREATE INDEX entry_by_entry_id ON entry (session_id, id);

CREATE TABLE source_cursor (
    source TEXT    NOT NULL,
    key    TEXT    NOT NULL,
    inode  INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    lines  INTEGER NOT NULL,
    PRIMARY KEY (source, key)
) WITHOUT ROWID;

CREATE TABLE source_health (
    source     TEXT PRIMARY KEY,
    error      TEXT,
    checked_ms INTEGER NOT NULL
);
"#;

/// Bring the connection to `SCHEMA_VERSION`, rebuilding from empty if it is at
/// any other version (including a fresh database at 0).
pub fn ensure(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version != 0 {
        tracing::info!(
            from = version,
            to = SCHEMA_VERSION,
            "read model schema changed; dropping and re-ingesting"
        );
    }
    drop_all(conn)?;
    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn drop_all(conn: &Connection) -> Result<()> {
    // Foreign keys are on, and the drop order is not guaranteed to respect
    // them; suspending enforcement for the teardown is exactly what the pragma
    // is for. It is restored before any data is written.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for name in names {
        conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{name}\""))?;
    }
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure(&conn).unwrap();
        ensure(&conn).unwrap();
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_stale_version_drops_everything_and_rebuilds() {
        let conn = Connection::open_in_memory().unwrap();
        ensure(&conn).unwrap();
        conn.execute("INSERT INTO session (id, harness) VALUES ('pi:a', 'pi')", []).unwrap();
        conn.execute_batch("CREATE TABLE leftover (x INTEGER)").unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1).unwrap();

        ensure(&conn).unwrap();

        let sessions: i64 =
            conn.query_row("SELECT count(*) FROM session", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 0, "derived rows must not survive a schema change");
        let leftover: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'leftover'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0, "tables from the old schema must be dropped too");
    }
}
