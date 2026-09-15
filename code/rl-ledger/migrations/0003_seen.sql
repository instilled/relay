-- When the user last looked at a run's news. NULL: never.
ALTER TABLE runs ADD COLUMN seen_at INTEGER;
