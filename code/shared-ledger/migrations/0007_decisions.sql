-- Decisions: a question a run left for a human, with its options and, once
-- answered, what was chosen and what that started.
CREATE TABLE decisions (
  id            TEXT PRIMARY KEY,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  workspace     TEXT NOT NULL,
  run           TEXT,
  flow          TEXT NOT NULL,
  group_key     TEXT,
  title         TEXT NOT NULL,
  summary       TEXT,
  options_json  TEXT NOT NULL,
  multi         INTEGER NOT NULL DEFAULT 0,
  status        TEXT NOT NULL,
  expires_at    INTEGER,
  resolved_at   INTEGER,
  chosen_json   TEXT,
  outcomes_json TEXT,
  note          TEXT
);
CREATE INDEX decisions_status ON decisions (status, created_at DESC);
CREATE INDEX decisions_run ON decisions (run);
-- One open question per group: asking again replaces it.
CREATE UNIQUE INDEX decisions_open_group ON decisions (group_key)
  WHERE status = 'open' AND group_key IS NOT NULL;
