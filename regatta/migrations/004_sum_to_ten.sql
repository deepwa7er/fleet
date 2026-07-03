-- The course rule changes: not "exactly ten steps" but "step quantities add
-- up to exactly ten". Steps are rebuilt to drop the ten-position CHECK (a
-- course may now have any number of steps), and proposals that don't satisfy
-- the new budget are retired. Foreign keys are OFF for the whole migration
-- pass (fleet-common's invariant), so the rebuild is safe — and cascades
-- don't fire, which is why steps and votes are deleted explicitly.

CREATE TABLE steps_new (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
  position    INTEGER NOT NULL CHECK (position >= 1),
  -- RESTRICT: an activity referenced by a proposal cannot be deleted out from
  -- under it (the store turns the attempt into a friendly error).
  activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE RESTRICT,
  quantity    REAL NOT NULL CHECK (quantity > 0),
  UNIQUE (proposal_id, position)
);

INSERT INTO steps_new (id, proposal_id, position, activity_id, quantity)
SELECT id, proposal_id, position, activity_id, quantity FROM steps;

DROP TABLE steps;
ALTER TABLE steps_new RENAME TO steps;
CREATE INDEX idx_steps_proposal ON steps(proposal_id);
CREATE INDEX idx_steps_activity ON steps(activity_id);

-- Retire proposals that don't add up to ten. Their votes go with them; a
-- tally under one rule says nothing about a course under another.
CREATE TEMPORARY TABLE retired AS
  SELECT p.id
  FROM proposals p LEFT JOIN steps s ON s.proposal_id = p.id
  GROUP BY p.id
  HAVING COALESCE(SUM(s.quantity), 0) <> 10;

DELETE FROM steps     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM votes     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM proposals WHERE id          IN (SELECT id FROM retired);

DROP TABLE retired;
