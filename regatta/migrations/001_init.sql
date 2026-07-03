-- Regatta schema. The binary is the only writer; CHECK constraints are a
-- backstop for the category enum spelled in core/model.rs and for the
-- exactly-ten-positive-steps rule the store enforces.

CREATE TABLE IF NOT EXISTS activities (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL,               -- what a step demands ("Distance run")
  category   TEXT NOT NULL
               CHECK (category IN ('foods','misc','physical','video-games')),
  unit       TEXT NOT NULL,               -- what the quantity counts ("miles")
  sort_order INTEGER NOT NULL DEFAULT 0,  -- picker order (lower first)
  created_at TEXT NOT NULL                -- ISO-8601 UTC
);

CREATE TABLE IF NOT EXISTS proposals (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  title      TEXT NOT NULL,
  author     TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS steps (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
  position    INTEGER NOT NULL CHECK (position BETWEEN 1 AND 10),
  -- RESTRICT: an activity referenced by a proposal cannot be deleted out from
  -- under it (the store turns the attempt into a friendly error).
  activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE RESTRICT,
  quantity    REAL NOT NULL CHECK (quantity > 0),
  UNIQUE (proposal_id, position)
);

-- One vote per voter per proposal; casting is set membership, not a counter.
CREATE TABLE IF NOT EXISTS votes (
  proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
  voter       TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  PRIMARY KEY (proposal_id, voter)
);

CREATE INDEX IF NOT EXISTS idx_steps_proposal  ON steps(proposal_id);
CREATE INDEX IF NOT EXISTS idx_steps_activity  ON steps(activity_id);
