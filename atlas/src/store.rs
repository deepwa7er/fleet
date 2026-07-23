//! All SQLite access. Single writer (one `Connection` behind a `Mutex`), the
//! fleet's open/migrate invariants via fleet-common.
//!
//! A project's graph is replaced wholesale in one transaction on re-index, so
//! readers always see either the old graph or the new one, never a mix.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::ingest::Graph;

const MIGRATIONS: &[&str] = &[include_str!("../migrations/001_init.sql")];

/// Caps that keep pathological queries from flooding the UI; each response
/// says when it was truncated.
const LINKED_SYMBOL_CAP: usize = 200;
pub const TRACE_MAX_DEPTH: u32 = 6;
pub const TRACE_NODE_CAP: usize = 400;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Serialize)]
pub struct ProjectMeta {
    pub id: i64,
    pub name: String,
    pub root: String,
    pub indexed_at: Option<String>,
    pub commit_hash: Option<String>,
    pub duration_ms: Option<i64>,
    pub symbols: i64,
    pub call_edges: i64,
}

#[derive(Serialize)]
pub struct SymbolSummary {
    pub id: i64,
    pub name: String,
    pub display: String,
    pub kind: String,
    pub crate_name: String,
    pub module_path: String,
    pub container: Option<String>,
    pub trait_name: Option<String>,
    pub signature: Option<String>,
    pub file: Option<String>,
    pub start_line: Option<i64>,
    pub is_external: bool,
}

const SUMMARY_COLS: &str = "id, name, display, kind, crate_name, module_path, container, \
                            trait_name, signature, file, start_line, is_external";

#[derive(Serialize)]
pub struct LinkedSymbol {
    pub edge_kind: String,
    pub count: i64,
    #[serde(flatten)]
    pub symbol: SymbolSummary,
}

#[derive(Serialize)]
pub struct SymbolDetail {
    #[serde(flatten)]
    pub summary: SymbolSummary,
    pub end_line: Option<i64>,
    pub docs: Option<String>,
    pub callers: Vec<LinkedSymbol>,
    pub callees: Vec<LinkedSymbol>,
    pub callers_truncated: bool,
    pub callees_truncated: bool,
    /// For a trait or one of its methods: the implementing counterparts.
    pub implementations: Vec<SymbolSummary>,
    /// For a trait-impl member: the trait's declaration of it.
    pub declaration: Option<SymbolSummary>,
}

#[derive(Serialize)]
pub struct ModuleRow {
    pub crate_name: String,
    pub module_path: String,
    pub items: i64,
}

#[derive(Serialize)]
pub struct TraceNode {
    #[serde(flatten)]
    pub symbol: SymbolSummary,
    pub depth: u32,
}

#[derive(Serialize)]
pub struct TraceEdge {
    pub from: i64,
    pub to: i64,
    pub count: i64,
}

