-- Seed the starting activity catalog. Runs once on a fresh database; these are
-- ordinary rows — rename, re-unit, or delete them freely from the UI afterward
-- (deletion is blocked only while a proposal still references the activity).

INSERT INTO activities (name, category, unit, sort_order, created_at) VALUES
  ('Donuts eaten',                 'foods',       'donuts',    10, '2026-07-02T00:00:00Z'),
  ('Shots (or shot equivalents)',  'foods',       'shots',     20, '2026-07-02T00:00:00Z'),
  ('Things bought',                'misc',        'purchases', 30, '2026-07-02T00:00:00Z'),
  ('Friends made',                 'misc',        'friends',   40, '2026-07-02T00:00:00Z'),
  ('Cats petted',                  'misc',        'cats',      50, '2026-07-02T00:00:00Z'),
  ('Handstand held',               'physical',    'minutes',   60, '2026-07-02T00:00:00Z'),
  ('Distance run',                 'physical',    'miles',     70, '2026-07-02T00:00:00Z'),
  ('Rocket League wins',           'video-games', 'wins',      80, '2026-07-02T00:00:00Z'),
  ('GeoGuessr rounds',             'video-games', 'rounds',    90, '2026-07-02T00:00:00Z'),
  ('Valorant wins',                'video-games', 'wins',     100, '2026-07-02T00:00:00Z');
