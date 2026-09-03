//! keep's databases: one turso database per fleet service, each behind its
//! own async mutex.
//!
//! The fleet's services are single writers today (one connection behind a
//! mutex in every `Store`), and keep preserves that shape exactly — one
//! connection per database, serialized server-side. There is no pool to tune
//! and no interleaving to reason about: a request holds its database's lock
//! from the first statement to the last, so a `/tx` batch (and a client's
//! `PRAGMA foreign_keys=OFF` … migrate … `ON` bracket around it) can never
//! observe another request mid-flight.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use fleet_common::keep::{Column, Outcome, Value};
use tokio::sync::Mutex;

/// A database name is also a filename (`<name>.db`) and a URL segment, so it
/// is validated once at startup and trusted afterwards — including inside the
/// `VACUUM INTO` path the snapshot loop interpolates.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

struct Db {
    conn: Mutex<turso::Connection>,
}

/// The open databases, keyed by name. Tokens live alongside (loaded from the
/// same file) but are checked in the server layer, not here.
pub struct Registry {
    dbs: HashMap<String, Db>,
    tokens: HashMap<String, String>,
}

impl Registry {
    /// Open (creating) one turso database per token entry, in WAL mode with
    /// foreign keys on and a 5s busy timeout. A database that has never been
    /// written is just an empty file until its app's migrations arrive —
    /// provisioning a new app is adding one line to the tokens file.
    pub async fn open(
        data_dir: &Path,
        entries: Vec<(String, String)>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!entries.is_empty(), "no databases configured");
        std::fs::create_dir_all(data_dir)?;
        let mut dbs = HashMap::new();
        let mut tokens = HashMap::new();
        for (name, token) in entries {
            anyhow::ensure!(valid_name(&name), "invalid database name: {name:?}");
            anyhow::ensure!(!token.is_empty(), "empty token for database {name:?}");
            anyhow::ensure!(
                tokens.insert(name.clone(), token).is_none(),
                "duplicate database: {name:?}"
            );
            let path = data_dir.join(format!("{name}.db"));
            let db = turso::Builder::new_local(path.to_string_lossy().as_ref())
                .build()
                .await
                .map_err(|e| anyhow::anyhow!("opening database {name:?}: {e}"))?;
            let conn = db.connect()?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            // PRAGMAs report their values as rows, and `execute_batch`
            // refuses row-returning statements — drain them as queries.
            for pragma in ["PRAGMA journal_mode=WAL", "PRAGMA foreign_keys=ON"] {
                let mut rows = conn.query(pragma, ()).await?;
                while rows.next().await?.is_some() {}
            }
            tracing::info!("database {name:?} at {}", path.display());
            dbs.insert(name, Db {
                conn: Mutex::new(conn),
            });
        }
        Ok(Registry { dbs, tokens })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.dbs.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.dbs.keys().cloned().collect();
        names.sort();
        names
    }

    /// Constant-time comparison: tokens are secrets, and even over the
    /// tailnet there is no reason to leak their prefixes through timing.
    /// Unknown databases fail closed before any comparison runs.
    pub fn authorized(&self, name: &str, presented: &str) -> bool {
        match self.tokens.get(name) {
            None => false,
            Some(expected) => {
                let (a, b) = (expected.as_bytes(), presented.as_bytes());
                a.len() == b.len()
                    && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
            }
        }
    }

    /// Run one statement in autocommit: a prepared single when params are
    /// given, a multi-statement script otherwise (DDL, migrations). See
    /// [`run_tx`] for the atomic batch form.
    pub async fn run_one(
        &self,
        name: &str,
        sql: &str,
        params: &[Value],
        batch: bool,
    ) -> Result<Outcome, turso::Error> {
        let db = self.dbs.get(name).expect("checked by the caller");
        let conn = db.conn.lock().await;
        run_statement(&conn, sql, params, batch).await
    }

    /// Run statements atomically under `BEGIN IMMEDIATE`: all commit, or on
    /// any error all roll back. Explicit SQL rather than turso's
    /// `Transaction` object, which has no batch form — migrations need one —
    /// and explicitness keeps the single code path for both endpoints. The
    /// per-database mutex makes the BEGIN/COMMIT bracket race-free.
    pub async fn run_tx(
        &self,
        name: &str,
        statements: &[(String, Vec<Value>, bool)],
    ) -> Result<Vec<Outcome>, turso::Error> {
        let db = self.dbs.get(name).expect("checked by the caller");
        let conn = db.conn.lock().await;
        conn.execute_batch("BEGIN IMMEDIATE").await?;
        let mut results = Vec::with_capacity(statements.len());
        for (sql, params, batch) in statements {
            match run_statement(&conn, sql, params, *batch).await {
                Ok(outcome) => results.push(outcome),
                Err(e) => {
                    // Best-effort: the rollback failing too leaves nothing to
                    // do but report the original error loudly.
                    let _ = conn.execute_batch("ROLLBACK").await;
                    return Err(e);
                }
            }
        }
        conn.execute_batch("COMMIT").await?;
        Ok(results)
    }

    /// Snapshot every database into `dir` via `VACUUM INTO`: a consistent,
    /// plain-SQLite single file per database, no WAL sidecar, readable by any
    /// SQLite-compatible engine without keep-specific code (verified against
    /// rusqlite before this shipped). Runs under each database's lock, so a
    /// snapshot never observes a half-written transaction.
    pub async fn snapshot_all(&self, dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dir)?;
        let mut out = Vec::new();
        for (name, db) in &self.dbs {
            // Safe interpolation: `name` passed `valid_name` at startup, so
            // it holds no quote, slash, or whitespace.
            let dest = dir.join(format!("{name}.db"));
            // `VACUUM INTO` refuses to overwrite: snapshot to a sibling and
            // atomically rename over the previous one, so restic and drill
            // readers never observe a half-written file.
            let staging = dir.join(format!("{name}.db.next"));
            let sql = format!(
                "VACUUM INTO '{}'",
                staging.to_string_lossy().replace('\'', "''")
            );
            db.conn.lock().await.execute_batch(sql.as_str()).await?;
            std::fs::rename(&staging, &dest)?;
            // The destination inherits WAL mode, so the staging write can
            // leave an empty sidecar behind the rename — sweep the orphans
            // so restic never ships litter and readers see one file per db.
            // (A WAL-mode file with no -wal beside it reads fine; SQLite
            // treats the absent log as empty.)
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut orphan = staging.as_os_str().to_owned();
                orphan.push(suffix);
                let _ = std::fs::remove_file(Path::new(&orphan));
            }
            out.push(dest);
        }
        Ok(out)
    }
}

