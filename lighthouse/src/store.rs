//! Persisted health history.
//!
//! A small SQLite time-series of each monitored service's systemd state, memory,
//! and out-of-loopback reachability — written by the background [`collector`] and
//! read by the history API. The collector is the only writer; the API only reads,
//! so a single connection behind a `Mutex` is enough (the volume is tiny: a
//! handful of services every minute).
//!
//! [`collector`]: crate::collector

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// One recorded observation of a service at a point in time.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Unix epoch seconds.
    pub at: i64,
    /// systemd `ActiveState` (`active`, `failed`, …).
    pub active_state: String,
    /// `MemoryCurrent`, when systemd reported it.
    pub memory_bytes: Option<i64>,
    /// The out-of-loopback probe result, or `None` when the service has no public
    /// URL to probe (so reachability is simply not tracked for it).
    pub probe: Option<Probe>,
}

/// The result of fetching a service's public URL through breakwater.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Reachable: a valid TLS handshake and an HTTP status below 500. A 4xx still
    /// counts as reachable — it proves TLS, host routing, and a live upstream;
    /// only TLS/connection failures and 5xx are "down".
    pub ok: bool,
    /// HTTP status, when one was received (`None` on a TLS/connection failure).
    pub status: Option<u16>,
    /// Round-trip latency in milliseconds, when the fetch completed.
    pub ms: Option<u32>,
}

/// The history database.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if needed) the database at `path`, ensuring its parent
    /// directory and schema exist.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating history db directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening history db {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS samples (
                 unit         TEXT    NOT NULL,
                 at           INTEGER NOT NULL,
                 active_state TEXT    NOT NULL,
                 memory_bytes INTEGER,
                 probe_ok     INTEGER,  -- NULL = not probed, else 0/1
                 probe_status INTEGER,
                 probe_ms     INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_samples_unit_at ON samples(unit, at);",
        )
        .context("initializing history schema")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Record one sample for a unit.
    pub fn insert(&self, unit: &str, sample: &Sample) -> Result<()> {
        let (ok, status, ms) = match &sample.probe {
            Some(p) => (
                Some(p.ok as i64),
                p.status.map(i64::from),
                p.ms.map(i64::from),
            ),
            None => (None, None, None),
        };
        self.conn.lock().unwrap().execute(
            "INSERT INTO samples
                 (unit, at, active_state, memory_bytes, probe_ok, probe_status, probe_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![unit, sample.at, sample.active_state, sample.memory_bytes, ok, status, ms],
        )?;
        Ok(())
    }

    /// Every sample for a unit at or after `since`, oldest first.
    pub fn window(&self, unit: &str, since: i64) -> Result<Vec<Sample>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT at, active_state, memory_bytes, probe_ok, probe_status, probe_ms
             FROM samples WHERE unit = ?1 AND at >= ?2 ORDER BY at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![unit, since], |r| {
            let probe_ok: Option<i64> = r.get(3)?;
            let probe_status: Option<i64> = r.get(4)?;
            let probe_ms: Option<i64> = r.get(5)?;
            Ok(Sample {
                at: r.get(0)?,
                active_state: r.get(1)?,
                memory_bytes: r.get(2)?,
                probe: probe_ok.map(|ok| Probe {
                    ok: ok != 0,
                    status: probe_status.map(|v| v as u16),
                    ms: probe_ms.map(|v| v as u32),
                }),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Delete samples older than `before`. Returns how many were removed.
    pub fn prune(&self, before: i64) -> Result<usize> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM samples WHERE at < ?1", rusqlite::params![before])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_store() -> Store {
        // An in-memory db exercises the same schema and SQL as the real file.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE samples (
                 unit TEXT NOT NULL, at INTEGER NOT NULL, active_state TEXT NOT NULL,
                 memory_bytes INTEGER, probe_ok INTEGER, probe_status INTEGER, probe_ms INTEGER);",
        )
        .unwrap();
        Store { conn: Mutex::new(conn) }
    }

    fn sample(at: i64, state: &str, mem: Option<i64>, probe: Option<Probe>) -> Sample {
        Sample { at, active_state: state.into(), memory_bytes: mem, probe }
    }

    #[test]
    fn round_trips_samples_in_order_within_window() {
        let store = mem_store();
        store.insert("a.service", &sample(100, "active", Some(10), Some(Probe { ok: true, status: Some(200), ms: Some(5) }))).unwrap();
        store.insert("a.service", &sample(200, "failed", None, Some(Probe { ok: false, status: None, ms: None }))).unwrap();
        store.insert("b.service", &sample(150, "active", None, None)).unwrap();

        let a = store.window("a.service", 0).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].at, 100);
        assert_eq!(a[1].at, 200);
        assert!(a[0].probe.as_ref().unwrap().ok);
        assert_eq!(a[0].probe.as_ref().unwrap().status, Some(200));
        assert!(!a[1].probe.as_ref().unwrap().ok);
        // A service with no URL stores no probe.
        assert!(store.window("b.service", 0).unwrap()[0].probe.is_none());
        // The window filters by `since`.
        assert_eq!(store.window("a.service", 150).unwrap().len(), 1);
    }

    #[test]
    fn prune_drops_old_samples_only() {
        let store = mem_store();
        store.insert("a.service", &sample(100, "active", None, None)).unwrap();
        store.insert("a.service", &sample(500, "active", None, None)).unwrap();
        assert_eq!(store.prune(300).unwrap(), 1);
        let left = store.window("a.service", 0).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].at, 500);
    }
}
