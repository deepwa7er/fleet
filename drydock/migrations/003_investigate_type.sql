-- 003: support investigate tickets.
--   * tickets.type CHECK gains 'investigate' — an investigate ticket asks the
--     worker to look into something and report findings (no code change / PR).
--   * comments.kind CHECK gains 'report' — the worker's findings are posted as a
--     first-class 'report' comment, distinct from notes/questions/answers.
-- SQLite can't alter a CHECK in place, so both tables are rebuilt (preserving
-- every row and id), the same shape as migration 002. The runner applies this
-- whole file in one transaction with foreign keys already disabled (see
-- core/store.rs), so dropping/recreating tickets doesn't disturb comments'
-- references and a crash mid-migration rolls the file back cleanly.

-- tickets: widen the type CHECK.
DROP TABLE IF EXISTS tickets_migrate_003;
CREATE TABLE tickets_migrate_003 (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  title       TEXT NOT NULL,
  type        TEXT NOT NULL DEFAULT 'feature'
                CHECK (type IN ('feature','investigate')),
  target      TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'open'
                CHECK (state IN ('open','in-progress','needs-input',
                                 'in-review','blocked','done','closed')),
  priority    TEXT NOT NULL DEFAULT 'med'
                CHECK (priority IN ('high','med','low')),
  goal        TEXT NOT NULL,
  acceptance  TEXT,
  constraints TEXT,
  branch      TEXT,
  pr_url      TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
INSERT INTO tickets_migrate_003 SELECT * FROM tickets;
DROP TABLE tickets;
ALTER TABLE tickets_migrate_003 RENAME TO tickets;

-- comments: widen the kind CHECK (now that the tickets table its FK points at exists).
DROP TABLE IF EXISTS comments_migrate_003;
CREATE TABLE comments_migrate_003 (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id  INTEGER NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
  author     TEXT NOT NULL CHECK (author IN ('worker','human')),
  kind       TEXT NOT NULL CHECK (kind IN ('note','question','answer','report')),
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL
);
INSERT INTO comments_migrate_003 SELECT * FROM comments;
DROP TABLE comments;
ALTER TABLE comments_migrate_003 RENAME TO comments;

CREATE INDEX IF NOT EXISTS idx_tickets_state ON tickets(state);
CREATE INDEX IF NOT EXISTS idx_comments_ticket ON comments(ticket_id);
