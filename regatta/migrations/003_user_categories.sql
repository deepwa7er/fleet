-- Categories graduate from a fixed CHECK-constrained enum to user-editable
-- rows, so new sections can be added from the UI without a deploy. The old
-- activities.category TEXT column becomes a foreign key; activity ids are
-- preserved so existing proposal steps keep pointing at the same rows.
-- (Foreign keys are OFF for the whole migration pass — fleet-common's
-- invariant — so the table rebuild is safe.)

CREATE TABLE categories (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL UNIQUE,
  sort_order INTEGER NOT NULL DEFAULT 0, -- picker order (lower first)
  created_at TEXT NOT NULL               -- ISO-8601 UTC
);

INSERT INTO categories (name, sort_order, created_at) VALUES
  ('Foods',           10, '2026-07-03T00:00:00Z'),
  ('Misc',            20, '2026-07-03T00:00:00Z'),
  ('Physical',        30, '2026-07-03T00:00:00Z'),
  ('Video games',     40, '2026-07-03T00:00:00Z'),
  ('Games & puzzles', 50, '2026-07-03T00:00:00Z'),
  ('Social',          60, '2026-07-03T00:00:00Z'),
  ('Exploration',     70, '2026-07-03T00:00:00Z'),
  ('Chance',          80, '2026-07-03T00:00:00Z'),
  ('Adulting',        90, '2026-07-03T00:00:00Z'),
  ('Creative',       100, '2026-07-03T00:00:00Z');

-- Rebuild activities: category TEXT -> category_id FK, ids preserved.
CREATE TABLE activities_new (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT NOT NULL,
  -- RESTRICT: a category with activities cannot be deleted out from under
  -- them (the store turns the attempt into a friendly error).
  category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE RESTRICT,
  unit        TEXT NOT NULL,
  sort_order  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL
);

INSERT INTO activities_new (id, name, category_id, unit, sort_order, created_at)
SELECT a.id, a.name, c.id, a.unit, a.sort_order, a.created_at
FROM activities a
JOIN categories c ON c.name = CASE a.category
  WHEN 'foods'       THEN 'Foods'
  WHEN 'misc'        THEN 'Misc'
  WHEN 'physical'    THEN 'Physical'
  WHEN 'video-games' THEN 'Video games'
END;

DROP TABLE activities;
ALTER TABLE activities_new RENAME TO activities;
CREATE INDEX idx_activities_category ON activities(category_id);

-- Grow the catalog: easily-quantifiable activities only — counts, streaks,
-- and measures that need no judging panel. Ordinary rows; edit freely.
INSERT INTO activities (name, category_id, unit, sort_order, created_at)
SELECT v.column1, c.id, v.column3, v.column4, '2026-07-03T00:00:00Z'
FROM (VALUES
  ('Hot wings eaten',                     'Foods',           'wings',         110),
  ('Tacos eaten',                         'Foods',           'tacos',         120),
  ('Espresso shots drunk',                'Foods',           'shots',         130),
  ('Glasses of water drunk',              'Foods',           'glasses',       140),
  ('Push-ups done',                       'Physical',        'push-ups',      150),
  ('Pull-ups done',                       'Physical',        'pull-ups',      160),
  ('Miles biked',                         'Physical',        'miles',         170),
  ('Flights of stairs climbed',           'Physical',        'flights',       180),
  ('Cold-plunge minutes',                 'Physical',        'minutes',       190),
  ('Mario Kart races won',                'Video games',     'races',         200),
  ('Smash matches won',                   'Video games',     'matches',       210),
  ('Chess games won',                     'Games & puzzles', 'wins',          220),
  ('Wordle puzzles solved',               'Games & puzzles', 'puzzles',       230),
  ('Rubik''s cubes solved',               'Games & puzzles', 'solves',        240),
  ('Darts bullseyes hit',                 'Games & puzzles', 'bullseyes',     250),
  ('Trivia questions answered correctly', 'Games & puzzles', 'questions',     260),
  ('High-fives from strangers',           'Social',          'high-fives',    270),
  ('Compliments given',                   'Social',          'compliments',   280),
  ('Karaoke songs performed',             'Social',          'songs',         290),
  ('Phone numbers collected',             'Social',          'numbers',       300),
  ('New restaurants tried',               'Exploration',     'restaurants',   310),
  ('Parks visited',                       'Exploration',     'parks',         320),
  ('Geocaches found',                     'Exploration',     'geocaches',     330),
  ('Neighborhoods walked through',        'Exploration',     'neighborhoods', 340),
  ('Claw machine wins',                   'Chance',          'wins',          350),
  ('Scratcher tickets scratched',         'Chance',          'tickets',       360),
  ('Coin flips called correctly in a row','Chance',          'flips',         370),
  ('Rock-paper-scissors wins in a row',   'Chance',          'wins',          380),
  ('Dishes washed',                       'Adulting',        'dishes',        390),
  ('Loads of laundry finished',           'Adulting',        'loads',         400),
  ('Items donated',                       'Adulting',        'items',         410),
  ('Emails archived',                     'Adulting',        'emails',        420),
  ('Haikus written (5-7-5 counts itself)','Creative',        'haikus',        430),
  ('TikToks posted',                      'Creative',        'videos',        440),
  ('Photos of dogs taken',                'Creative',        'photos',        450)
) AS v
JOIN categories c ON c.name = v.column2;
