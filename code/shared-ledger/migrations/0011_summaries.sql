-- An assistant's account of an ended run, one per run, replaced when asked
-- again. `by_run` is the assistant run that wrote it.
CREATE TABLE summaries (
  run     TEXT PRIMARY KEY REFERENCES runs(id),
  agent   TEXT NOT NULL,
  by_run  TEXT NOT NULL,
  at      INTEGER NOT NULL,
  text    TEXT NOT NULL
);
