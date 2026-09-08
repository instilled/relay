-- Runs: the one noun. Timestamps are epoch milliseconds. JSON columns hold
-- lists/objects that are never queried relationally (argv, tags, cost).
CREATE TABLE runs (
  id           TEXT PRIMARY KEY,
  workspace    TEXT NOT NULL,
  kind         TEXT NOT NULL,
  title        TEXT NOT NULL,
  cli          TEXT,
  argv_json    TEXT NOT NULL,
  cwd          TEXT NOT NULL,
  branch       TEXT,
  worktree     TEXT,
  "ref"        TEXT,
  tags_json    TEXT NOT NULL DEFAULT '[]',
  state        TEXT NOT NULL,
  source       TEXT NOT NULL,
  confidence   TEXT NOT NULL,
  state_at     INTEGER NOT NULL,
  started_at   INTEGER NOT NULL,
  ended_at     INTEGER,
  exit_code    INTEGER,
  cost_json    TEXT,
  schedule_run TEXT,
  plugin       TEXT,
  parent_run   TEXT,
  last_line    TEXT
);
CREATE INDEX runs_state_idx ON runs(state);
CREATE INDEX runs_started_idx ON runs(started_at);

-- Append-only facts. group_key is materialised at write time from the run
-- (ref → branch → workspace) so the digest can group without joining.
CREATE TABLE events (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  at        INTEGER NOT NULL,
  workspace TEXT NOT NULL,
  run       TEXT,
  group_key TEXT NOT NULL,
  kind      TEXT NOT NULL,
  summary   TEXT NOT NULL,
  data_json TEXT
);
CREATE INDEX events_at_idx ON events(at);
CREATE INDEX events_group_idx ON events(group_key, at);
CREATE INDEX events_run_idx ON events(run, at);

-- Last time each client looked. The boundary between "now" and "since".
CREATE TABLE presence (
  client       TEXT PRIMARY KEY,
  last_seen_at INTEGER NOT NULL
);
