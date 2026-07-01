-- Clothes schema. The binary is the only writer; CHECK constraints are a
-- backstop for the status enum spelled in core/model.rs.

CREATE TABLE IF NOT EXISTS categories (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT NOT NULL,
  note         TEXT,                       -- guidance for the category (classic-wardrobe advice)
  target_count INTEGER,                    -- how many items to own here; NULL = no target
  sort_order   INTEGER NOT NULL DEFAULT 0, -- display order (lower first)
  created_at   TEXT NOT NULL               -- ISO-8601 UTC
);

CREATE TABLE IF NOT EXISTS items (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
  url         TEXT NOT NULL,               -- the product link (the item's identity)
  title       TEXT,                        -- friendly label; UI falls back to the host
  store       TEXT,                        -- brand / retailer
  price_cents INTEGER,                     -- integer cents; NULL = unknown
  image_url   TEXT,                        -- thumbnail
  status      TEXT NOT NULL DEFAULT 'wishlist'
                CHECK (status IN ('wishlist','ordered','owned','skipped')),
  size        TEXT,
  notes       TEXT,
  sort_order  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_items_category ON items(category_id);
CREATE INDEX IF NOT EXISTS idx_items_status   ON items(status);
