-- A user-side overlay: leads the user has dismissed from the report, by identity.
-- Applies to built-in (computed) leads as well as stored ones.
CREATE TABLE dismissals (
    source TEXT    NOT NULL,
    key    TEXT    NOT NULL,
    at     INTEGER NOT NULL,          -- epoch millis, when dismissed
    PRIMARY KEY (source, key)
);
