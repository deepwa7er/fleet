//! The warehouse store: typed facts in, aggregates out.
//!
//! Single writer (the ingest loop) plus reads from the API, so one connection
//! behind a `Mutex` is enough — the fleet's standard shape. Volume is ~800k
//! access rows a year, which SQLite serves from an index without noticing.

use std::path::Path;
use std::sync::Mutex;

use fleet_common::http::Result;
use fleet_common::store::open_migrated;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::schema::MIGRATIONS;

/// The `User-Agent` lighthouse's reachability probe sends. It fetches every
/// routed host on an interval, so it is the loudest client in the access log and
/// entirely synthetic — excluded by default from anything answering "what gets
/// used". Kept as a constant rather than a query parameter so every caller
/// excludes it the same way.
pub const PROBE_USER_AGENT: &str = "lighthouse-probe/1";

/// One request, exactly as breakwater emitted it.
#[derive(Debug, Clone, Deserialize)]
pub struct AccessRecord {
    pub at_ms: i64,
    pub route: String,
    pub host: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Option<String>,
    pub status: i64,
    pub ms: i64,
    pub client_ip: String,
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// One deploy attempt, exactly as tugboat emitted it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeployRecord {
    pub at: i64,
    pub name: String,
    pub host: String,
    pub source: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub short: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    pub result: String,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub build_ms: Option<i64>,
    #[serde(default)]
    pub ship_ms: Option<i64>,
    #[serde(default)]
    pub install_ms: Option<i64>,
    pub total_ms: i64,
}

/// Requests to one host over a window.
#[derive(Debug, Serialize)]
pub struct HostUsage {
    pub host: String,
    pub requests: i64,
    pub clients: i64,
    /// Most recent request, epoch milliseconds.
    pub last_at_ms: i64,
}

