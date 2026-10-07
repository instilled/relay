-- Schedules and reactions are gone: flows replace them. Nothing is carried over, and what they
-- left is cleared: their runs and all of each run, the questions they raised and what the answers
-- started, and the keys their job rows went by.
CREATE TEMP TABLE old_runs (id TEXT PRIMARY KEY);
INSERT INTO old_runs SELECT id FROM runs WHERE cause LIKE 'schedule:%' OR cause LIKE 'reaction:%';
CREATE TEMP TABLE old_decisions (id TEXT PRIMARY KEY);
INSERT INTO old_decisions SELECT id FROM decisions
  WHERE by_reaction IS NOT NULL OR run IN (SELECT id FROM old_runs);
INSERT OR IGNORE INTO old_runs SELECT id FROM runs
  WHERE cause IN (SELECT 'decision:' || id FROM old_decisions);
DELETE FROM session_io WHERE run IN (SELECT id FROM old_runs);
DELETE FROM sessions WHERE run IN (SELECT id FROM old_runs);
DELETE FROM summaries WHERE run IN (SELECT id FROM old_runs);
DELETE FROM transcripts WHERE run IN (SELECT id FROM old_runs);
DELETE FROM steps WHERE run IN (SELECT id FROM old_runs);
DELETE FROM summary_kept WHERE target LIKE 'j:%' OR target IN (SELECT 'r:' || id FROM old_runs);
DELETE FROM events WHERE run IN (SELECT id FROM old_runs)
  OR kind LIKE 'schedule.%'
  OR json_extract(data_json, '$.decision') IN (SELECT id FROM old_decisions);
DELETE FROM decisions WHERE id IN (SELECT id FROM old_decisions);
DELETE FROM runs WHERE id IN (SELECT id FROM old_runs);
DELETE FROM row_order WHERE row_key LIKE 'j:%' OR row_key IN (SELECT 'r:' || id FROM old_runs);
DELETE FROM hidden WHERE key LIKE 'j:%';
DELETE FROM meta WHERE key = 'reactions.cursor';
DROP TABLE schedules;
DROP TABLE schedule_specs;
DROP TABLE reactions;
DROP TABLE reaction_specs;
ALTER TABLE decisions DROP COLUMN by_reaction;
DROP TABLE old_runs;
DROP TABLE old_decisions;
