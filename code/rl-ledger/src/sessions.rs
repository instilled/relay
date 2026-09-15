//! `sessions` table: where each run's holder lives.

use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub run: String,
    pub socket: PathBuf,
    pub exit_file: PathBuf,
    pub log: PathBuf,
    pub pid: u32,
    pub holder_pid: u32,
    pub tty: bool,
    pub cols: u16,
    pub rows: u16,
    pub started_at: Timestamp,
    pub attached_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
}

const COLS: &str = "run, socket, exit_file, log, pid, holder_pid, tty, cols, rows, started_at, attached_at, closed_at";

pub fn insert(conn: &Connection, s: &SessionRow) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO sessions ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"),
        params![
            s.run,
            s.socket.to_string_lossy(),
            s.exit_file.to_string_lossy(),
            s.log.to_string_lossy(),
            s.pid,
            s.holder_pid,
            s.tty as i32,
            s.cols,
            s.rows,
            to_millis(s.started_at),
            s.attached_at.map(to_millis),
            s.closed_at.map(to_millis),
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, run: &str) -> Result<Option<SessionRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM sessions WHERE run = ?1"),
            params![run],
            from_row,
        )
        .optional()?)
}

/// Sessions whose holder has not been closed — candidates for re-attach.
pub fn list_open(conn: &Connection) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM sessions WHERE closed_at IS NULL ORDER BY started_at"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn set_attached(conn: &Connection, run: &str, at: Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET attached_at = ?2 WHERE run = ?1",
        params![run, to_millis(at)],
    )?;
    Ok(())
}

pub fn set_size(conn: &Connection, run: &str, cols: u16, rows: u16) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET cols = ?2, rows = ?3 WHERE run = ?1",
        params![run, cols, rows],
    )?;
    Ok(())
}

pub fn set_closed(conn: &Connection, run: &str, at: Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET closed_at = ?2 WHERE run = ?1",
        params![run, to_millis(at)],
    )?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    let conv = |i, e| crate::conv(i, e);
    Ok(SessionRow {
        run: row.get(0)?,
        socket: PathBuf::from(row.get::<_, String>(1)?),
        exit_file: PathBuf::from(row.get::<_, String>(2)?),
        log: PathBuf::from(row.get::<_, String>(3)?),
        pid: row.get(4)?,
        holder_pid: row.get(5)?,
        tty: row.get::<_, i32>(6)? != 0,
        cols: row.get(7)?,
        rows: row.get(8)?,
        started_at: from_millis(row.get(9)?).map_err(|e| conv(9, e))?,
        attached_at: row
            .get::<_, Option<i64>>(10)?
            .map(from_millis)
            .transpose()
            .map_err(|e| conv(10, e))?,
        closed_at: row
            .get::<_, Option<i64>>(11)?
            .map(from_millis)
            .transpose()
            .map_err(|e| conv(11, e))?,
    })
}
