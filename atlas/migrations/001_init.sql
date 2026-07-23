-- atlas 001: projects, symbols, edges.
--
-- One row set per project; re-indexing replaces a project's symbols (and,
-- via cascade, its edges) in a single transaction, so readers never see a
-- half-ingested graph.

CREATE TABLE projects (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    root        TEXT NOT NULL,
    -- NULL until the first successful index.
    indexed_at  TEXT,
    commit_hash TEXT,
    duration_ms INTEGER
);

-- One row per distinct SCIP symbol referenced anywhere in the project.
-- Workspace symbols carry a definition site; external symbols (std, deps)
-- have file NULL and only the fields parseable from the symbol string.
CREATE TABLE symbols (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- The full SCIP symbol string; unique within a project and the join key
    -- during ingest. A crate that is both bin and lib defines a few symbols
    -- twice (rust-analyzer #18771); the first definition wins.
    symbol      TEXT NOT NULL,
    crate_name  TEXT NOT NULL,
    -- "a::b" for nested modules, '' for the crate root.
    module_path TEXT NOT NULL,
    name        TEXT NOT NULL,
    -- Human path (`crate::module::Container::name`), computed once at ingest
    -- so every surface shows symbols the same way.
    display     TEXT NOT NULL,
    -- module | struct | enum | enum_member | trait | type_alias | assoc_type |
    -- function | method | static_method | trait_method | field | constant |
    -- static | macro | unknown
    kind        TEXT NOT NULL,
    -- For members of a type or impl block: the containing type's name.
    container   TEXT,
    -- For members of a trait impl (impl Trait for Type): the trait's name.
    trait_name  TEXT,
    signature   TEXT,
    docs        TEXT,
    file        TEXT,
    start_line  INTEGER,
    end_line    INTEGER,
    is_external INTEGER NOT NULL DEFAULT 0,
    UNIQUE (project_id, symbol)
);

CREATE INDEX symbols_by_module ON symbols (project_id, crate_name, module_path);
CREATE INDEX symbols_by_name   ON symbols (project_id, name);

-- Derived references, aggregated: `count` occurrences of `from` referring to
-- `to`. kind 'call' = the target is function-like or a macro; kind 'use' =
-- types, fields, constants, statics.
CREATE TABLE edges (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_id    INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    to_id      INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    count      INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);

CREATE INDEX edges_by_to ON edges (to_id);
