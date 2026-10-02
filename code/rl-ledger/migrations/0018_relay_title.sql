-- The name Relay gave a run — the one it started with, or the one its first
-- prompt gave it — kept beside `title`, so a rename by hand can be undone. A
-- run whose title is still Relay's has it already; for the rest it is not
-- known, and stays empty.
ALTER TABLE runs ADD COLUMN relay_title TEXT;
UPDATE runs SET relay_title = title WHERE title_auto = 1;
