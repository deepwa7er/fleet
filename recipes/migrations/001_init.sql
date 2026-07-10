-- Recipes schema. The binary is the only writer.
--
-- Ingredients and steps are TEXT, one entry per line — the natural way to type
-- a recipe, and the UI renders them as a list / numbered steps. Tags are a
-- comma-separated, lowercased TEXT column; the model owns the split/join so
-- the API speaks a string array.

CREATE TABLE IF NOT EXISTS recipes (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  title        TEXT NOT NULL,
  description  TEXT,               -- one-line summary shown under the title
  ingredients  TEXT NOT NULL,      -- one ingredient per line
  steps        TEXT NOT NULL,      -- one step per line
  tags         TEXT NOT NULL DEFAULT '', -- comma-separated, lowercase, deduped
  servings     INTEGER,            -- NULL = unspecified
  prep_minutes INTEGER,
  cook_minutes INTEGER,
  source_url   TEXT,               -- where the recipe came from, if anywhere
  notes        TEXT,               -- freeform: tweaks, results, ideas
  created_at   TEXT NOT NULL,      -- ISO-8601 UTC
  updated_at   TEXT NOT NULL
);