async fn run_statement(
    conn: &turso::Connection,
    sql: &str,
    params: &[Value],
    batch: bool,
) -> Result<Outcome, turso::Error> {
    if batch {
        // A script (one statement or many), run verbatim with no params.
        // Outcomes carry no change count — a script has no single number to
        // give, and the only batch users are migrations, which never ask.
        conn.execute_batch(sql).await?;
        return Ok(Outcome {
            columns: Vec::new(),
            rows: Vec::new(),
            rowid: conn.last_insert_rowid(),
            changes: 0,
        });
    }
    let mut stmt = conn.prepare(sql).await?;
    if stmt.column_count() == 0 {
        let changes = stmt.execute(to_params(params)).await?;
        Ok(Outcome {
            columns: Vec::new(),
            rows: Vec::new(),
            rowid: conn.last_insert_rowid(),
            changes,
        })
    } else {
        let columns = stmt
            .columns()
            .iter()
            .map(|c| Column {
                name: c.name().to_owned(),
                decl_type: c.decl_type().map(str::to_owned),
            })
            .collect();
        let mut rows = stmt.query(to_params(params)).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut cells = Vec::with_capacity(row.column_count());
            for idx in 0..row.column_count() {
                cells.push(to_wire(row.get_value(idx)?));
            }
            out.push(cells);
        }
        Ok(Outcome {
            columns,
            rows: out,
            rowid: conn.last_insert_rowid(),
            changes: stmt.n_change(),
        })
    }
}

