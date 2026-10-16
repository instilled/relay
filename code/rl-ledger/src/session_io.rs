//! `session_io`: journaled terminal traffic per run. The bytes live in
//! append-only files on disk — one per run and direction, under the ledger's
//! io root (`<io>/<run>/{out,in,note}.log`) — so `ledger.db` stays small. The
//! table keeps only where each chunk sits: `(run, seq, direction, pos, len,
//! ts)`, `pos`/`len` a byte range in that direction's file. The io root is
//! where the ledger was opened: a `meta` row set at open, so any `&Connection`
//! can find the files without threading a path through every caller.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rusqlite::{Connection, params};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// `meta` key: the directory the per-run journal files live under, set when
/// the ledger is opened.
pub const IO_ROOT_KEY: &str = "io.root";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    /// A remark by Relay itself (e.g. "output lost while the daemon was
    /// down"); never part of the child's byte stream.
    Note,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
            Self::Note => "note",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "in" => Self::In,
            "note" => Self::Note,
            _ => Self::Out,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IoChunk {
    pub seq: i64,
    pub direction: Direction,
    pub data: Vec<u8>,
    pub ts: Timestamp,
}

/// Where this ledger keeps its journal files, from the `meta` row the opener
/// set. Absent means the ledger was built without one — a programming error,
/// not a runtime condition.
fn io_root(conn: &Connection) -> Result<PathBuf> {
    crate::meta::get(conn, IO_ROOT_KEY)?
        .map(PathBuf::from)
        .context("ledger has no io root (open it with Ledger::open/open_in_memory)")
}

/// The append-only file a run's bytes in one direction are written to.
fn log_path(root: &Path, run: &str, direction: Direction) -> PathBuf {
    root.join(run).join(format!("{}.log", direction.as_str()))
}

