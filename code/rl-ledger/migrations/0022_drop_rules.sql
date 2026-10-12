-- Schedules and reactions are gone: flows replace them. Nothing is carried over, and what they
-- left is cleared: their runs and all of each run, the questions they raised and what the answers
-- started, and the keys their job rows went by.
-- What they left, followed down: a run, the questions it raised, the runs their answers started,
-- the questions those raised, and on. Never by chain: a reaction's runs took the chain of the run
-- that set it off, so a chain can hold runs started by hand.
CREATE TEMP TABLE gone (kind TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY (kind, id));
WITH RECURSIVE left_by(kind, id) AS (
  SELECT 'run', id FROM runs WHERE cause LIKE 'schedule:%' OR cause LIKE 'reaction:%'
  UNION SELECT 'decision', id FROM decisions WHERE by_reaction IS NOT NULL
  UNION SELECT 'decision', d.id FROM decisions d JOIN left_by l ON l.kind = 'run' AND d.run = l.id
  UNION SELECT 'run', r.id FROM runs r JOIN left_by l ON l.kind = 'decision' AND r.cause = 'decision:' || l.id
)
INSERT INTO gone SELECT kind, id FROM left_by;
CREATE TEMP TABLE old_runs (id TEXT PRIMARY KEY);
INSERT INTO old_runs SELECT id FROM gone WHERE kind = 'run';
CREATE TEMP TABLE old_decisions (id TEXT PRIMARY KEY);
INSERT INTO old_decisions SELECT id FROM gone WHERE kind = 'decision';
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
DROP TABLE gone;
DROP TABLE old_runs;
DROP TABLE old_decisions;