/// What the warehouse currently holds.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub access_rows: i64,
    pub deploy_rows: i64,
    /// Oldest and newest access record, epoch milliseconds; `None` when empty.
    pub access_from_ms: Option<i64>,
    pub access_to_ms: Option<i64>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(open_migrated(path, MIGRATIONS)?),
        })
    }

    /// Insert a batch of access records in one transaction, ignoring any already
    /// present. Returns how many were new — so a caller can tell a genuine
    /// backfill from a re-read of ground already covered.
    pub fn insert_access(&self, records: &[AccessRecord]) -> Result<usize> {
        let mut conn = self.conn.lock().expect("store lock");
        let tx = conn.transaction()?;
        let mut new = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO access
                     (at_ms, route, host, method, path, query, status, ms, client_ip, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for r in records {
                new += stmt.execute(params![
                    r.at_ms,
                    r.route,
                    r.host,
                    r.method,
                    r.path,
                    r.query,
                    r.status,
                    r.ms,
                    r.client_ip,
                    r.user_agent,
                ])?;
            }
        }
        tx.commit()?;
        Ok(new)
    }

    /// Insert one deploy event, ignoring a duplicate. Returns whether it was new.
    pub fn insert_deploy(&self, r: &DeployRecord) -> Result<bool> {
        let conn = self.conn.lock().expect("store lock");
        let n = conn.execute(
            "INSERT OR IGNORE INTO deploys
                 (at, name, host, source, sha, short, branch, dirty, result, stage, error,
                  build_ms, ship_ms, install_ms, total_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                r.at,
                r.name,
                r.host,
                r.source,
                r.sha,
                r.short,
                r.branch,
                r.dirty,
                r.result,
                r.stage,
                r.error,
                r.build_ms,
                r.ship_ms,
                r.install_ms,
                r.total_ms,
            ],
        )?;
        Ok(n > 0)
    }

    /// Which services actually get used, busiest first.
    ///
    /// `include_probe` exists only for diagnosing the monitoring itself; the
    /// default excludes it, because counting lighthouse's every-host-every-
    /// interval probe as usage would make every service look equally popular.
    pub fn usage_since(&self, since_ms: i64, include_probe: bool) -> Result<Vec<HostUsage>> {
        let conn = self.conn.lock().expect("store lock");
        let sql = if include_probe {
            "SELECT host, COUNT(*), COUNT(DISTINCT client_ip), MAX(at_ms)
               FROM access WHERE at_ms >= ?1
              GROUP BY host ORDER BY COUNT(*) DESC"
        } else {
            "SELECT host, COUNT(*), COUNT(DISTINCT client_ip), MAX(at_ms)
               FROM access
              WHERE at_ms >= ?1 AND (user_agent IS NULL OR user_agent <> ?2)
              GROUP BY host ORDER BY COUNT(*) DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let map = |row: &rusqlite::Row| {
            Ok(HostUsage {
                host: row.get(0)?,
                requests: row.get(1)?,
                clients: row.get(2)?,
                last_at_ms: row.get(3)?,
            })
        };
        let rows = if include_probe {
            stmt.query_map(params![since_ms], map)?.collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![since_ms, PROBE_USER_AGENT], map)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Recent deploys, newest first.
    pub fn recent_deploys(&self, limit: i64) -> Result<Vec<DeployRecord>> {
        let conn = self.conn.lock().expect("store lock");
        let mut stmt = conn.prepare(
            "SELECT at, name, host, source, sha, short, branch, dirty, result, stage, error,
                    build_ms, ship_ms, install_ms, total_ms
               FROM deploys ORDER BY at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(DeployRecord {
                    at: row.get(0)?,
                    name: row.get(1)?,
                    host: row.get(2)?,
                    source: row.get(3)?,
                    sha: row.get(4)?,
                    short: row.get(5)?,
                    branch: row.get(6)?,
                    dirty: row.get(7)?,
                    result: row.get(8)?,
                    stage: row.get(9)?,
                    error: row.get(10)?,
                    build_ms: row.get(11)?,
                    ship_ms: row.get(12)?,
                    install_ms: row.get(13)?,
                    total_ms: row.get(14)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn summary(&self) -> Result<Summary> {
        let conn = self.conn.lock().expect("store lock");
        let access_rows: i64 = conn.query_row("SELECT COUNT(*) FROM access", [], |r| r.get(0))?;
        let deploy_rows: i64 = conn.query_row("SELECT COUNT(*) FROM deploys", [], |r| r.get(0))?;
        let (access_from_ms, access_to_ms) = conn.query_row(
            "SELECT MIN(at_ms), MAX(at_ms) FROM access",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(Summary { access_rows, deploy_rows, access_from_ms, access_to_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        // A fresh in-memory DB per test: `open_migrated` runs the real
        // migrations, so these exercise the shipping schema.
        Store::open(":memory:").unwrap()
    }

    fn access(at_ms: i64, host: &str, agent: Option<&str>) -> AccessRecord {
        AccessRecord {
            at_ms,
            route: "proxy".into(),
            host: host.into(),
            method: "GET".into(),
            path: "/".into(),
            query: None,
            status: 200,
            ms: 3,
            client_ip: "100.111.100.87".into(),
            user_agent: agent.map(str::to_string),
        }
    }

    fn deploy(at: i64, name: &str, result: &str) -> DeployRecord {
        DeployRecord {
            at,
            name: name.into(),
            host: "deepwa7er".into(),
            source: "default_branch".into(),
            sha: Some("abc123".into()),
            short: Some("abc123".into()),
            branch: Some("main".into()),
            dirty: false,
            result: result.into(),
            stage: None,
            error: None,
            build_ms: Some(1000),
            ship_ms: Some(200),
            install_ms: Some(300),
            total_ms: 1500,
        }
    }

    #[test]
    fn access_ingest_is_idempotent() {
        // The whole recovery story depends on this: re-reading a journald range
        // must not double-count requests.
        let s = store();
        let batch = vec![access(1000, "tide.x", None), access(2000, "tide.x", None)];
        assert_eq!(s.insert_access(&batch).unwrap(), 2);
        assert_eq!(s.insert_access(&batch).unwrap(), 0, "re-ingest must add nothing");
        assert_eq!(s.summary().unwrap().access_rows, 2);
    }

    #[test]
    fn same_millisecond_from_different_clients_both_count() {
        // The natural key includes client_ip precisely so concurrent requests
        // are not silently collapsed into one.
        let s = store();
        let mut b = access(5000, "tide.x", None);
        b.client_ip = "100.98.184.58".into();
        assert_eq!(s.insert_access(&[access(5000, "tide.x", None), b]).unwrap(), 2);
    }

    #[test]
    fn usage_excludes_the_monitoring_probe_by_default() {
        let s = store();
        s.insert_access(&[
            access(1000, "tide.x", Some("Mozilla/5.0")),
            access(2000, "tide.x", Some(PROBE_USER_AGENT)),
            access(3000, "tide.x", Some(PROBE_USER_AGENT)),
            access(4000, "atlas.x", Some(PROBE_USER_AGENT)),
        ])
        .unwrap();

        let human = s.usage_since(0, false).unwrap();
        assert_eq!(human.len(), 1, "atlas saw only probe traffic, so it is not usage");
        assert_eq!(human[0].host, "tide.x");
        assert_eq!(human[0].requests, 1);

        let all = s.usage_since(0, true).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().map(|u| u.requests).sum::<i64>(), 4);
    }

    #[test]
    fn usage_respects_the_window_and_orders_by_volume() {
        let s = store();
        s.insert_access(&[
            access(1_000, "old.x", None),
            access(10_000, "busy.x", None),
            access(11_000, "busy.x", None),
            access(12_000, "quiet.x", None),
        ])
        .unwrap();
        let rows = s.usage_since(5_000, false).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.host.as_str()).collect::<Vec<_>>(),
            vec!["busy.x", "quiet.x"],
            "outside the window is excluded; busiest first"
        );
        assert_eq!(rows[0].last_at_ms, 11_000);
    }

    #[test]
    fn deploy_ingest_is_idempotent_per_service_and_start_time() {
        let s = store();
        let d = deploy(1_700_000_000, "tide", "deployed");
        assert!(s.insert_deploy(&d).unwrap(), "first push is new");
        assert!(!s.insert_deploy(&d).unwrap(), "a retried push is not");
        // A different service at the same instant is a different deploy.
        assert!(s.insert_deploy(&deploy(1_700_000_000, "atlas", "deployed")).unwrap());
        assert_eq!(s.summary().unwrap().deploy_rows, 2);
    }

    #[test]
    fn recent_deploys_are_newest_first_and_round_trip_failures() {
        let s = store();
        s.insert_deploy(&deploy(100, "tide", "deployed")).unwrap();
        let mut failed = deploy(200, "regatta", "failed");
        failed.stage = Some("artifacts".into());
        failed.error = Some("file artifact not found".into());
        failed.build_ms = None;
        s.insert_deploy(&failed).unwrap();

        let rows = s.recent_deploys(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "regatta");
        assert_eq!(rows[0].stage.as_deref(), Some("artifacts"));
        assert_eq!(rows[0].build_ms, None, "a skipped build stays absent, not 0");
        assert_eq!(rows[1].name, "tide");
    }

    #[test]
    fn summary_of_an_empty_warehouse_has_no_range() {
        let s = store();
        let summary = s.summary().unwrap();
        assert_eq!(summary.access_rows, 0);
        assert!(summary.access_from_ms.is_none());
    }
}
