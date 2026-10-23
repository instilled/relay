-- Per-run facts the report reads: a collected record of what a run did.
CREATE TABLE run_facts (
    run        TEXT PRIMARY KEY,
    facts_json TEXT NOT NULL,         -- the serialized RunFacts
    at         TEXT NOT NULL          -- when collected, RFC 3339
);
