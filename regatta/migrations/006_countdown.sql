-- The course rule becomes the countdown: exactly ten steps over ten
-- DIFFERENT activities, where step 1 is done ten times, step 2 nine times,
-- … step 10 once. The quantity is therefore determined by the position
-- (11 - position), so the quantity column is dropped — a derived value
-- stored twice is a value that can disagree with itself. Proposals that
-- don't already have the countdown shape are retired. Foreign keys are OFF
-- for the whole migration pass (fleet-common's invariant), so the rebuild is
-- safe — and cascades don't fire, which is why steps and votes are deleted
-- explicitly.

CREATE TEMPORARY TABLE retired AS
  SELECT p.id
  FROM proposals p LEFT JOIN steps s ON s.proposal_id = p.id
  GROUP BY p.id
  HAVING COUNT(s.id) <> 10
      OR COUNT(DISTINCT s.activity_id) <> 10
      OR SUM(CASE WHEN s.quantity = 11 - s.position THEN 1 ELSE 0 END) <> 10;

DELETE FROM steps     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM votes     WHERE proposal_id IN (SELECT id FROM retired);
DELETE FROM proposals WHERE id          IN (SELECT id FROM retired);

DROP TABLE retired;

-- Rebuild steps without the quantity column; distinctness of activities
-- within a course gets a UNIQUE backstop.
CREATE TABLE steps_new (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
  position    INTEGER NOT NULL CHECK (position BETWEEN 1 AND 10),
  -- RESTRICT: an activity referenced by a proposal cannot be deleted out from
  -- under it (the store turns the attempt into a friendly error).
  activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE RESTRICT,
  UNIQUE (proposal_id, position),
  UNIQUE (proposal_id, activity_id)
);

INSERT INTO steps_new (id, proposal_id, position, activity_id)
SELECT id, proposal_id, position, activity_id FROM steps;

DROP TABLE steps;
ALTER TABLE steps_new RENAME TO steps;
CREATE INDEX idx_steps_proposal ON steps(proposal_id);
CREATE INDEX idx_steps_activity ON steps(activity_id);
