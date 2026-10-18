-- User-posted leads: one row per (source, key). Re-posting refreshes in place;
-- clearing deletes. Relay's own leads are projected at read time, never stored.
CREATE TABLE leads (
    source      TEXT    NOT NULL,
    key         TEXT    NOT NULL,
    band        TEXT    NOT NULL,          -- needs_you | live | happened
    title       TEXT    NOT NULL,
    summary     TEXT,
    scope       TEXT,                       -- a folder path
    action      TEXT,
    status      TEXT    NOT NULL,           -- open | resolved | dismissed
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    expires_at  INTEGER,                    -- epoch millis, NULL = no TTL
    PRIMARY KEY (source, key)
);

CREATE INDEX leads_band_idx ON leads (band, updated_at DESC);
CREATE INDEX leads_expires_idx ON leads (expires_at);
