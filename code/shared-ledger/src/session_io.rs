//! `session_io` table: journaled terminal traffic per run.

use anyhow::Result;
use jiff::Timestamp;
use relay_core::time::{from_millis, to_millis};
use rusqlite::{Connection, params};

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

/// Appends one chunk and returns its sequence number (1-based per run).
pub fn append(conn: &Connection, run: &str, direction: Direction, data: &[u8], ts: Timestamp) -> Result<i64> {
    conn.execute(
        "INSERT INTO session_io (run, seq, direction, data, ts) \
         SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4 FROM session_io WHERE run = ?1",
        params![run, direction.as_str(), data, to_millis(ts)],
    )?;
    Ok(
        conn.query_row("SELECT MAX(seq) FROM session_io WHERE run = ?1", params![run], |r| {
            r.get(0)
        })?,
    )
}

/// Chunks after `after_seq`, oldest first.
pub fn read(conn: &Connection, run: &str, after_seq: i64, limit: u32) -> Result<Vec<IoChunk>> {
    let mut stmt = conn
        .prepare("SELECT seq, direction, data, ts FROM session_io WHERE run = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3")?;
    let rows = stmt.query_map(params![run, after_seq, limit], |row| {
        let dir: String = row.get(1)?;
        Ok(IoChunk {
            seq: row.get(0)?,
            direction: Direction::parse(&dir),
            data: row.get(2)?,
            ts: from_millis(row.get(3)?).map_err(|e| crate::conv(3, e))?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The last `max_bytes` of output, concatenated — scrollback for a viewer.
pub fn tail(conn: &Connection, run: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let mut stmt =
        conn.prepare("SELECT data FROM session_io WHERE run = ?1 AND direction = 'out' ORDER BY seq DESC")?;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    for row in stmt.query_map(params![run], |r| r.get::<_, Vec<u8>>(0))? {
        let data = row?;
        total += data.len();
        chunks.push(data);
        if total >= max_bytes {
            break;
        }
    }
    let mut out: Vec<u8> = chunks.into_iter().rev().flatten().collect();
    if out.len() > max_bytes {
        out.drain(..out.len() - max_bytes);
    }
    Ok(out)
}

/// Total child output bytes journaled — the offset to resume a replay from.
pub fn out_bytes(conn: &Connection, run: &str) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(length(data)), 0) FROM session_io WHERE run = ?1 AND direction = 'out'",
        params![run],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

pub fn last_seq(conn: &Connection, run: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_io WHERE run = ?1",
        params![run],
        |r| r.get(0),
    )?)
}
