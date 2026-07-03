-- Quantities move in whole units only. The half-unit allowance is retired:
-- steps.quantity becomes INTEGER, and proposals containing fractional steps
-- are retired with it (their sums may hit ten, but not in whole jumps).
-- Foreign keys are OFF for the whole migration pass (fleet-common's
-- invariant), so the rebuild is safe — and cascades don't fire, which is why
-- steps and votes are deleted explicitly.

CREATE TEMPORARY TABLE retired AS
  SELECT DISTINCT proposal_id AS id FROM steps
  WHERE quantity <> CAST(quantity AS INTEGER);

DELETE FROM steps     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM votes     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM proposals WHERE id          IN (SELECT id FROM retired);

DROP TABLE retired;

-- Rebuild steps with an INTEGER quantity (every surviving value is whole).
CREATE TABLE steps_new (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
  position    INTEGER NOT NULL CHECK (position >= 1),
  -- RESTRICT: an activity referenced by a proposal cannot be deleted out from
  -- under it (the store turns the attempt into a friendly error).
  activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE RESTRICT,
  quantity    INTEGER NOT NULL CHECK (quantity > 0),
  UNIQUE (proposal_id, position)
);

INSERT INTO steps_new (id, proposal_id, position, activity_id, quantity)
SELECT id, proposal_id, position, activity_id, CAST(quantity AS INTEGER)
FROM steps;

DROP TABLE steps;
ALTER TABLE steps_new RENAME TO steps;
CREATE INDEX idx_steps_proposal ON steps(proposal_id);
CREATE INDEX idx_steps_activity ON steps(activity_id);
