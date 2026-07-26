//! The warehouse schema.
//!
//! Append-only fact tables, one per emitter. Two properties shape every table
//! here and are worth stating once:
//!
//! 1. **Ingestion is idempotent.** Both sources can deliver the same record
//!    twice — journald can be re-read from an older cursor, and tugboat retries
//!    a push that failed. Every table therefore carries a natural key with a
//!    `UNIQUE` constraint, and inserts use `INSERT OR IGNORE`. Re-ingesting is
//!    always safe, which means recovery never has to reason about what was
//!    already stored.
//!
//! 2. **Facts are stored as emitted, not as interpreted.** The access log's
//!    `at_ms` is epoch milliseconds and the deploy log's `at` is epoch seconds
//!    because that is what each emitter writes; converting on the way in would
//!    make a stored row disagree with its source. Queries do the arithmetic.

/// Append-only. Index = `user_version`; never edit an entry that has shipped
/// (`fleet_common::store::open_migrated` fingerprints applied migrations and
/// fails loudly if one changes).
pub const MIGRATIONS: &[&str] = &[
    // 0 — the two emitter fact tables.
    r#"
    -- One request through breakwater. Natural key: no two requests from the
    -- same client to the same path can share a millisecond, and re-ingesting a
    -- journald range must not duplicate rows.
    CREATE TABLE access (
        at_ms      INTEGER NOT NULL,
        route      TEXT    NOT NULL,
        host       TEXT    NOT NULL,
        method     TEXT    NOT NULL,
        path       TEXT    NOT NULL,
        query      TEXT,
        status     INTEGER NOT NULL,
        ms         INTEGER NOT NULL,
        client_ip  TEXT    NOT NULL,
        user_agent TEXT,
        UNIQUE (at_ms, client_ip, method, host, path)
    );
    CREATE INDEX access_at      ON access (at_ms);
    CREATE INDEX access_host_at ON access (host, at_ms);

    -- One deploy attempt from tugboat. `at` is stamped once at deploy start and
    -- shared with the host ledger and the transcript id, so it is both the join
    -- key to those and a natural key here: one service cannot start two deploys
    -- in the same second.
    CREATE TABLE deploys (
        at         INTEGER NOT NULL,
        name       TEXT    NOT NULL,
        host       TEXT    NOT NULL,
        source     TEXT    NOT NULL,
        sha        TEXT,
        short      TEXT,
        branch     TEXT,
        dirty      INTEGER NOT NULL,
        result     TEXT    NOT NULL,
        stage      TEXT,
        error      TEXT,
        build_ms   INTEGER,
        ship_ms    INTEGER,
        install_ms INTEGER,
        total_ms   INTEGER NOT NULL,
        UNIQUE (name, at)
    );
    CREATE INDEX deploys_at      ON deploys (at);
    CREATE INDEX deploys_name_at ON deploys (name, at);
    "#,
];
