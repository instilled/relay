-- How far into a run's session file its agent's own recap has been read, in
-- bytes: the next read starts there, so a transcript of many MB is read once.
-- Back to 0 when the run names another file.
ALTER TABLE transcripts ADD COLUMN read_to INTEGER NOT NULL DEFAULT 0;
