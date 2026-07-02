-- Drydock schema. The binary is the only writer; CHECK constraints are a
-- backstop, the real state-machine enforcement lives in core/state.rs.

CREATE TABLE IF NOT EXISTS tickets (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,        -- shown as #42
  title       TEXT NOT NULL,
  type        TEXT NOT NULL DEFAULT 'feature'
                CHECK (type IN ('feature')),            -- 'new-service' added later
  target      TEXT NOT NULL,                            -- existing service name
  state       TEXT NOT NULL DEFAULT 'open'
                CHECK (state IN ('open','in-progress','needs-input',
                                 'in-review','blocked','done')),
  priority    TEXT NOT NULL DEFAULT 'med'
                CHECK (priority IN ('high','med','low')),
  goal        TEXT NOT NULL,
  acceptance  TEXT,                                     -- markdown checklist
  constraints TEXT,
  branch      TEXT,                                     -- set on claim
  pr_url      TEXT,                                     -- set on resolve
  created_at  TEXT NOT NULL,                            -- ISO-8601 UTC
  updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS comments (                   -- the human-facing Thread
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id  INTEGER NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
  author     TEXT NOT NULL CHECK (author IN ('worker','human')),
  kind       TEXT NOT NULL CHECK (kind IN ('note','question','answer')),
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (                     -- append-only audit log
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id  INTEGER NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
  from_state TEXT,                                      -- null on creation
  to_state   TEXT NOT NULL,
  actor      TEXT NOT NULL CHECK (actor IN ('worker','human')),
  detail     TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tickets_state  ON tickets(state);
CREATE INDEX IF NOT EXISTS idx_comments_ticket ON comments(ticket_id);
CREATE INDEX IF NOT EXISTS idx_events_ticket   ON events(ticket_id);
