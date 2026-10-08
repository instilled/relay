-- Flows: the folders in ~/.relay/flows as last loaded, and what each firing did.
CREATE TABLE flows (
  name TEXT PRIMARY KEY,
  hash TEXT NOT NULL,              -- the folder's, less runs/
  def_json TEXT NOT NULL,          -- rl_flow::def::FlowDef as loaded
  approved_hash TEXT,
  paused INTEGER NOT NULL DEFAULT 0,
  last_fired_at INTEGER,
  next_at INTEGER,
  removed_at INTEGER
);
CREATE TABLE firings (
  id TEXT PRIMARY KEY,             -- also its step runs' chain
  flow TEXT NOT NULL,
  def_json TEXT NOT NULL,          -- the definition it runs on, whatever is edited meanwhile
  trigger TEXT NOT NULL,           -- 'when' | 'on' | 'hand'
  event INTEGER,                   -- what fired it, for `on`
  status TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER
);
CREATE INDEX firings_flow ON firings (flow, started_at DESC);
CREATE TABLE steps (
  firing TEXT NOT NULL REFERENCES firings(id),
  name TEXT NOT NULL,
  status TEXT NOT NULL,
  run TEXT,
  decision TEXT,
  PRIMARY KEY (firing, name)
);
CREATE INDEX steps_run ON steps (run);
CREATE INDEX steps_decision ON steps (decision);
-- A question an ask step raised: its answer is the engine's to carry out.
ALTER TABLE decisions ADD COLUMN by_step TEXT;
