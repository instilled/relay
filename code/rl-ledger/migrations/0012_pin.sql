-- Kept at the top of the panel (`run.pin`), however old. NULL: not pinned.
ALTER TABLE runs ADD COLUMN pinned_at INTEGER;
