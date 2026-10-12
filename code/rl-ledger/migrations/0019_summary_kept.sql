-- 0019_summary_kept.sql: a summary of anything — a run read deep, a job, a project, all —
-- over a stretch. A run's own brief recap stays in `summaries`.
CREATE TABLE summary_kept (
  target  TEXT NOT NULL,
  deep    INTEGER NOT NULL,
  from_at INTEGER NOT NULL DEFAULT -1,   -- -1: the whole of it
  agent   TEXT NOT NULL,
  by_run  TEXT NOT NULL,
  at      INTEGER NOT NULL,
  text    TEXT NOT NULL,
  source  TEXT NOT NULL,
  reach   TEXT NOT NULL,
  calls   INTEGER NOT NULL DEFAULT 0,
  runs    INTEGER NOT NULL DEFAULT 0,   -- how many runs it covers
  PRIMARY KEY (target, deep, from_at)
);
