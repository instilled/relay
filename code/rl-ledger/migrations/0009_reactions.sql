-- Reactions: events → an action, or a question. Same two-table shape as
-- schedules (a version by hash, an identity by scope and name) minus the one
-- thing only a schedule has, its next firing.
CREATE TABLE reaction_specs (
  hash          TEXT PRIMARY KEY,
  spec_json     TEXT NOT NULL,
  first_seen_at INTEGER NOT NULL
);

CREATE TABLE reactions (
  scope         TEXT NOT NULL,
  name          TEXT NOT NULL,
  hash          TEXT NOT NULL REFERENCES reaction_specs(hash),
  source        TEXT NOT NULL,
  paused        INTEGER NOT NULL DEFAULT 0,
  last_fired_at INTEGER,
  last_run      TEXT,
  removed_at    INTEGER,
  approved_hash TEXT,
  PRIMARY KEY (scope, name)
);

-- The daemon's own bookkeeping: how far the reactor has read the events.
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Which reaction asked a question, so a rule never answers its own.
ALTER TABLE decisions ADD COLUMN by_reaction TEXT;
