-- Whether a run's title is still the one Relay gave it (1) rather than one
-- you gave it (0): an agent you did not name takes its first prompt as its
-- title, once, and a rename by hand always wins. Runs from before are left
-- as they are.
ALTER TABLE runs ADD COLUMN title_auto INTEGER NOT NULL DEFAULT 0;
