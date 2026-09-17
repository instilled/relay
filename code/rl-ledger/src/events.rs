//! `events` table. Append-only; every writer in `rld` that changes anything
//! also appends here, which is what the digest reads.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::methods::EventFilter;
use rl_proto::types::{Event, Run, Signal};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use std::path::PathBuf;

const COLS: &str = "id, at, workspace, run, group_key, kind, summary, data_json";
const DEFAULT_LIMIT: u32 = 500;

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub at: Timestamp,
    pub workspace: PathBuf,
    pub run: Option<String>,
    pub group_key: String,
    pub kind: String,
    pub summary: String,
    pub data: Option<serde_json::Value>,
}

impl NewEvent {
    /// An event about a run, grouped the way the run is.
    pub fn for_run(run: &Run, at: Timestamp, kind: &str, summary: impl Into<String>) -> Self {
        Self {
            at,
            workspace: run.workspace.clone(),
            run: Some(run.id.clone()),
            group_key: run.group_key(),
            kind: kind.to_string(),
            summary: summary.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

pub fn append(conn: &Connection, e: &NewEvent) -> Result<i64> {
    conn.execute(
        "INSERT INTO events (at, workspace, run, group_key, kind, summary, data_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            to_millis(e.at),
            e.workspace.to_string_lossy(),
            e.run,
            e.group_key,
            e.kind,
            e.summary,
            e.data.as_ref().map(serde_json::to_string).transpose()?,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list(conn: &Connection, f: &EventFilter) -> Result<Vec<Event>> {
    let mut sql = format!("SELECT {COLS} FROM events WHERE 1=1");
    let mut args: Vec<Value> = Vec::new();
    if let Some(since) = f.since {
        sql.push_str(" AND at >= ?");
        args.push(Value::from(to_millis(since)));
    }
    if let Some(run) = &f.run {
        sql.push_str(" AND run = ?");
        args.push(Value::from(run.clone()));
    }
    if let Some(g) = &f.group {
        sql.push_str(" AND group_key = ?");
        args.push(Value::from(g.clone()));
    }
    if !f.kinds.is_empty() {
        let marks = std::iter::repeat_n("?", f.kinds.len()).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(" AND kind IN ({marks})"));
        args.extend(f.kinds.iter().map(|k| Value::from(k.clone())));
    }
    sql.push_str(" ORDER BY at ASC, id ASC LIMIT ?");
    args.push(Value::from(f.limit.unwrap_or(DEFAULT_LIMIT) as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One event by id.
pub fn get(conn: &Connection, id: i64) -> Result<Option<Event>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM events WHERE id = ?1"),
            params![id],
            from_row,
        )
        .optional()?)
}

/// Events after `id`, oldest first: how the reactor reads the stream.
pub fn after(conn: &Connection, id: i64, limit: u32) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM events WHERE id > ?1 ORDER BY id LIMIT ?2"))?;
    let rows = stmt.query_map(params![id, limit], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The newest event's id, or 0 when there are none.
pub fn last_id(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?)
}

/// The newest event of a run other than `run.started` (that one if it is
/// all there is).
pub fn latest_for_run(conn: &Connection, run: &str) -> Result<Option<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM events WHERE run = ?1 ORDER BY (kind = 'run.started') ASC, at DESC, id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map(params![run], from_row)?;
    Ok(rows.next().transpose()?)
}

/// What a run raised itself (`rl emit`): each kind and how often, in the
/// order they first came. The daemon's own families are left out — those are
/// its own doing, and the run's state already says them.
pub fn emitted_for_run(conn: &Connection, run: &str) -> Result<Vec<Signal>> {
    // Each kind once, counted, with the newest one's own words.
    let mut stmt = conn.prepare(
        "SELECT e.kind, c.n, e.summary, e.at FROM events e
         JOIN (SELECT kind, COUNT(*) n, MAX(id) last_id, MIN(id) first_id
               FROM events WHERE run = ?1 GROUP BY kind) c ON e.id = c.last_id
         ORDER BY c.first_id",
    )?;
    let rows = stmt.query_map(params![run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(kind, ..)| !rl_proto::types::event_kind::is_own(kind))
        .map(|(kind, count, said, at)| {
            Ok(Signal {
                kind,
                count,
                said,
                at: from_millis(at)?,
            })
        })
        .collect()
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let data: Option<String> = row.get(7)?;
    Ok(Event {
        id: row.get(0)?,
        at: from_millis(row.get(1)?).map_err(|e| conv(1, e))?,
        workspace: PathBuf::from(row.get::<_, String>(2)?),
        run: row.get(3)?,
        group_key: row.get(4)?,
        kind: row.get(5)?,
        summary: row.get(6)?,
        data: data
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| conv(7, e))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_run_raised_leaves_out_the_daemons_own() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        let t: jiff::Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let r = rl_proto::types::Run::new(
            "r1",
            rl_proto::types::RunKind::Command,
            "probe",
            vec!["probe".into()],
            "/w",
            "/w",
            t,
        );
        crate::runs::insert(&c, &r).unwrap();
        let ev = |kind: &str| NewEvent {
            at: t,
            workspace: PathBuf::from("/w"),
            run: Some("r1".into()),
            group_key: "/w".into(),
            kind: kind.into(),
            summary: kind.into(),
            data: None,
        };
        for (kind, said) in [
            ("run.started", "started"),
            ("flint.product.new-feedback", "2 rows to process"),
            ("log.issue", "a note"),
            ("flint.product.new-feedback", "3 rows to process"),
            ("run.finished", "exit 0"),
            ("flint.product.new-feedback", "5 rows to process"),
        ] {
            let mut e = ev(kind);
            e.summary = said.into();
            append(&c, &e).unwrap();
        }
        let raised = emitted_for_run(&c, "r1").unwrap();
        assert_eq!(
            raised.iter().map(|s| (s.kind.as_str(), s.count)).collect::<Vec<_>>(),
            [("flint.product.new-feedback", 3), ("log.issue", 1)],
            "each kind once, in the order they first came"
        );
        assert_eq!(
            raised.iter().map(|s| s.said.as_str()).collect::<Vec<_>>(),
            ["5 rows to process", "a note"],
            "the newest one's own words"
        );
        assert!(emitted_for_run(&c, "nobody").unwrap().is_empty());
    }
}
