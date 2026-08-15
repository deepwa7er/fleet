-- dev-warehouse schema v0 - SQLite (rusqlite bundled, pure Rust/C)
-- Embedded SQLite. Run via warehouse-ingest on init. Idempotent.
-- All tables use content hash for dedup and hourly replace.

PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS dim_repo (
    repo_id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    primary_language TEXT,
    languages TEXT, -- JSON array
    build_system TEXT, -- cargo|npm|go|python|unknown
    test_cmd TEXT,
    deploy_target TEXT,
    first_seen TEXT,
    last_seen TEXT
);

CREATE TABLE IF NOT EXISTS dim_tool (
    tool_name TEXT PRIMARY KEY,
    category TEXT -- build, lang, vcs, shell, container
);

CREATE TABLE IF NOT EXISTS fact_file (
    repo_id TEXT NOT NULL REFERENCES dim_repo(repo_id),
    path TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    language TEXT,
    ext TEXT,
    bytes INTEGER,
    hash TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    PRIMARY KEY (repo_id, path)
);

CREATE TABLE IF NOT EXISTS fact_dependency (
    repo_id TEXT NOT NULL REFERENCES dim_repo(repo_id),
    dependency TEXT NOT NULL,
    version TEXT,
    source_file TEXT NOT NULL,
    PRIMARY KEY (repo_id, dependency, source_file)
);

-- Heuristic-only integrations. type = heuristic_import | heuristic_config_ref | heuristic_api_url
-- Never manual in v0.
CREATE TABLE IF NOT EXISTS fact_integration (
    src_repo_id TEXT NOT NULL REFERENCES dim_repo(repo_id),
    dst_repo_id TEXT, -- nullable if target is external / machine
    dst_name TEXT NOT NULL, -- repo name or external identifier
    type TEXT NOT NULL,
    evidence TEXT NOT NULL, -- file:line snippet
    confidence REAL NOT NULL,
    PRIMARY KEY (src_repo_id, dst_name, evidence)
);

CREATE TABLE IF NOT EXISTS fact_git (
    repo_id TEXT NOT NULL REFERENCES dim_repo(repo_id),
    commit_hash TEXT NOT NULL,
    author TEXT,
    author_email TEXT,
    ts TEXT NOT NULL,
    message TEXT,
    files_changed INTEGER,
    PRIMARY KEY (repo_id, commit_hash)
);

CREATE TABLE IF NOT EXISTS fact_shell (
    ts TEXT,
    repo_id TEXT REFERENCES dim_repo(repo_id),
    cwd TEXT,
    cmd TEXT NOT NULL,
    tool_name TEXT REFERENCES dim_tool(tool_name),
    raw_line TEXT NOT NULL
);

-- Agent-facing views (SQLite needs DROP+CREATE instead of CREATE OR REPLACE)
DROP VIEW IF EXISTS repo_profile;
CREATE VIEW repo_profile AS
SELECT
    r.repo_id,
    r.name,
    r.path,
    r.primary_language,
    r.languages,
    r.build_system,
    r.test_cmd,
    (SELECT COUNT(*) FROM fact_file f WHERE f.repo_id = r.repo_id) AS file_count,
    (SELECT COUNT(*) FROM fact_dependency d WHERE d.repo_id = r.repo_id) AS dep_count,
    (SELECT COUNT(*) FROM fact_integration i WHERE i.src_repo_id = r.repo_id) AS integration_count
FROM dim_repo r;

DROP VIEW IF EXISTS integration_graph;
CREATE VIEW integration_graph AS
SELECT src_repo_id, dst_repo_id, dst_name, type, confidence, evidence FROM fact_integration;

DROP VIEW IF EXISTS tool_preferences;
CREATE VIEW tool_preferences AS
SELECT tool_name, category, COUNT(*) AS repo_count FROM (
    SELECT DISTINCT repo_id, tool_name FROM (
        SELECT repo_id, build_system AS tool_name FROM dim_repo WHERE build_system IS NOT NULL
        UNION ALL
        SELECT repo_id, tool_name FROM fact_shell WHERE tool_name IS NOT NULL
    )
) JOIN dim_tool USING (tool_name) GROUP BY tool_name, category ORDER BY repo_count DESC;
