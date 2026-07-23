//! SCIP index → symbol graph.
//!
//! SCIP records *occurrences* (a symbol appears at a range, optionally as its
//! definition), not a call graph. The graph falls out of one geometric step:
//! rust-analyzer emits `enclosing_range` — the full body extent — on every
//! definition, so a reference occurring inside a function's body extent is an
//! edge from that function to the referenced symbol. References in module
//! scope (`use` lines, types in signatures, const initializers) fall outside
//! every body and are deliberately not edges; they'd duplicate what the
//! in-body references already say.

use std::collections::HashMap;

use scip::types::{Index, Occurrence};

use crate::symbols::{self, ParsedSymbol, kind_is_callable, suffix_kind_str};

const DEFINITION_ROLE: i32 = 1;

pub struct SymbolRow {
    pub symbol: String,
    pub crate_name: String,
    pub module_path: String,
    pub name: String,
    pub display: String,
    pub kind: String,
    pub container: Option<String>,
    pub trait_name: Option<String>,
    pub signature: Option<String>,
    pub docs: Option<String>,
    pub file: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub is_external: bool,
}

pub struct EdgeRow {
    /// Symbol strings; the store resolves them to row ids at insert time.
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub count: i64,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct IngestStats {
    pub files: usize,
    pub symbols: usize,
    pub external_symbols: usize,
    pub call_edges: usize,
    pub use_edges: usize,
    /// References in module scope, attributed to no function (see module doc).
    pub module_scope_refs: usize,
    /// Symbols defined more than once (bin+lib crates; first wins).
    pub duplicate_definitions: usize,
}

pub struct Graph {
    pub symbols: Vec<SymbolRow>,
    pub edges: Vec<EdgeRow>,
    pub stats: IngestStats,
}

/// (line, col); SCIP positions compare lexicographically.
type Pos = (i32, i32);

#[derive(Clone, Copy)]
struct Extent {
    start: Pos,
    end: Pos,
}

impl Extent {
    /// SCIP ranges are `[startLine, startChar, endLine, endChar]`, contracted
    /// to `[line, startChar, endChar]` when the range is single-line.
    fn of(range: &[i32]) -> Option<Extent> {
        match range {
            [line, start, end] => Some(Extent {
                start: (*line, *start),
                end: (*line, *end),
            }),
            [sl, sc, el, ec] => Some(Extent {
                start: (*sl, *sc),
                end: (*el, *ec),
            }),
            _ => None,
        }
    }

