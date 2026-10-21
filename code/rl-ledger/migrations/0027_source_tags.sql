-- A user-side overlay: tag a lead source with a co-worker/team name.
-- Set once per source; every future lead from that source inherits it.
CREATE TABLE source_tags (
    source TEXT NOT NULL PRIMARY KEY,
    tag    TEXT NOT NULL
);
