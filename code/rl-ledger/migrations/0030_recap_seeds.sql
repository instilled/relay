-- The agent's own recap of a run (Claude's `away_summary`, Codex's last
-- turn), kept apart from what an assistant writes: a seed is input and a free
-- fallback, never a written recap, and never replaces one.
CREATE TABLE recap_seeds (
  run     TEXT PRIMARY KEY,
  agent   TEXT NOT NULL,
  at      INTEGER NOT NULL,
  text    TEXT NOT NULL,
  source  TEXT NOT NULL,
  reach   TEXT NOT NULL
);
-- Seeds so far lived in `summaries` with no assistant run behind them. The
-- engine's own "nothing new" results have none either, but they are written
-- results, not seeds: they stay.
INSERT INTO recap_seeds (run, agent, at, text, source, reach)
  SELECT run, agent, at, text, source, reach FROM summaries
  WHERE by_run = '' AND source != 'nothing new';
DELETE FROM summaries WHERE by_run = '' AND source != 'nothing new';