fn to_params(params: &[Value]) -> Vec<turso::Value> {
    params
        .iter()
        .map(|v| match v {
            Value::Null => turso::Value::Null,
            Value::Integer(i) => turso::Value::Integer(*i),
            Value::Real(r) => turso::Value::Real(*r),
            Value::Text(t) => turso::Value::Text(t.clone()),
            Value::Blob(b) => turso::Value::Blob(b.clone()),
        })
        .collect()
}

fn to_wire(value: turso::Value) -> Value {
    match value {
        turso::Value::Null => Value::Null,
        turso::Value::Integer(i) => Value::Integer(i),
        turso::Value::Real(r) => Value::Real(r),
        turso::Value::Text(t) => Value::Text(t),
        turso::Value::Blob(b) => Value::Blob(b),
    }
}

/// turso's typed errors onto the fleet's status codes. Constraint and misuse
/// are the client's fault (400); a busy store is transient but never retried
/// server-side — 503 tells the client to surface, not spin.
pub fn error_status(e: &turso::Error) -> StatusCode {
    match e {
        turso::Error::Constraint(_)
        | turso::Error::Misuse(_)
        | turso::Error::ToSqlConversionFailure(_)
        | turso::Error::ConversionFailure(_) => StatusCode::BAD_REQUEST,
        turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keep-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn open_rejects_bad_config() {
        let dir = scratch("badcfg");
        assert!(Registry::open(&dir, vec![]).await.is_err());
        for bad in ["", "Has Space", "../escape", "UPPER", "semi;colon"] {
            let err = Registry::open(&dir, vec![(bad.into(), "t".into())]).await;
            assert!(err.is_err(), "{bad:?} should be rejected");
        }
        let dup = Registry::open(
            &dir,
            vec![("a".into(), "t1".into()), ("a".into(), "t2".into())],
        )
        .await;
        assert!(dup.is_err());
        let empty_token = Registry::open(&dir, vec![("a".into(), "".into())]).await;
        assert!(empty_token.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn authorized_fails_closed() {
        let dir = scratch("authz");
        let reg = Registry::open(&dir, vec![("a".into(), "secret".into())])
            .await
            .unwrap();
        assert!(reg.authorized("a", "secret"));
        assert!(!reg.authorized("a", "wrong"));
        assert!(!reg.authorized("a", "secre")); // prefix leaks nothing
        assert!(!reg.authorized("nosuchdb", "secret"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exit-path guarantee, locked in: a snapshot opens in stock
    /// SQLite and holds committed data — no keep code on the read path.
    #[tokio::test]
    async fn snapshots_open_in_stock_sqlite() {
        let dir = scratch("snap");
        let reg = Registry::open(&dir, vec![("a".into(), "t".into())])
            .await
            .unwrap();
        reg.run_one(
            "a",
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t (v) VALUES ('x')",
            &[],
            true,
        )
        .await
        .unwrap();
        let snap_dir = dir.join("snapshots");
        let files = reg.snapshot_all(&snap_dir).await.unwrap();
        assert_eq!(files, vec![snap_dir.join("a.db")]);
        drop(reg);
        let conn = rusqlite::Connection::open(snap_dir.join("a.db")).unwrap();
        let v: String = conn.query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "x");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Snapshots repeat: `VACUUM INTO` refuses to overwrite, so the second
    /// tick must still succeed (via the staging rename) and see new writes.
    #[tokio::test]
    async fn snapshots_repeat_and_stay_current() {
        let dir = scratch("snap2");
        let reg = Registry::open(&dir, vec![("a".into(), "t".into())])
            .await
            .unwrap();
        reg.run_one("a", "CREATE TABLE t (id INTEGER PRIMARY KEY)", &[], true)
            .await
            .unwrap();
        let snap_dir = dir.join("snapshots");
        reg.snapshot_all(&snap_dir).await.unwrap();
        reg.run_one("a", "INSERT INTO t DEFAULT VALUES", &[], false)
            .await
            .unwrap();
        reg.snapshot_all(&snap_dir).await.unwrap();
        // One file per database: the staging file renamed, its WAL
        // sidecar orphans swept.
        let mut entries: Vec<_> = std::fs::read_dir(&snap_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["a.db"]);
        drop(reg);
        let conn = rusqlite::Connection::open(snap_dir.join("a.db")).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
