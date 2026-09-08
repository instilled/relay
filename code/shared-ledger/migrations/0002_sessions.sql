-- One holder per run. Enough to find and re-attach to it after a daemon
-- restart, and to know it was closed.
CREATE TABLE sessions (
  run         TEXT PRIMARY KEY REFERENCES runs(id),
  socket      TEXT NOT NULL,
  exit_file   TEXT NOT NULL,
  log         TEXT NOT NULL,
  pid         INTEGER NOT NULL,
  holder_pid  INTEGER NOT NULL,
  tty         INTEGER NOT NULL,
  cols        INTEGER NOT NULL,
  rows        INTEGER NOT NULL,
  started_at  INTEGER NOT NULL,
  attached_at INTEGER,
  closed_at   INTEGER
);

-- Journaled terminal traffic, in order. `direction` is 'out' or 'in'.
CREATE TABLE session_io (
  run       TEXT NOT NULL REFERENCES runs(id),
  seq       INTEGER NOT NULL,
  direction TEXT NOT NULL,
  data      BLOB NOT NULL,
  ts        INTEGER NOT NULL,
  PRIMARY KEY (run, seq)
);