    fn contains(&self, p: Pos) -> bool {
        self.start <= p && p < self.end
    }
}

fn is_definition(occ: &Occurrence) -> bool {
    occ.symbol_roles & DEFINITION_ROLE != 0
}

pub fn build_graph(index: &Index) -> Graph {
    let mut stats = IngestStats {
        files: index.documents.len(),
        ..IngestStats::default()
    };
    let mut rows: HashMap<&str, SymbolRow> = HashMap::new();

    // Pass 1 — nodes. Every document lists SymbolInformation for the symbols
    // it defines: kind, signature, docs. Locations come from pass 2.
    for doc in &index.documents {
        for info in &doc.symbols {
            let Some(parsed) = symbols::parse(&info.symbol) else {
                continue;
            };
            rows.entry(info.symbol.as_str()).or_insert_with(|| {
                let kind = symbols::kind_str(info.kind.enum_value_or_default(), &parsed);
                SymbolRow {
                    symbol: info.symbol.clone(),
                    display: parsed.display_path(),
                    kind: kind.to_string(),
                    signature: (!info.signature_documentation.text.is_empty())
                        .then(|| info.signature_documentation.text.clone()),
                    docs: (!info.documentation.is_empty()).then(|| info.documentation.join("\n\n")),
                    file: None,
                    start_line: None,
                    end_line: None,
                    is_external: false,
                    crate_name: parsed.crate_name,
                    module_path: parsed.module_path,
                    name: parsed.name,
                    container: parsed.container,
                    trait_name: parsed.trait_name,
                }
            });
        }
    }

    // Pass 2 — definition sites, and the callable body extents for pass 3.
    // `bodies` maps each document to its callable definitions' extents.
    let mut bodies: Vec<Vec<(Extent, &str)>> = Vec::with_capacity(index.documents.len());
    for doc in &index.documents {
        let mut doc_bodies: Vec<(Extent, &str)> = Vec::new();
        for occ in &doc.occurrences {
            if !is_definition(occ) {
                continue;
            }
            let Some(row) = rows.get_mut(occ.symbol.as_str()) else {
                continue;
            };
            let name_range = Extent::of(&occ.range);
            let body_range = Extent::of(&occ.enclosing_range);
            if row.file.is_some() {
                stats.duplicate_definitions += 1;
            } else {
                row.file = Some(doc.relative_path.clone());
                row.start_line = name_range.map(|e| i64::from(e.start.0));
                row.end_line = body_range.or(name_range).map(|e| i64::from(e.end.0));
            }
            if kind_is_callable(&row.kind)
                && let Some(extent) = body_range
            {
                doc_bodies.push((extent, occ.symbol.as_str()));
            }
        }
        doc_bodies.sort_by_key(|(e, _)| e.start);
        bodies.push(doc_bodies);
    }

    // Pass 3 — references, attributed to the innermost enclosing body.
    let mut edge_counts: HashMap<(&str, &str, &'static str), i64> = HashMap::new();
    let mut externals: HashMap<&str, ParsedSymbol> = HashMap::new();
    for (doc, doc_bodies) in index.documents.iter().zip(&bodies) {
        for occ in &doc.occurrences {
            if is_definition(occ) {
                continue;
            }
            let Some(pos) = Extent::of(&occ.range).map(|e| e.start) else {
                continue;
            };
            // Innermost containing body: the last definition (by start) that
            // begins at or before `pos` and whose extent contains it. Nested
            // functions are the only nesting case, so the backwards walk is
            // short in practice.
            let idx = doc_bodies.partition_point(|(e, _)| e.start <= pos);
            let Some((_, caller)) = doc_bodies[..idx]
                .iter()
                .rev()
                .find(|(e, _)| e.contains(pos))
            else {
                if symbols::parse(&occ.symbol).is_some() {
                    stats.module_scope_refs += 1;
                }
                continue;
            };

            let kind = match rows.get(occ.symbol.as_str()) {
                Some(row) => {
                    if kind_is_callable(&row.kind) {
                        "call"
                    } else {
                        "use"
                    }
                }
                None => {
                    // First sight of an external symbol; parse it once.
                    let parsed = match externals.get(occ.symbol.as_str()) {
                        Some(p) => p,
                        None => match symbols::parse(&occ.symbol) {
                            Some(p) => externals.entry(occ.symbol.as_str()).or_insert(p),
                            None => continue,
                        },
                    };
                    match parsed.suffix_kind {
                        symbols::SuffixKind::Function | symbols::SuffixKind::Macro => "call",
                        _ => "use",
                    }
                }
            };
            *edge_counts
                .entry((caller, occ.symbol.as_str(), kind))
                .or_insert(0) += 1;
        }
    }

    for (symbol, parsed) in externals {
        rows.insert(
            symbol,
            SymbolRow {
                symbol: symbol.to_string(),
                display: parsed.display_path(),
                kind: suffix_kind_str(parsed.suffix_kind).to_string(),
                signature: None,
                docs: None,
                file: None,
                start_line: None,
                end_line: None,
                is_external: true,
                crate_name: parsed.crate_name,
                module_path: parsed.module_path,
                name: parsed.name,
                container: parsed.container,
                trait_name: parsed.trait_name,
            },
        );
    }

    let edges: Vec<EdgeRow> = edge_counts
        .into_iter()
        .map(|((from, to, kind), count)| EdgeRow {
            from: from.to_string(),
            to: to.to_string(),
            kind,
            count,
        })
        .collect();

    stats.symbols = rows.len();
    stats.external_symbols = rows.values().filter(|r| r.is_external).count();
    stats.call_edges = edges.iter().filter(|e| e.kind == "call").count();
    stats.use_edges = edges.iter().filter(|e| e.kind == "use").count();

    Graph {
        symbols: rows.into_values().collect(),
        edges,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document, SymbolInformation, symbol_information::Kind};

    fn sym(s: &str) -> String {
        format!("rust-analyzer cargo demo 0.1.0 {s}")
    }

    fn info(s: &str, kind: Kind) -> SymbolInformation {
        SymbolInformation {
            symbol: sym(s),
            kind: kind.into(),
            ..SymbolInformation::default()
        }
    }

    fn occurrence(s: &str, range: &[i32], roles: i32, enclosing: &[i32]) -> Occurrence {
        Occurrence {
            symbol: sym(s),
            range: range.to_vec(),
            symbol_roles: roles,
            enclosing_range: enclosing.to_vec(),
            ..Occurrence::default()
        }
    }

    /// A file shaped like:
    /// ```text
    /// 0  use std::fs::read;          // module-scope ref: no edge
    /// 1  struct Config;
    /// 2  fn main() {                 // body lines 2..=5
    /// 3      let c = load();         // call edge main -> load
    /// 4      let _: Config = c;      // use edge main -> Config
    /// 5  }
    /// 6  fn load() -> Config {       // body lines 6..=8
    /// 7      Config                  // use edge load -> Config
    /// 8  }
    /// ```
    fn demo_index() -> Index {
        let doc = Document {
            relative_path: "src/main.rs".into(),
            symbols: vec![
                info("Config#", Kind::Struct),
                info("main().", Kind::Function),
                info("load().", Kind::Function),
            ],
            occurrences: vec![
                occurrence("read().", &[0, 13, 17], 0, &[]),
                occurrence("Config#", &[1, 7, 13], DEFINITION_ROLE, &[]),
                occurrence("main().", &[2, 3, 7], DEFINITION_ROLE, &[2, 0, 5, 1]),
                occurrence("load().", &[3, 12, 16], 0, &[]),
                occurrence("Config#", &[4, 11, 17], 0, &[]),
                occurrence("load().", &[6, 3, 7], DEFINITION_ROLE, &[6, 0, 8, 1]),
                occurrence("Config#", &[7, 4, 10], 0, &[]),
            ],
            ..Document::default()
        };
        Index {
            documents: vec![doc],
            ..Index::default()
        }
    }

    fn edge<'a>(g: &'a Graph, from: &str, to: &str) -> Option<&'a EdgeRow> {
        g.edges
            .iter()
            .find(|e| e.from == sym(from) && e.to == sym(to))
    }

