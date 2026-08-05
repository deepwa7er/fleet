//! The mirror's schema — an append-only migration list, index = `user_version`.
//!
//! Never edit an entry that has shipped; add a new one. `fleet_common::store`
//! fingerprints each applied migration and fails the next open loudly if one
//! changes underneath it.
//!
//! The whole database is a **snapshot, not a record**: every sync replaces its
//! contents wholesale. Nothing here is a source of truth, so there is no
//! history to preserve and no reason for a row to outlive the board it came
//! from — which is precisely what makes unpublishing a board in Fizzy remove
//! it from the public page on the next pass.

pub const MIGRATIONS: &[&str] = &[
    // 0 — the snapshot.
    r#"
    CREATE TABLE boards (
        id                TEXT PRIMARY KEY,   -- Fizzy's id: a join key, never published
        slug              TEXT NOT NULL UNIQUE,
        name              TEXT NOT NULL,
        description_html  TEXT NOT NULL DEFAULT '',
        position          INTEGER NOT NULL
    );

    CREATE TABLE sections (
        id        INTEGER PRIMARY KEY,
        board_id  TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        kind      TEXT NOT NULL,              -- triage | column | not_now | closed
        name      TEXT NOT NULL,
        position  INTEGER NOT NULL
    );
    CREATE INDEX sections_board ON sections(board_id, position);

    CREATE TABLE cards (
        id                INTEGER PRIMARY KEY,
        board_id          TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        section_id        INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
        number            INTEGER NOT NULL,
        title             TEXT NOT NULL,
        description_html  TEXT NOT NULL DEFAULT '',
        image_path        TEXT,               -- /a/<hash>.<ext>, or NULL
        tags              TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
        golden            INTEGER NOT NULL DEFAULT 0,
        created_at        TEXT NOT NULL,
        last_active_at    TEXT NOT NULL,
        creator_name      TEXT NOT NULL,
        creator_avatar    TEXT,
        assignees         TEXT NOT NULL DEFAULT '[]',   -- JSON [{name, avatar}]
        more_assignees    INTEGER NOT NULL DEFAULT 0,
        position          INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX cards_board_number ON cards(board_id, number);
    CREATE INDEX cards_section ON cards(section_id, position);

    -- One row. The page states its own freshness rather than implying it, so
    -- a reader can tell a quiet board from a mirror that stopped updating.
    CREATE TABLE sync_state (
        id               INTEGER PRIMARY KEY CHECK (id = 1),
        last_success_at  TEXT,
        last_attempt_at  TEXT,
        last_error       TEXT
    );
    INSERT INTO sync_state (id) VALUES (1);
    "#,
];
