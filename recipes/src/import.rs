//! One-time import from a local SQLite file into keep.
//!
//! Reads every row from the file's `recipes` table (opened read-only — the
//! live file is never modified) and inserts them with their original ids
//! and timestamps, so URLs and dates survive the move. Refuses a non-empty
//! keep database: imports run once, against an empty store, and a second
//! run would violate primary keys anyway.
//!
//! The SELECT column list must match `migrations/001_init.sql`; tags travel
//! in their stored comma-joined form (they are already canonical — the
//! model owns the split/join on the way back out).

use std::path::Path;

use fleet_common::keep::{Client, Statement};
use rusqlite::OpenFlags;

use crate::core::Store;

/// Statements per transaction. keep caps a transaction at 1000; 500 leaves
/// headroom while keeping a personal recipe book to one or two rounds.
const CHUNK: usize = 500;

pub async fn run(store: &Store, from: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let client = store.client();

    let count = count_remote(client).await?;
    if count > 0 {
        return Err(format!(
            "keep already holds {count} recipe(s) — imports run once, against an empty store"
        )
        .into());
    }

    let src = rusqlite::Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("opening {from:?} read-only: {e}"))?;
    let mut stmt = src.prepare(
        "SELECT id, title, description, ingredients, steps, tags, servings,
                prep_minutes, cook_minutes, source_url, notes, created_at, updated_at
         FROM recipes ORDER BY id",
    )?;
    let rows: Vec<[rusqlite::types::Value; 13]> = stmt
        .query_map([], |row| {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
            ])
        })?
        .collect::<rusqlite::Result<_>>()?;

    for chunk in rows.chunks(CHUNK) {
        let statements = chunk
            .iter()
            .map(|cells| {
                Statement::new(
                    "INSERT INTO recipes
                       (id, title, description, ingredients, steps, tags, servings,
                        prep_minutes, cook_minutes, source_url, notes, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    cells.iter().map(to_wire).collect(),
                )
            })
            .collect();
        client.tx(statements).await?;
    }
    Ok(rows.len())
}

async fn count_remote(client: &Client) -> Result<i64, Box<dyn std::error::Error>> {
    let outcome = client.query("SELECT COUNT(*) FROM recipes", vec![]).await?;
    match outcome.rows.first().and_then(|row| row.first()) {
        Some(fleet_common::keep::Value::Integer(n)) => Ok(*n),
        other => Err(format!("COUNT(*) answered {other:?}, want one integer").into()),
    }
}

/// rusqlite's value straight across to keep's: the storage classes are the
/// same five on both sides, so this is a mechanical mapping, not a conversion.
fn to_wire(value: &rusqlite::types::Value) -> fleet_common::keep::Value {
    use fleet_common::keep::Value as W;
    use rusqlite::types::Value as R;
    match value {
        R::Null => W::Null,
        R::Integer(i) => W::Integer(*i),
        R::Real(r) => W::Real(*r),
        R::Text(t) => W::Text(t.clone()),
        R::Blob(b) => W::Blob(b.clone()),
    }
}
