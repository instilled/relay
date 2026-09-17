-- A summary is a recap of a stretch of a run, live or ended: `since` the
-- stretch (a duration back from when it was made; NULL the whole run),
-- `source` where it came from, `reach` how far the run had got — so asking
-- again with nothing new returns the one kept.
ALTER TABLE summaries ADD COLUMN since TEXT;
ALTER TABLE summaries ADD COLUMN source TEXT NOT NULL DEFAULT '';
ALTER TABLE summaries ADD COLUMN reach TEXT NOT NULL DEFAULT '';

-- The agent's own session file for a run (Claude's transcript, Codex's
-- rollout), once known: from a hook, or found by folder and start time.
CREATE TABLE transcripts (
  run   TEXT PRIMARY KEY REFERENCES runs(id),
  path  TEXT NOT NULL
);
