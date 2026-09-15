-- The `run:` grammar changed: what used to be a top-level `agent` with a
-- `prompt` beside it is now `run: {agent: …, prompt: …}`. Stored specs written
-- the old way can no longer be read, so they go; the files they came from are
-- re-read on the next start, and a workspace approves the new version once.
--
-- Command specs (`run:` as a list) read the same either way and are left
-- alone. This runs before any new-style row can exist, so matching on the old
-- key is safe here and only here.
DELETE FROM schedule_specs WHERE spec_json LIKE '%"agent":%';
DELETE FROM schedules WHERE hash NOT IN (SELECT hash FROM schedule_specs);
DELETE FROM decisions WHERE options_json LIKE '%"agent":%';
