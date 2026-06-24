-- 002: add the 'closed' terminal state. A human may abandon a ticket (won't-do)
-- from any non-terminal state — distinct from 'done', which means completed and
-- merged. SQLite can't alter a CHECK constraint in place, so rebuild the tickets
-- table with the widened state CHECK, preserving every row and id.
--
-- Re-runnable: the temp table is dropped first so a retry after a mid-migration
-- failure starts clean. Foreign keys are disabled for the swap so dropping the
-- old tickets table doesn't cascade into comments/events (their ids are stable).

PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS tickets_migrate_002;

CREATE TABLE tickets_migrate_002 (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  title       TEXT NOT NULL,
  type        TEXT NOT NULL DEFAULT 'feature'
                CHECK (type IN ('feature')),
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

INSERT INTO tickets_migrate_002 SELECT * FROM tickets;
DROP TABLE tickets;
ALTER TABLE tickets_migrate_002 RENAME TO tickets;

PRAGMA foreign_keys = ON;

CREATE INDEX IF NOT EXISTS idx_tickets_state ON tickets(state);
