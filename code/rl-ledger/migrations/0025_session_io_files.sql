-- Terminal bytes move out of SQLite into append-only files per run and
-- direction (`io/<run>/{out,in,note}.log`); the table keeps only where each
-- chunk sits. The old inline `data` BLOBs are dropped — terminal bytes are
-- ephemeral scrollback the daemon re-journals, and it is restarted on upgrade.
DROP TABLE session_io;
CREATE TABLE session_io (
  run       TEXT NOT NULL REFERENCES runs(id),
  seq       INTEGER NOT NULL,
  direction TEXT NOT NULL,
  pos       INTEGER NOT NULL,
  len       INTEGER NOT NULL,
  ts        INTEGER NOT NULL,
  PRIMARY KEY (run, seq)
);
