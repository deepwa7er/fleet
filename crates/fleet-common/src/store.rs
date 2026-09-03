//! SQLite open + migrate, the fleet way.
//!
//! The fleet's services are single writers: one `Connection` behind a `Mutex`,
//! WAL mode, and an append-only list of migrations tracked by the database's
//! `user_version`. Two invariants live here so no service can lose them by
//! copy-drift:
//!
//! 1. **Foreign keys are OFF for the whole migration pass, ON afterwards** —
//!    unconditionally. A table-rebuild migration (SQLite's only way to alter
//!    most column properties) drops and recreates the table; with foreign keys
//!    enforced, that drop cascades into every referencing table and silently
//!    destroys its rows. Services without rebuild migrations *today* gain them
//!    the first time a column changes, so the bracket is not optional.
//!    (`PRAGMA foreign_keys` is a no-op inside a transaction, which is why it
//!    must happen out here and not inside a migration.)
//!
//! 2. **Applied migrations are fingerprinted.** The discipline is append-only
//!    — never edit an entry that has shipped — but a comment can't enforce
//!    that. Each applied migration's hash is recorded in `_fleet_migrations`,
//!    and an edit to an already-applied migration fails the next open loudly
//!    instead of silently diverging fresh databases from existing ones.

use std::path::Path;

use rusqlite::Connection;

use crate::http::{Error, Result};

