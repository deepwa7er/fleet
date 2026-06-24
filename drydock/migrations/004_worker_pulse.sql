-- 004: worker liveness + progress. Each worker interaction (checking for work,
-- claiming, reporting, or an explicit heartbeat) appends a pulse, so the web view
-- can show whether the Claude Desktop worker is progressing or stuck — visible
-- from the phone, where the Desktop output isn't. Append-only; ticket_id is a
-- soft reference (a pulse may name no ticket), so no FK.

CREATE TABLE IF NOT EXISTS worker_pulse (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id  INTEGER,
  message    TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_worker_pulse_created ON worker_pulse(created_at);
