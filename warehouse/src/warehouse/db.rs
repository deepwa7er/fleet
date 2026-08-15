use anyhow::Context;
use rusqlite::Connection;
use std::path::Path;

pub fn open_and_migrate(db_path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("create warehouse dir")?;
    }
    let conn = Connection::open(db_path).context("open sqlite")?;
    // Enable WAL for better concurrency and backup friendliness
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .context("pragma")?;
    let schema = include_str!("../../sql/schema.sql");
    conn.execute_batch(schema).context("migrate schema")?;
    Ok(conn)
}

pub fn checkpoint(conn: &Connection) -> anyhow::Result<()> {
    // SQLite checkpoint for WAL -> DB file so backup sees consistent state
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .context("wal_checkpoint")?;
    Ok(())
}
