//! What the wiring graph needs from the ledger, as flat rows: the runs in a
//! window, the events they raised counted by who raised them, and the
//! questions reactions asked. The graph itself is built from these elsewhere.

use crate::{conv, conv_str};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::decision::DecisionStatus;
use rl_proto::types::{RunKind, RunState};
use rusqlite::{Connection, params};
use std::path::PathBuf;

/// One run, as much of it as the graph counts.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub cause: String,
    pub kind: RunKind,
    pub workspace: PathBuf,
    pub state: RunState,
    pub started_at: Timestamp,
}

/// Runs started at or after `since`, and any still going however old, oldest
/// first — so the last one seen for anything is its latest.
pub fn runs_since(conn: &Connection, since: Timestamp) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(
        "SELECT cause, kind, workspace, state, started_at FROM runs \
         WHERE started_at >= ?1 OR state NOT IN ('finished','failed','stopped') \
         ORDER BY started_at, id",
    )?;
    let rows = stmt.query_map(params![to_millis(since)], |row| {
        Ok(RunRow {
            cause: row.get(0)?,
            kind: row.get::<_, String>(1)?.parse().map_err(|e| conv_str(1, e))?,
            workspace: PathBuf::from(row.get::<_, String>(2)?),
            state: row.get::<_, String>(3)?.parse().map_err(|e| conv_str(3, e))?,
            started_at: from_millis(row.get(4)?).map_err(|e| conv(4, e))?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Events of one kind from one producer, counted. The producer is the run
/// that raised them, when one did; else the reaction whose question they are
/// about; else only the folder they were raised in.
#[derive(Debug, Clone, PartialEq)]
pub struct EventCount {
    pub kind: String,
    pub workspace: PathBuf,
    /// The raising run's cause, kind and folder.
    pub run: Option<(String, RunKind, PathBuf)>,
    /// With no run: the reaction that asked the question the event is about.
    pub asked_by: Option<String>,
    pub count: u64,
}

pub fn events_since(conn: &Connection, since: Timestamp) -> Result<Vec<EventCount>> {
    let mut stmt = conn.prepare(
        "SELECT e.kind, e.workspace, r.cause, r.kind, r.workspace, d.by_reaction, COUNT(*) \
         FROM events e \
         LEFT JOIN runs r ON r.id = e.run \
         LEFT JOIN decisions d ON r.id IS NULL AND d.id = json_extract(e.data_json, '$.decision') \
         WHERE e.at >= ?1 \
         GROUP BY e.kind, e.workspace, r.cause, r.kind, r.workspace, d.by_reaction \
         ORDER BY e.kind, e.workspace",
    )?;
    let rows = stmt.query_map(params![to_millis(since)], |row| {
        let cause: Option<String> = row.get(2)?;
        let run = match cause {
            Some(cause) => Some((
                cause,
                row.get::<_, String>(3)?.parse().map_err(|e| conv_str(3, e))?,
                PathBuf::from(row.get::<_, String>(4)?),
            )),
            None => None,
        };
        Ok(EventCount {
            kind: row.get(0)?,
            workspace: PathBuf::from(row.get::<_, String>(1)?),
            run,
            asked_by: row.get(5)?,
            count: row.get::<_, i64>(6)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A question a reaction asked.
#[derive(Debug, Clone, PartialEq)]
pub struct AskRow {
    pub id: String,
    pub by_reaction: String,
    pub workspace: PathBuf,
    pub status: DecisionStatus,
    pub chosen: Vec<String>,
}

/// Questions reactions asked in the window, every one still open, and the
/// ones whose answers started a run in the window — oldest first.
pub fn asks_since(conn: &Connection, since: Timestamp) -> Result<Vec<AskRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, by_reaction, workspace, status, chosen_json FROM decisions \
         WHERE by_reaction IS NOT NULL AND (created_at >= ?1 OR status = 'open' \
           OR id IN (SELECT substr(cause, 10) FROM runs WHERE cause LIKE 'decision:%' AND started_at >= ?1)) \
         ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(params![to_millis(since)], |row| {
        let chosen: Vec<String> = row
            .get::<_, Option<String>>(4)?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| conv(4, e))?
            .unwrap_or_default();
        Ok(AskRow {
            id: row.get(0)?,
            by_reaction: row.get(1)?,
            workspace: PathBuf::from(row.get::<_, String>(2)?),
            status: row.get::<_, String>(3)?.parse().map_err(|e| conv_str(3, e))?,
            chosen,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