#[derive(Serialize)]
pub struct TraceGraph {
    pub root: i64,
    pub direction: String,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
    pub truncated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    /// Callees: what the root reaches.
    Out,
    /// Callers: what reaches the root.
    In,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Columns past [`SUMMARY_COLS`] start at this index in a wider SELECT.
const AFTER_SUMMARY: usize = 12;

fn summary_from_row(row: &Row) -> rusqlite::Result<SymbolSummary> {
    Ok(SymbolSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        display: row.get(2)?,
        kind: row.get(3)?,
        crate_name: row.get(4)?,
        module_path: row.get(5)?,
        container: row.get(6)?,
        trait_name: row.get(7)?,
        signature: row.get(8)?,
        file: row.get(9)?,
        start_line: row.get(10)?,
        is_external: row.get(11)?,
    })
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = fleet_common::store::open_migrated(path, MIGRATIONS)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    /// Register (or re-root) a configured project; index metadata survives.
    pub fn upsert_project(&self, name: &str, root: &str) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO projects (name, root) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET root = excluded.root",
            params![name, root],
        )?;
        let id = conn.query_row("SELECT id FROM projects WHERE name = ?1", [name], |r| {
            r.get(0)
        })?;
        Ok(id)
    }

    pub fn projects(&self) -> Result<Vec<ProjectMeta>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.root, p.indexed_at, p.commit_hash, p.duration_ms,
                    (SELECT COUNT(*) FROM symbols s
                      WHERE s.project_id = p.id AND s.is_external = 0),
                    (SELECT COUNT(*) FROM edges e
                      WHERE e.project_id = p.id AND e.kind = 'call')
             FROM projects p ORDER BY p.name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ProjectMeta {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    root: row.get(2)?,
                    indexed_at: row.get(3)?,
                    commit_hash: row.get(4)?,
                    duration_ms: row.get(5)?,
                    symbols: row.get(6)?,
                    call_edges: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn project_id(&self, name: &str) -> Result<i64> {
        self.lock()
            .query_row("SELECT id FROM projects WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("project {name}")))
    }

    /// Swap in a freshly ingested graph for `project_id`, atomically.
    pub fn replace_graph(
        &self,
        project_id: i64,
        graph: &Graph,
        commit_hash: Option<&str>,
        duration_ms: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        // The FK on edges cascades, but the explicit delete keeps the cost
        // visible and works regardless of cascade order.
        tx.execute("DELETE FROM edges WHERE project_id = ?1", [project_id])?;
        tx.execute("DELETE FROM symbols WHERE project_id = ?1", [project_id])?;

        let mut ids: HashMap<&str, i64> = HashMap::with_capacity(graph.symbols.len());
        {
            let mut insert = tx.prepare(
                "INSERT INTO symbols
                   (project_id, symbol, crate_name, module_path, name, display, kind,
                    container, trait_name, signature, docs, file, start_line, end_line,
                    is_external)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for s in &graph.symbols {
                insert.execute(params![
                    project_id,
                    s.symbol,
                    s.crate_name,
                    s.module_path,
                    s.name,
                    s.display,
                    s.kind,
                    s.container,
                    s.trait_name,
                    s.signature,
                    s.docs,
                    s.file,
                    s.start_line,
                    s.end_line,
                    s.is_external,
                ])?;
                ids.insert(s.symbol.as_str(), tx.last_insert_rowid());
            }

            let mut insert_edge = tx.prepare(
                "INSERT INTO edges (project_id, from_id, to_id, kind, count)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for e in &graph.edges {
                // Both endpoints come from the same ingest pass, so they are
                // present by construction; a miss would be an ingest bug and
                // the `expect` names it.
                let from = ids.get(e.from.as_str()).expect("edge source ingested");
                let to = ids.get(e.to.as_str()).expect("edge target ingested");
                insert_edge.execute(params![project_id, from, to, e.kind, e.count])?;
            }
        }

        tx.execute(
            "UPDATE projects
                SET indexed_at = ?2, commit_hash = ?3, duration_ms = ?4
              WHERE id = ?1",
            params![project_id, now(), commit_hash, duration_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every module that holds at least one item, with its item count.
    /// The client builds the tree (and any empty intermediate modules) from
    /// the paths.
    pub fn modules(&self, project_id: i64) -> Result<Vec<ModuleRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT crate_name, module_path, COUNT(*) FROM symbols
              WHERE project_id = ?1 AND is_external = 0 AND kind != 'module'
              GROUP BY crate_name, module_path
              ORDER BY crate_name, module_path",
        )?;
        let rows = stmt
            .query_map([project_id], |row| {
                Ok(ModuleRow {
                    crate_name: row.get(0)?,
                    module_path: row.get(1)?,
                    items: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The items of one module: top-level symbols and type/impl members,
    /// source order.
    pub fn module_items(
        &self,
        project_id: i64,
        crate_name: &str,
        module_path: &str,
    ) -> Result<Vec<SymbolSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLS} FROM symbols
              WHERE project_id = ?1 AND crate_name = ?2 AND module_path = ?3
                AND is_external = 0 AND kind != 'module'
              ORDER BY file, start_line, name"
        ))?;
        let rows = stmt
            .query_map(
                params![project_id, crate_name, module_path],
                summary_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn symbol_detail(&self, id: i64) -> Result<SymbolDetail> {
        let conn = self.lock();
        let (summary, project_id, end_line, docs) = conn
            .query_row(
                &format!(
                    "SELECT {SUMMARY_COLS}, project_id, end_line, docs
                            FROM symbols WHERE id = ?1"
                ),
                [id],
                |row| {
                    Ok((
                        summary_from_row(row)?,
                        row.get::<_, i64>(AFTER_SUMMARY)?,
                        row.get(AFTER_SUMMARY + 1)?,
                        row.get(AFTER_SUMMARY + 2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("symbol #{id}")))?;

        let linked = |sql: &str| -> Result<Vec<LinkedSymbol>> {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt
                .query_map([id], |row| {
                    Ok(LinkedSymbol {
                        symbol: summary_from_row(row)?,
                        edge_kind: row.get(AFTER_SUMMARY)?,
                        count: row.get(AFTER_SUMMARY + 1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        };

        // One row past the cap signals truncation without a second count query.
        let fetch = LINKED_SYMBOL_CAP + 1;
        let mut callers = linked(&format!(
            "SELECT {}, e.kind, e.count FROM edges e JOIN symbols s ON s.id = e.from_id
              WHERE e.to_id = ?1
              ORDER BY e.kind ASC, e.count DESC, s.name LIMIT {fetch}",
            qualified_summary_cols()
        ))?;
        let mut callees = linked(&format!(
            "SELECT {}, e.kind, e.count FROM edges e JOIN symbols s ON s.id = e.to_id
              WHERE e.from_id = ?1
              ORDER BY e.kind ASC, e.count DESC, s.name LIMIT {fetch}",
            qualified_summary_cols()
        ))?;
        let callers_truncated = callers.len() > LINKED_SYMBOL_CAP;
        let callees_truncated = callees.len() > LINKED_SYMBOL_CAP;
        callers.truncate(LINKED_SYMBOL_CAP);
        callees.truncate(LINKED_SYMBOL_CAP);

        // Trait linkage is by name: SCIP relationships are absent from
        // rust-analyzer's output, but the symbol grammar names both sides.
        // Same-named traits in different crates could collide; for a personal
        // fleet that ambiguity is acceptable and visible in the listed paths.
        let implementations = match summary.kind.as_str() {
            "trait" => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {SUMMARY_COLS} FROM symbols
                      WHERE project_id = ?1 AND trait_name = ?2 AND is_external = 0
                      ORDER BY container, name"
                ))?;
                stmt.query_map(params![project_id, summary.name], summary_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
            "trait_method" => {
                let trait_name = summary.container.as_deref().unwrap_or_default();
                let mut stmt = conn.prepare(&format!(
                    "SELECT {SUMMARY_COLS} FROM symbols
                      WHERE project_id = ?1 AND trait_name = ?2 AND name = ?3
                        AND is_external = 0
                      ORDER BY container"
                ))?;
                stmt.query_map(
                    params![project_id, trait_name, summary.name],
                    summary_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
            }
            _ => Vec::new(),
        };

        let declaration = match (&summary.trait_name, summary.kind.as_str()) {
            (Some(trait_name), "method" | "static_method" | "function" | "trait_method") => conn
                .query_row(
                    &format!(
                        "SELECT {SUMMARY_COLS} FROM symbols
                          WHERE project_id = ?1 AND kind = 'trait_method'
                            AND container = ?2 AND name = ?3"
                    ),
                    params![project_id, trait_name, summary.name],
                    summary_from_row,
                )
                .optional()?,
            _ => None,
        };

        Ok(SymbolDetail {
            summary,
            end_line,
            docs,
            callers,
            callees,
            callers_truncated,
            callees_truncated,
            implementations,
            declaration,
        })
    }

    /// Breadth-first slice of the call graph from `root`, out to `max_depth`
    /// or [`TRACE_NODE_CAP`] nodes, whichever comes first.
    ///
    /// With `include_external` off (the default surface), the walk stays on
    /// workspace symbols: calls into std/deps otherwise dominate every layer
    /// and bury the flow being traced. The symbol page still lists them.
    pub fn trace(
        &self,
        root: i64,
        direction: TraceDirection,
        max_depth: u32,
        include_external: bool,
    ) -> Result<TraceGraph> {
        let conn = self.lock();
        conn.query_row("SELECT 1 FROM symbols WHERE id = ?1", [root], |_| Ok(()))
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("symbol #{root}")))?;

        let max_depth = max_depth.min(TRACE_MAX_DEPTH);
        let mut depths: HashMap<i64, u32> = HashMap::from([(root, 0)]);
        let mut edges: Vec<TraceEdge> = Vec::new();
        let mut frontier: Vec<i64> = vec![root];
        let mut truncated = false;

        for depth in 1..=max_depth {
            if frontier.is_empty() {
                break;
            }
            let marks = placeholders(frontier.len());
            let external = if include_external {
                ""
            } else {
                "AND s.is_external = 0"
            };
            let sql = match direction {
                TraceDirection::Out => format!(
                    "SELECT e.from_id, e.to_id, e.count FROM edges e
                      JOIN symbols s ON s.id = e.to_id
                      WHERE e.kind = 'call' AND e.from_id IN ({marks}) {external}
                      ORDER BY e.from_id, e.count DESC"
                ),
                TraceDirection::In => format!(
                    "SELECT e.from_id, e.to_id, e.count FROM edges e
                      JOIN symbols s ON s.id = e.from_id
                      WHERE e.kind = 'call' AND e.to_id IN ({marks}) {external}
                      ORDER BY e.to_id, e.count DESC"
                ),
            };
            let mut stmt = conn.prepare(&sql)?;
            let step: Vec<(i64, i64, i64)> = stmt
                .query_map(rusqlite::params_from_iter(&frontier), |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let mut next: Vec<i64> = Vec::new();
            for (from, to, count) in step {
                edges.push(TraceEdge { from, to, count });
                let discovered = match direction {
                    TraceDirection::Out => to,
                    TraceDirection::In => from,
                };
                if depths.len() >= TRACE_NODE_CAP {
                    truncated = truncated || !depths.contains_key(&discovered);
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(e) = depths.entry(discovered) {
                    e.insert(depth);
                    next.push(discovered);
                }
            }
            frontier = next;
        }

        // The depth limit left a frontier unexplored; that only truncates the
        // trace if those nodes' edges would have shown something.
        if !frontier.is_empty() && !truncated {
            let marks = placeholders(frontier.len());
            let external = if include_external {
                ""
            } else {
                "AND s.is_external = 0"
            };
            let (own, far) = match direction {
                TraceDirection::Out => ("from_id", "to_id"),
                TraceDirection::In => ("to_id", "from_id"),
            };
            let more: bool = conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM edges e
                                     JOIN symbols s ON s.id = e.{far}
                                     WHERE e.kind = 'call' AND e.{own} IN ({marks}) {external})"
                ),
                rusqlite::params_from_iter(&frontier),
                |r| r.get(0),
            )?;
            truncated = more;
        }

        // Edges whose far end was cut by the node cap would dangle; drop them.
        edges.retain(|e| depths.contains_key(&e.from) && depths.contains_key(&e.to));
        edges.sort_by_key(|e| (e.from, e.to));
        edges.dedup_by_key(|e| (e.from, e.to));

        let node_ids: Vec<i64> = depths.keys().copied().collect();
        let marks = placeholders(node_ids.len());
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLS} FROM symbols WHERE id IN ({marks})"
        ))?;
        let mut nodes: Vec<TraceNode> = stmt
            .query_map(rusqlite::params_from_iter(&node_ids), |row| {
                let symbol = summary_from_row(row)?;
                Ok(TraceNode {
                    depth: depths[&symbol.id],
                    symbol,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        nodes.sort_by(|a, b| {
            (a.depth, &a.symbol.crate_name, &a.symbol.name).cmp(&(
                b.depth,
                &b.symbol.crate_name,
                &b.symbol.name,
            ))
        });

        Ok(TraceGraph {
            root,
            direction: match direction {
                TraceDirection::Out => "out".into(),
                TraceDirection::In => "in".into(),
            },
            nodes,
            edges,
            truncated,
        })
    }

    /// Name search, exact-then-prefix-then-substring, internal symbols first.
    pub fn search(&self, project_id: i64, query: &str, limit: usize) -> Result<Vec<SymbolSummary>> {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLS} FROM symbols
              WHERE project_id = ?1 AND kind != 'module'
                AND (name LIKE ?2 ESCAPE '\\' OR container LIKE ?2 ESCAPE '\\')
              ORDER BY is_external ASC,
                       (name = ?3) DESC,
                       (name LIKE ?4 ESCAPE '\\') DESC,
                       LENGTH(name) ASC,
                       name ASC
              LIMIT {limit}"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    project_id,
                    format!("%{escaped}%"),
                    query,
                    format!("{escaped}%")
                ],
                summary_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// `?1, ?2, …` for an IN list.
fn placeholders(n: usize) -> String {
    let mut out = String::new();
    for i in 1..=n {
        if i > 1 {
            out.push_str(", ");
        }
        out.push('?');
        out.push_str(&i.to_string());
    }
    out
}

/// [`SUMMARY_COLS`] with each column qualified as `s.` for joins.
fn qualified_summary_cols() -> String {
    SUMMARY_COLS
        .split(", ")
        .map(|c| format!("s.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{EdgeRow, IngestStats, SymbolRow};

    fn row(symbol: &str, name: &str, kind: &str, module: &str) -> SymbolRow {
        SymbolRow {
            symbol: symbol.into(),
            crate_name: "demo".into(),
            module_path: module.into(),
            name: name.into(),
            display: format!("demo::{name}"),
            kind: kind.into(),
            container: None,
            trait_name: None,
            signature: Some(format!("fn {name}()")),
            docs: None,
            file: Some("src/main.rs".into()),
            start_line: Some(1),
            end_line: Some(3),
            is_external: false,
        }
    }

    fn edge(from: &str, to: &str, kind: &'static str) -> EdgeRow {
        EdgeRow {
            from: from.into(),
            to: to.into(),
            kind,
            count: 1,
        }
    }

    fn demo_graph() -> Graph {
        let mut config = row("Config#", "Config", "struct", "config");
        config.signature = Some("struct Config".into());
        // Later in the file than `load`, so item ordering is source order.
        config.start_line = Some(10);
        Graph {
            symbols: vec![
                row("main().", "main", "function", ""),
                row("run().", "run", "function", ""),
                row("config/load().", "load", "function", "config"),
                config,
            ],
            edges: vec![
                edge("main().", "run().", "call"),
                edge("run().", "config/load().", "call"),
                edge("config/load().", "Config#", "use"),
            ],
            stats: IngestStats::default(),
        }
    }

    fn open_store(name: &str) -> (Store, i64) {
        let path = std::env::temp_dir().join(format!("atlas-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let id = store.upsert_project("demo", "/tmp/demo").unwrap();
        store
            .replace_graph(id, &demo_graph(), Some("abc1234"), 1500)
            .unwrap();
        (store, id)
    }

    fn find<'a>(nodes: &'a [TraceNode], name: &str) -> Option<&'a TraceNode> {
        nodes.iter().find(|n| n.symbol.name == name)
    }

    #[test]
    fn replace_graph_swaps_wholesale() {
        let (store, id) = open_store("swap");
        let before = store.projects().unwrap();
        assert_eq!(before[0].symbols, 4);
        assert_eq!(before[0].call_edges, 2);
        assert_eq!(before[0].commit_hash.as_deref(), Some("abc1234"));

        // Re-index with a smaller graph: nothing from the old one survives.
        let smaller = Graph {
            symbols: vec![row("main().", "main", "function", "")],
            edges: vec![],
            stats: IngestStats::default(),
        };
        store.replace_graph(id, &smaller, None, 900).unwrap();
        let after = store.projects().unwrap();
        assert_eq!(after[0].symbols, 1);
        assert_eq!(after[0].call_edges, 0);
        assert!(store.search(id, "load", 10).unwrap().is_empty());
    }

    #[test]
    fn modules_and_items() {
        let (store, id) = open_store("modules");
        let modules = store.modules(id).unwrap();
        let paths: Vec<&str> = modules.iter().map(|m| m.module_path.as_str()).collect();
        assert_eq!(paths, vec!["", "config"]);
        assert_eq!(modules[1].items, 2);

        let items = store.module_items(id, "demo", "config").unwrap();
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["load", "Config"]);
    }

    #[test]
    fn symbol_detail_links_callers_and_callees() {
        let (store, id) = open_store("detail");
        let run = &store.search(id, "run", 10).unwrap()[0];
        let detail = store.symbol_detail(run.id).unwrap();
        assert_eq!(detail.callers.len(), 1);
        assert_eq!(detail.callers[0].symbol.name, "main");
        assert_eq!(detail.callees.len(), 1);
        assert_eq!(detail.callees[0].symbol.name, "load");
        assert_eq!(detail.callees[0].edge_kind, "call");
        assert!(!detail.callers_truncated);
    }

    #[test]
    fn trait_implementations_link_by_name() {
        let (store, id) = open_store("traits");
        let mut graph = demo_graph();
        let mut decl = row("Sink#emit().", "emit", "trait_method", "");
        decl.container = Some("Sink".into());
        let mut sink = row("Sink#", "Sink", "trait", "");
        sink.signature = Some("trait Sink".into());
        let mut imp = row("impl#[FileSink][Sink]emit().", "emit", "method", "");
        imp.container = Some("FileSink".into());
        imp.trait_name = Some("Sink".into());
        graph.symbols.extend([decl, sink, imp]);
        store.replace_graph(id, &graph, None, 0).unwrap();

        let decl_id = store
            .search(id, "emit", 10)
            .unwrap()
            .into_iter()
            .find(|s| s.kind == "trait_method")
            .unwrap()
            .id;
        let detail = store.symbol_detail(decl_id).unwrap();
        assert_eq!(detail.implementations.len(), 1);
        assert_eq!(
            detail.implementations[0].container.as_deref(),
            Some("FileSink")
        );

        let impl_id = detail.implementations[0].id;
        let impl_detail = store.symbol_detail(impl_id).unwrap();
        assert_eq!(impl_detail.declaration.unwrap().id, decl_id);
    }

    #[test]
    fn trace_walks_call_edges_only() {
        let (store, id) = open_store("trace");
        let main = &store.search(id, "main", 10).unwrap()[0];

        let out = store.trace(main.id, TraceDirection::Out, 6, false).unwrap();
        assert_eq!(
            out.nodes.len(),
            3,
            "Config is reached by a use edge, not a call"
        );
        assert_eq!(find(&out.nodes, "main").unwrap().depth, 0);
        assert_eq!(find(&out.nodes, "run").unwrap().depth, 1);
        assert_eq!(find(&out.nodes, "load").unwrap().depth, 2);
        assert_eq!(out.edges.len(), 2);
        assert!(!out.truncated);

        let load = &store.search(id, "load", 10).unwrap()[0];
        let inward = store.trace(load.id, TraceDirection::In, 6, false).unwrap();
        assert_eq!(find(&inward.nodes, "main").unwrap().depth, 2);
    }

    #[test]
    fn trace_hides_externals_unless_asked() {
        let (store, id) = open_store("externals");
        let mut graph = demo_graph();
        let mut ext = row("tokio/spawn().", "spawn", "function", "");
        ext.is_external = true;
        ext.file = None;
        graph.symbols.push(ext);
        graph.edges.push(edge("main().", "tokio/spawn().", "call"));
        store.replace_graph(id, &graph, None, 0).unwrap();
        let main = &store.search(id, "main", 10).unwrap()[0];

        let internal = store.trace(main.id, TraceDirection::Out, 6, false).unwrap();
        assert!(find(&internal.nodes, "spawn").is_none());
        assert!(
            !internal.truncated,
            "an external-only tail is not truncation"
        );

        let full = store.trace(main.id, TraceDirection::Out, 6, true).unwrap();
        assert!(find(&full.nodes, "spawn").is_some());
    }

    #[test]
    fn trace_survives_cycles() {
        let (store, id) = open_store("cycles");
        let mut graph = demo_graph();
        graph.edges.push(edge("config/load().", "main().", "call"));
        store.replace_graph(id, &graph, None, 0).unwrap();
        let main = &store.search(id, "main", 10).unwrap()[0];
        let out = store.trace(main.id, TraceDirection::Out, 6, false).unwrap();
        assert_eq!(out.nodes.len(), 3);
        // The back edge is present but introduces no new node.
        assert_eq!(out.edges.len(), 3);
    }

    #[test]
    fn search_ranks_exact_and_prefix_first() {
        let (store, id) = open_store("search");
        let mut graph = demo_graph();
        graph
            .symbols
            .push(row("reload().", "reload", "function", ""));
        graph
            .symbols
            .push(row("loader().", "loader", "function", ""));
        store.replace_graph(id, &graph, None, 0).unwrap();
        let hits = store.search(id, "load", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["load", "loader", "reload"]);
        // LIKE metacharacters in the query must not act as wildcards.
        assert!(store.search(id, "l%ad", 10).unwrap().is_empty());
    }
}
