//! What the wiring graph needs from the ledger, as flat rows: the runs in a
//! window, the events they raised counted by who raised them, and the
//! questions flows' ask steps raised. The graph itself is built from these
//! elsewhere.

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
/// that raised them, when one did; else the flow they are from; else only the
/// folder they were raised in.
#[derive(Debug, Clone, PartialEq)]
pub struct EventCount {
    pub kind: String,
    pub workspace: PathBuf,
    /// The raising run's cause, kind and folder.
    pub run: Option<(String, RunKind, PathBuf)>,
    /// With no run: the flow whose step asked the question the event is
    /// about, or whose own `flow.*` event it is.
    pub by_flow: Option<String>,
    pub count: u64,
}

pub fn events_since(conn: &Connection, since: Timestamp) -> Result<Vec<EventCount>> {
    let mut stmt = conn.prepare(
        "SELECT kind, workspace, cause, run_kind, run_workspace, by_flow, COUNT(*) FROM ( \
           SELECT e.kind, e.workspace, r.cause, r.kind AS run_kind, r.workspace AS run_workspace, \
             CASE WHEN r.id IS NOT NULL THEN NULL \
                  WHEN e.kind LIKE 'flow.%' THEN json_extract(e.data_json, '$.flow') \
                  ELSE f.flow END AS by_flow \
           FROM events e \
           LEFT JOIN runs r ON r.id = e.run \
           LEFT JOIN decisions d ON r.id IS NULL AND d.id = json_extract(e.data_json, '$.decision') \
           LEFT JOIN firings f ON f.id = substr(d.by_step, 1, instr(d.by_step, '/') - 1) \
           WHERE e.at >= ?1) \
         GROUP BY kind, workspace, cause, run_kind, run_workspace, by_flow \
         ORDER BY kind, workspace",
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
            by_flow: row.get(5)?,
            count: row.get::<_, i64>(6)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A question a flow's ask step raised.
#[derive(Debug, Clone, PartialEq)]
pub struct AskRow {
    pub id: String,
    /// The flow whose step asked it.
    pub flow: String,
    pub title: String,
    pub status: DecisionStatus,
}

/// Questions flows' ask steps raised in the window, and every one still
/// open — oldest first.
pub fn asks_since(conn: &Connection, since: Timestamp) -> Result<Vec<AskRow>> {
    let mut stmt = conn.prepare(
        "SELECT d.id, f.flow, d.title, d.status FROM decisions d \
         JOIN firings f ON f.id = substr(d.by_step, 1, instr(d.by_step, '/') - 1) \
         WHERE d.created_at >= ?1 OR d.status = 'open' \
         ORDER BY d.created_at, d.id",
    )?;
    let rows = stmt.query_map(params![to_millis(since)], |row| {
        Ok(AskRow {
            id: row.get(0)?,
            flow: row.get(1)?,
            title: row.get(2)?,
            status: row.get::<_, String>(3)?.parse().map_err(|e| conv_str(3, e))?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A question an ask step raised is that flow's, and so are the events
    /// about it and the flow's own events; one a run raised is the run's.
    #[test]
    fn a_question_and_its_events_are_the_flow_whose_step_asked() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        c.execute_batch(
            "INSERT INTO firings (id, flow, def_json, trigger, status, started_at)
               VALUES ('fire0001', 'review', '{}', 'hand', 'waiting', 10);
             INSERT INTO runs (id, workspace, kind, title, argv_json, cwd, state, source, confidence, state_at, started_at, cause)
               VALUES ('run00001', '/w', 'command', 'probe', '[]', '/w', 'finished', 'relay', 'exact', 10, 10, 'flow:nightly/probe');
             INSERT INTO decisions (id, created_at, updated_at, workspace, run, chain, title, options_json, status, by_step)
               VALUES ('dq000001', 10, 10, '/w', NULL, 'fire0001', 'ship it?', '[]', 'open', 'fire0001/ask'),
                      ('dq000002', 10, 10, '/w', 'run00001', 'run00001', 'by rl ask', '[]', 'open', NULL),
                      ('dq000003', 1, 1, '/w', NULL, 'fire0001', 'old', '[]', 'taken', 'fire0001/ask');
             INSERT INTO events (at, workspace, run, group_key, kind, summary, data_json)
               VALUES (10, '/w', NULL, '/w', 'decision.raised', 's', '{\"decision\":\"dq000001\"}'),
                      (10, '/w', 'run00001', '/w', 'decision.raised', 's', '{\"decision\":\"dq000002\"}'),
                      (10, '/h', NULL, '/h', 'flow.fired', 's', '{\"flow\":\"review\"}'),
                      (10, '/w', NULL, '/w', 'logs.issue', 's', NULL);",
        )
        .unwrap();
        let since = from_millis(5).unwrap();
        let asks = asks_since(&c, since).unwrap();
        let said: Vec<(&str, &str, &str)> = asks
            .iter()
            .map(|a| (a.id.as_str(), a.flow.as_str(), a.title.as_str()))
            .collect();
        assert_eq!(
            said,
            [("dq000001", "review", "ship it?")],
            "a run's own question is not a step's; an old answered one is out of the window"
        );
        let events = events_since(&c, since).unwrap();
        let by: Vec<(&str, Option<&str>, Option<&str>)> = events
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.by_flow.as_deref(),
                    e.run.as_ref().map(|r| r.0.as_str()),
                )
            })
            .collect();
        assert_eq!(
            by,
            [
                ("decision.raised", Some("review"), None),
                ("decision.raised", None, Some("flow:nightly/probe")),
                ("flow.fired", Some("review"), None),
                ("logs.issue", None, None),
            ]
        );
    }
}