/// Reads `len` bytes from `pos` of a run's direction file — the bytes one
/// chunk's row points at.
fn read_at(root: &Path, run: &str, direction: Direction, pos: i64, len: i64) -> Result<Vec<u8>> {
    let mut f = fs::File::open(log_path(root, run, direction))?;
    f.seek(SeekFrom::Start(pos as u64))?;
    let mut buf = vec![0u8; len.max(0) as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Bytes a run's journal takes on disk, across all its directions.
fn run_bytes(root: &Path, run: &str) -> u64 {
    [Direction::Out, Direction::In, Direction::Note]
        .into_iter()
        .map(|d| fs::metadata(log_path(root, run, d)).map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// Reads the last `max_bytes` of a file, or all of it when shorter; empty when
/// it is not there yet.
fn tail_file(path: &Path, max_bytes: usize, from: u64) -> Result<Vec<u8>> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let size = f.seek(SeekFrom::End(0))?;
    let avail = size.saturating_sub(from.min(size));
    let take = avail.min(max_bytes as u64);
    f.seek(SeekFrom::Start(size - take))?;
    let mut out = vec![0u8; take as usize];
    f.read_exact(&mut out)?;
    Ok(out)
}

/// Appends one chunk: its bytes to the direction's file, then a row saying
/// where they sit. Returns the chunk's sequence number (1-based per run). The
/// file is written first, so a crash between the two leaves at most a slab of
/// real output not yet pointed at — never a row pointing past the bytes.
pub fn append(conn: &Connection, run: &str, direction: Direction, data: &[u8], ts: Timestamp) -> Result<i64> {
    let path = log_path(&io_root(conn)?, run, direction);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let pos = f.seek(SeekFrom::End(0))?;
    f.write_all(data)?;
    Ok(conn.query_row(
        "INSERT INTO session_io (run, seq, direction, pos, len, ts) \
         SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5 FROM session_io WHERE run = ?1 \
         RETURNING seq",
        params![run, direction.as_str(), pos as i64, data.len() as i64, to_millis(ts)],
        |r| r.get(0),
    )?)
}

/// Chunks after `after_seq`, oldest first — each row's bytes fetched from its
/// direction file.
pub fn read(conn: &Connection, run: &str, after_seq: i64, limit: u32) -> Result<Vec<IoChunk>> {
    let root = io_root(conn)?;
    let mut stmt = conn.prepare(
        "SELECT seq, direction, pos, len, ts FROM session_io WHERE run = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![run, after_seq, limit], |row| {
            let dir: String = row.get(1)?;
            Ok((
                row.get::<_, i64>(0)?,
                Direction::parse(&dir),
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(seq, direction, pos, len, ts)| {
            Ok(IoChunk {
                seq,
                direction,
                data: read_at(&root, run, direction, pos, len)?,
                ts: from_millis(ts).map_err(|e| crate::conv(4, e))?,
            })
        })
        .collect()
}

/// The last `max_bytes` of output, concatenated — scrollback for a viewer.
/// Straight off `out.log`.
pub fn tail(conn: &Connection, run: &str, max_bytes: usize) -> Result<Vec<u8>> {
    tail_file(&log_path(&io_root(conn)?, run, Direction::Out), max_bytes, 0)
}

/// The last `max_bytes` of output written at or after `from`, concatenated —
/// a stretch of a run for a recap. The table finds where that stretch starts
/// in `out.log`; the file gives the bytes.
pub fn tail_since(conn: &Connection, run: &str, from: Timestamp, max_bytes: usize) -> Result<Vec<u8>> {
    let start: Option<i64> = conn
        .query_row(
            "SELECT pos FROM session_io WHERE run = ?1 AND direction = 'out' AND ts >= ?2 ORDER BY seq LIMIT 1",
            params![run, to_millis(from)],
            |r| r.get(0),
        )
        .ok();
    let Some(start) = start else {
        return Ok(Vec::new());
    };
    tail_file(&log_path(&io_root(conn)?, run, Direction::Out), max_bytes, start as u64)
}

/// Total child output bytes journaled — the offset to resume a replay from.
/// It is the size of `out.log`: the file is the stream, the table only indexes
/// it, so a trailing slab not yet rowed still counts (it is real output).
pub fn out_bytes(conn: &Connection, run: &str) -> Result<u64> {
    match fs::metadata(log_path(&io_root(conn)?, run, Direction::Out)) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

pub fn last_seq(conn: &Connection, run: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_io WHERE run = ?1",
        params![run],
        |r| r.get(0),
    )?)
}

/// The journal of every run that ended before `before`: gone, unless
/// `dry_run`, which only counts. Per run, the bytes it was — read off disk.
/// The files go first (the bulk of it), then the rows that index them.
pub fn drop_ended_before(conn: &Connection, before: Timestamp, dry_run: bool) -> Result<Vec<(String, u64)>> {
    let root = io_root(conn)?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.run FROM session_io s JOIN runs r ON r.id = s.run \
         WHERE r.ended_at IS NOT NULL AND r.ended_at < ?1 ORDER BY r.ended_at",
    )?;
    let runs = stmt
        .query_map(params![to_millis(before)], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let out: Vec<(String, u64)> = runs.into_iter().map(|run| {
        let bytes = run_bytes(&root, &run);
        (run, bytes)
    }).collect();
    if !dry_run {
        for (run, _) in &out {
            match fs::remove_dir_all(root.join(run)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            conn.execute("DELETE FROM session_io WHERE run = ?1", params![run])?;
        }
    }
    Ok(out)
}

/// Removes any `<io>/<run>/` directory with no run left in the ledger — what a
/// crash between a run's file-delete and its row-delete could leave behind.
/// Returns how many went.
pub fn orphan_sweep(conn: &Connection) -> Result<usize> {
    let root = io_root(conn)?;
    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut gone = 0;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(run) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let known: i64 = conn.query_row("SELECT COUNT(*) FROM runs WHERE id = ?1", params![run], |r| r.get(0))?;
        if known == 0 {
            fs::remove_dir_all(entry.path())?;
            gone += 1;
        }
    }
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs;
    use jiff::SignedDuration;
    use rl_proto::types::{Run, RunKind, RunState, Source};

    /// An in-memory ledger whose journal files go to a fresh temp dir — what
    /// `Ledger::open` does with the `meta` row, done by hand here.
    fn mem() -> (Connection, tempfile::TempDir) {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        crate::meta::set(&conn, IO_ROOT_KEY, &dir.path().to_string_lossy()).unwrap();
        (conn, dir)
    }

    fn seed(conn: &Connection, id: &str, now: Timestamp) {
        let r = Run::new(id, RunKind::Command, id, vec![], "/w", "/w", now);
        runs::insert(conn, &r).unwrap();
    }

    #[test]
    fn a_chunk_is_bytes_on_disk_and_a_row_that_points_at_them() {
        let (conn, dir) = mem();
        let now = Timestamp::now();
        seed(&conn, "aaaaaaaa", now);
        assert_eq!(append(&conn, "aaaaaaaa", Direction::Out, b"hello ", now).unwrap(), 1);
        assert_eq!(append(&conn, "aaaaaaaa", Direction::In, b"x", now).unwrap(), 2);
        assert_eq!(append(&conn, "aaaaaaaa", Direction::Out, b"world", now).unwrap(), 3);
        // The out stream is one contiguous file; the row is only an index.
        assert_eq!(std::fs::read(dir.path().join("aaaaaaaa/out.log")).unwrap(), b"hello world");
        let chunks = read(&conn, "aaaaaaaa", 0, 10).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].data, b"hello ");
        assert_eq!(chunks[1].direction, Direction::In);
        assert_eq!(chunks[1].data, b"x");
        assert_eq!(chunks[2].data, b"world");
        assert_eq!(read(&conn, "aaaaaaaa", 1, 10).unwrap().len(), 2, "seq > 1");
    }

    #[test]
    fn out_bytes_is_the_out_files_size_and_the_resume_offset() {
        let (conn, dir) = mem();
        let now = Timestamp::now();
        seed(&conn, "aaaaaaaa", now);
        assert_eq!(out_bytes(&conn, "aaaaaaaa").unwrap(), 0, "nothing journaled yet");
        append(&conn, "aaaaaaaa", Direction::Out, b"hello", now).unwrap();
        append(&conn, "aaaaaaaa", Direction::In, b"ignored for out", now).unwrap();
        append(&conn, "aaaaaaaa", Direction::Out, b" world", now).unwrap();
        // The invariant the replay/resume path leans on.
        assert_eq!(out_bytes(&conn, "aaaaaaaa").unwrap(), 11);
        assert_eq!(
            out_bytes(&conn, "aaaaaaaa").unwrap(),
            std::fs::metadata(dir.path().join("aaaaaaaa/out.log")).unwrap().len()
        );
    }

    #[test]
    fn tail_is_the_last_bytes_of_out() {
        let (conn, _dir) = mem();
        let now = Timestamp::now();
        seed(&conn, "aaaaaaaa", now);
        append(&conn, "aaaaaaaa", Direction::Out, b"0123456789", now).unwrap();
        assert_eq!(tail(&conn, "aaaaaaaa", 4).unwrap(), b"6789");
        assert_eq!(tail(&conn, "aaaaaaaa", 1024).unwrap(), b"0123456789");
        assert_eq!(tail(&conn, "nope", 10).unwrap(), b"");
    }

    #[test]
    fn tail_since_starts_at_the_first_chunk_at_or_after_a_time() {
        let (conn, _dir) = mem();
        let t0 = Timestamp::now();
        let t1 = t0 + SignedDuration::from_secs(10);
        seed(&conn, "aaaaaaaa", t0);
        append(&conn, "aaaaaaaa", Direction::Out, b"early", t0).unwrap();
        append(&conn, "aaaaaaaa", Direction::Out, b"late", t1).unwrap();
        assert_eq!(tail_since(&conn, "aaaaaaaa", t1, 1024).unwrap(), b"late");
        assert_eq!(tail_since(&conn, "aaaaaaaa", t0, 1024).unwrap(), b"earlylate");
        assert_eq!(tail_since(&conn, "aaaaaaaa", t1 + SignedDuration::from_secs(10), 1024).unwrap(), b"");
    }

    #[test]
    fn only_journals_of_runs_ended_before_go() {
        let (conn, dir) = mem();
        let root = dir.path();
        let now = Timestamp::now();
        let mk = |id: &str, ended: Option<Timestamp>| {
            seed(&conn, id, now);
            if let Some(t) = ended {
                runs::finish(&conn, id, RunState::Finished, Source::Pty, Some(0), t).unwrap();
            }
            append(&conn, id, Direction::Out, b"hello", now).unwrap();
            append(&conn, id, Direction::In, b"x", now).unwrap();
        };
        mk("old00001", Some(now - SignedDuration::from_hours(48)));
        mk("new00001", Some(now - SignedDuration::from_hours(1)));
        mk("live0001", None);
        let cut = now - SignedDuration::from_hours(24);
        let dry = drop_ended_before(&conn, cut, true).unwrap();
        assert_eq!(dry, vec![("old00001".to_string(), 6)], "5 out + 1 in");
        assert_eq!(out_bytes(&conn, "old00001").unwrap(), 5, "a dry run keeps it");
        assert!(root.join("old00001").is_dir());
        let gone = drop_ended_before(&conn, cut, false).unwrap();
        assert_eq!(gone, dry);
        assert!(!root.join("old00001").exists(), "dir and bytes gone");
        assert_eq!(last_seq(&conn, "old00001").unwrap(), 0, "rows gone too");
        assert_eq!(out_bytes(&conn, "new00001").unwrap(), 5);
        assert_eq!(out_bytes(&conn, "live0001").unwrap(), 5, "a run still going is never touched");
        assert!(drop_ended_before(&conn, cut, false).unwrap().is_empty());
    }

    #[test]
    fn orphan_sweep_clears_dirs_with_no_run() {
        let (conn, dir) = mem();
        let root = dir.path();
        let now = Timestamp::now();
        seed(&conn, "aaaaaaaa", now);
        append(&conn, "aaaaaaaa", Direction::Out, b"kept", now).unwrap();
        // A directory a crash left behind: no run row for it.
        std::fs::create_dir_all(root.join("ghost0001")).unwrap();
        std::fs::write(root.join("ghost0001/out.log"), b"orphaned").unwrap();
        assert_eq!(orphan_sweep(&conn).unwrap(), 1);
        assert!(!root.join("ghost0001").exists());
        assert!(root.join("aaaaaaaa").is_dir(), "a live run's journal stays");
    }
}
