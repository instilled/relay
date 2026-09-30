-- Taken off the panel's list (`run.hide`), never stopped or removed by it.
-- NULL: on the list.
ALTER TABLE runs ADD COLUMN hidden_at INTEGER;
-- Projects and jobs taken off the list (`digest.hide`), by their row's key:
-- `g:<folder>` a project, whether registered or not; `j:<kind>:<name>@<group>`
-- a job. Everything under one is hidden with it, runs that come later too.
CREATE TABLE hidden (
  key       TEXT PRIMARY KEY,
  hidden_at INTEGER NOT NULL
);
