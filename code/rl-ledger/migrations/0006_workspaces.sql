-- Workspaces: registered folders, the trust boundary for their .relay/.
-- A schedule version fires only once approved.
CREATE TABLE workspaces (
  path          TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  registered_at INTEGER NOT NULL
);
ALTER TABLE schedules ADD COLUMN approved_hash TEXT;
-- Everything loaded so far came from the user's own file.
UPDATE schedules SET approved_hash = hash WHERE scope = 'global';
