-- Why a run started, and the chain it belongs to; the scheduler's tables.
ALTER TABLE runs DROP COLUMN schedule_run;
ALTER TABLE runs DROP COLUMN plugin;
ALTER TABLE runs ADD COLUMN cause TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE runs ADD COLUMN flow TEXT;
UPDATE runs SET flow = id WHERE flow IS NULL;

CREATE TABLE schedule_specs (
  hash          TEXT PRIMARY KEY,
  spec_json     TEXT NOT NULL,
  first_seen_at INTEGER NOT NULL
);

CREATE TABLE schedules (
  scope         TEXT NOT NULL,
  name          TEXT NOT NULL,
  hash          TEXT NOT NULL REFERENCES schedule_specs(hash),
  source        TEXT NOT NULL,
  paused        INTEGER NOT NULL DEFAULT 0,
  last_fired_at INTEGER,
  last_run      TEXT,
  next_at       INTEGER,
  removed_at    INTEGER,
  PRIMARY KEY (scope, name)
);