/// Open (creating if needed) the database at `path` in WAL mode, apply any
/// pending `migrations`, and return the connection with foreign keys ON.
///
/// `migrations` is the service's append-only list (index = `user_version`).
/// Each migration and its version bump commit in one transaction, so a crash
/// mid-migration can never leave a half-applied schema.
pub fn open_migrated(path: impl AsRef<Path>, migrations: &[&str]) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Invariant 1: the FK-off bracket around the entire migration pass.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    migrate(&mut conn, migrations)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _fleet_migrations (
             idx  INTEGER PRIMARY KEY,
             hash TEXT NOT NULL
         );",
    )?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    for (i, sql) in migrations.iter().enumerate() {
        let hash = fingerprint(sql);
        if (i as i64) < version {
            verify_applied(conn, i, &hash)?;
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO _fleet_migrations (idx, hash) VALUES (?1, ?2)",
            (i as i64, &hash),
        )?;
        // PRAGMA values can't be bound; `i + 1` is a controlled integer.
        // `user_version` lives in the DB header and is transactional, so it
        // rolls back with the rest if the commit never lands.
        tx.execute_batch(&format!("PRAGMA user_version = {};", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

/// Invariant 2: an already-applied migration's text must still hash to what
/// ran. A database from before this crate has no recorded hash for it — the
/// append-only discipline is assumed to have held until now, so the current
/// hash is backfilled and enforced from here on.
fn verify_applied(conn: &Connection, idx: usize, hash: &str) -> Result<()> {
    let recorded: Option<String> = conn
        .query_row(
            "SELECT hash FROM _fleet_migrations WHERE idx = ?1",
            [idx as i64],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    match recorded {
        Some(recorded) if recorded == hash => Ok(()),
        Some(_) => Err(Error::Internal(format!(
            "migration {idx} was edited after being applied — migrations are \
             append-only; restore the original text and add a new migration \
             instead"
        ))),
        None => {
            conn.execute(
                "INSERT INTO _fleet_migrations (idx, hash) VALUES (?1, ?2)",
                (idx as i64, hash),
            )?;
            Ok(())
        }
    }
}

/// Apply `migrations` to the keep database behind `client`: the remote twin
/// of [`open_migrated`], enforcing the same two invariants across the wire.
///
/// 1. **The FK-off bracket** — `PRAGMA foreign_keys=OFF`, the migration
///    pass, `PRAGMA foreign_keys=ON`. keep holds one connection per database
///    behind a mutex, so these PRAGMAs and everything between them run on
///    the same server-side connection with no interleaving request.
/// 2. **Fingerprinted append-only migrations** — each applied migration's
///    FNV-1a hash is recorded in `_fleet_migrations`, and an edit to an
///    already-applied migration fails loudly, exactly as locally.
///
/// Each pending migration plus its hash row and version bump commits in one
/// keep transaction, so a crash mid-migration can never leave a half-applied
/// schema — the same atomicity the local runner gets from rusqlite.
///
/// Not directly unit-tested here: fleet-common cannot depend on keep (keep
/// depends on fleet-common), so there is no embeddable server in this
/// crate. Coverage comes from migrated services' tests, which run this
/// against a real embedded keep (recipes does).
pub async fn open_migrated_remote(
    client: &crate::keep::Client,
    migrations: &[&str],
) -> Result<()> {
    client
        .batch(
            "CREATE TABLE IF NOT EXISTS _fleet_migrations (
             idx  INTEGER PRIMARY KEY,
             hash TEXT NOT NULL
         );",
        )
        .await?;
    // Invariant 1: the FK-off bracket around the entire migration pass.
    client.query("PRAGMA foreign_keys=OFF", vec![]).await?;
    let result = migrate_remote(client, migrations).await;
    client.query("PRAGMA foreign_keys=ON", vec![]).await?;
    result
}

async fn migrate_remote(
    client: &crate::keep::Client,
    migrations: &[&str],
) -> Result<()> {
    use crate::keep::{Statement, Value};

    let version = user_version(client).await?;
    for (i, sql) in migrations.iter().enumerate() {
        let hash = fingerprint(sql);
        if (i as i64) < version {
            verify_applied_remote(client, i, &hash).await?;
            continue;
        }
        client
            .tx(vec![
                Statement::batch(sql.to_string()),
                Statement::new(
                    "INSERT INTO _fleet_migrations (idx, hash) VALUES (?1, ?2)",
                    vec![Value::from(i as i64), Value::from(hash)],
                ),
                // `user_version` lives in the DB header and commits with the
                // transaction, so it rolls back with the rest if the commit
                // never lands — same guarantee as the local runner.
                Statement::new(format!("PRAGMA user_version = {}", i + 1), vec![]),
            ])
            .await?;
    }
    Ok(())
}

async fn user_version(client: &crate::keep::Client) -> Result<i64> {
    let outcome = client.query("PRAGMA user_version", vec![]).await?;
    match outcome.rows.first().and_then(|row| row.first()) {
        Some(crate::keep::Value::Integer(v)) => Ok(*v),
        other => Err(Error::Internal(format!(
            "keep answered PRAGMA user_version with {other:?}, want one integer"
        ))),
    }
}

/// Invariant 2, remote form: same backfill-then-verify discipline as
/// [`verify_applied`], including the legacy backfill for databases migrated
/// before hashes were recorded.
async fn verify_applied_remote(
    client: &crate::keep::Client,
    idx: usize,
    hash: &str,
) -> Result<()> {
    use crate::keep::{Statement, Value};

    let outcome = client
        .query(
            "SELECT hash FROM _fleet_migrations WHERE idx = ?1",
            vec![Value::from(idx as i64)],
        )
        .await?;
    match outcome.rows.first().and_then(|row| row.first()) {
        Some(Value::Text(recorded)) if recorded == hash => Ok(()),
        Some(_) => Err(Error::Internal(format!(
            "migration {idx} was edited after being applied — migrations are \
             append-only; restore the original text and add a new migration \
             instead"
        ))),
        _ => {
            client
                .tx(vec![Statement::new(
                    "INSERT INTO _fleet_migrations (idx, hash) VALUES (?1, ?2)",
                    vec![Value::from(idx as i64), Value::from(hash.to_string())],
                )])
                .await?;
            Ok(())
        }
    }
}

/// FNV-1a 64-bit over the migration text. This detects *accidental* edits to
/// shipped migrations (the failure mode that matters here), not tampering by
/// an adversary — anyone who can rewrite the migration files can rewrite the
/// recorded hashes too, so a cryptographic hash would add cost, not safety.
fn fingerprint(sql: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in sql.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: &str = "CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT);
                      CREATE TABLE child (
                          id INTEGER PRIMARY KEY,
                          parent_id INTEGER NOT NULL REFERENCES parent(id) ON DELETE CASCADE
                      );";
    // A table rebuild: exactly the migration shape that cascade-deletes child
    // rows if foreign keys are enforced while it runs.
    const M2: &str = "CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '');
                      INSERT INTO parent_new SELECT id, COALESCE(name, '') FROM parent;
                      DROP TABLE parent;
                      ALTER TABLE parent_new RENAME TO parent;";

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("fleet-common-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// The invariant this module exists for: a rebuild migration must not
    /// cascade-delete referencing rows, and foreign keys must be ON afterwards.
    #[test]
    fn rebuild_migration_preserves_child_rows_and_reenables_fks() {
        let path = tmp("fk");
        {
            let conn = open_migrated(&path, &[M1]).unwrap();
            conn.execute("INSERT INTO parent (id, name) VALUES (1, 'a')", []).unwrap();
            conn.execute("INSERT INTO child (id, parent_id) VALUES (10, 1)", []).unwrap();
        }
        let conn = open_migrated(&path, &[M1, M2]).unwrap();
        let children: i64 = conn.query_row("SELECT COUNT(*) FROM child", [], |r| r.get(0)).unwrap();
        assert_eq!(children, 1, "the rebuild must not cascade into child");
        let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1, "foreign keys are ON for normal operation");
        // …and enforced: deleting the parent cascades now, by the schema's rule.
        conn.execute("DELETE FROM parent WHERE id = 1", []).unwrap();
        let children: i64 = conn.query_row("SELECT COUNT(*) FROM child", [], |r| r.get(0)).unwrap();
        assert_eq!(children, 0);
        let _ = std::fs::remove_file(&path);
    }

    /// Editing an already-applied migration fails the next open loudly.
    #[test]
    fn edited_applied_migration_is_refused() {
        let path = tmp("hash");
        drop(open_migrated(&path, &[M1]).unwrap());
        let err = open_migrated(&path, &["CREATE TABLE parent (id INTEGER PRIMARY KEY);"])
            .unwrap_err();
        assert!(err.to_string().contains("append-only"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }

    /// A pre-crate database (user_version set, no hash table rows) backfills
    /// hashes on first open and verifies on the second.
    #[test]
    fn legacy_database_backfills_then_verifies() {
        let path = tmp("legacy");
        {
            // Simulate a database migrated before fleet-common existed.
            let mut conn = Connection::open(&path).unwrap();
            let tx = conn.transaction().unwrap();
            tx.execute_batch(M1).unwrap();
            tx.execute_batch("PRAGMA user_version = 1;").unwrap();
            tx.commit().unwrap();
        }
        drop(open_migrated(&path, &[M1]).unwrap()); // backfills
        let conn = open_migrated(&path, &[M1]).unwrap(); // verifies
        let recorded: String = conn
            .query_row("SELECT hash FROM _fleet_migrations WHERE idx = 0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded, fingerprint(M1));
        let _ = std::fs::remove_file(&path);
    }

    /// A crash between migrations re-runs cleanly: versions advance one at a
    /// time and already-applied entries are skipped.
    #[test]
    fn reopen_applies_only_pending_migrations() {
        let path = tmp("pending");
        drop(open_migrated(&path, &[M1]).unwrap());
        let conn = open_migrated(&path, &[M1, M2]).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 2);
        let _ = std::fs::remove_file(&path);
    }
}
