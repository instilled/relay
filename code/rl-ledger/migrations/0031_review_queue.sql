-- The review queue: one pointer per session (source, ref) to the point where
-- it last came to rest. A row stays after review so the next stretch starts
-- from where the last was seen.
CREATE TABLE review_queue (
    source    TEXT NOT NULL,
    ref       TEXT NOT NULL,
    kind      TEXT NOT NULL,
    how       TEXT NOT NULL,
    hints     TEXT NOT NULL DEFAULT '[]',
    at        INTEGER NOT NULL,
    rests     INTEGER NOT NULL,
    from_at   INTEGER NOT NULL,
    from_head TEXT,
    reviewed  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source, ref)
);

ALTER TABLE runs ADD COLUMN start_head TEXT;