    #[test]
    fn derives_call_and_use_edges_from_body_extents() {
        let graph = build_graph(&demo_index());

        let call = edge(&graph, "main().", "load().").expect("main -> load");
        assert_eq!(call.kind, "call");
        assert_eq!(call.count, 1);

        assert_eq!(edge(&graph, "main().", "Config#").unwrap().kind, "use");
        assert_eq!(edge(&graph, "load().", "Config#").unwrap().kind, "use");
        assert_eq!(
            graph.edges.len(),
            3,
            "the use-line ref must not become an edge"
        );
        assert_eq!(graph.stats.call_edges, 1);
        assert_eq!(graph.stats.use_edges, 2);
        assert_eq!(graph.stats.module_scope_refs, 1);
    }

    #[test]
    fn definition_sites_land_on_the_rows() {
        let graph = build_graph(&demo_index());
        let main = graph.symbols.iter().find(|s| s.name == "main").unwrap();
        assert_eq!(main.file.as_deref(), Some("src/main.rs"));
        assert_eq!(main.start_line, Some(2));
        assert_eq!(main.end_line, Some(5), "end comes from the enclosing range");
        let config = graph.symbols.iter().find(|s| s.name == "Config").unwrap();
        assert_eq!(config.start_line, Some(1));
        assert!(!config.is_external);
    }

    #[test]
    fn unresolved_references_become_external_symbols() {
        let graph = build_graph(&demo_index());
        // `read` is referenced at module scope only, so it must NOT appear:
        // externals exist to anchor edges, and there is no edge to it.
        assert!(graph.symbols.iter().all(|s| s.name != "read"));

        // Reference an external from inside a body instead.
        let mut index = demo_index();
        index.documents[0]
            .occurrences
            .push(occurrence("write().", &[3, 20, 25], 0, &[]));
        let graph = build_graph(&index);
        let ext = graph
            .symbols
            .iter()
            .find(|s| s.name == "write")
            .expect("external row");
        assert!(ext.is_external);
        assert_eq!(ext.kind, "function");
        assert_eq!(edge(&graph, "main().", "write().").unwrap().kind, "call");
    }

    #[test]
    fn duplicate_definitions_keep_the_first_site() {
        let mut index = demo_index();
        // The same symbol defined again later in the file (bin+lib shape).
        index.documents[0].occurrences.push(occurrence(
            "main().",
            &[10, 3, 7],
            DEFINITION_ROLE,
            &[10, 0, 12, 1],
        ));
        let graph = build_graph(&index);
        assert_eq!(graph.stats.duplicate_definitions, 1);
        let main = graph.symbols.iter().find(|s| s.name == "main").unwrap();
        assert_eq!(main.start_line, Some(2));
    }

    #[test]
    fn nested_bodies_attribute_to_the_innermost() {
        let doc = Document {
            relative_path: "src/lib.rs".into(),
            symbols: vec![
                info("outer().", Kind::Function),
                info("outer/inner().", Kind::Function),
                info("target().", Kind::Function),
            ],
            occurrences: vec![
                occurrence("outer().", &[0, 3, 8], DEFINITION_ROLE, &[0, 0, 9, 1]),
                occurrence(
                    "outer/inner().",
                    &[1, 7, 12],
                    DEFINITION_ROLE,
                    &[1, 4, 4, 5],
                ),
                // Inside both bodies -> attributed to inner.
                occurrence("target().", &[2, 8, 14], 0, &[]),
                // Inside outer only.
                occurrence("target().", &[6, 8, 14], 0, &[]),
                occurrence("target().", &[10, 0, 6], DEFINITION_ROLE, &[10, 0, 11, 1]),
            ],
            ..Document::default()
        };
        let index = Index {
            documents: vec![doc],
            ..Index::default()
        };
        let graph = build_graph(&index);
        assert_eq!(
            edge(&graph, "outer/inner().", "target().").unwrap().count,
            1
        );
        assert_eq!(edge(&graph, "outer().", "target().").unwrap().count, 1);
    }
}
