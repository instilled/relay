//! `runs` table.

use crate::{conv, conv_str};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, optional_from_millis, to_millis};
use rl_proto::methods::RunFilter;
use rl_proto::types::{Confidence, Run, RunState, Source};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use std::path::PathBuf;

const COLS: &str = "id, workspace, kind, title, cli, argv_json, cwd, branch, worktree, \"ref\", tags_json, \
    state, source, confidence, state_at, started_at, ended_at, exit_code, cost_json, \
    cause, flow, parent_run, last_line, seen_at, detail, archived_at, pinned_at";

const DEFAULT_LIMIT: u32 = 200;

/// The terminal `RunState`s as a SQL `IN (...)` fragment. Must be kept in
/// sync with `RunState::is_terminal()` (checked by a test below).
const TERMINAL_STATES_SQL: &str = "'finished','failed','stopped'";

pub fn insert(conn: &Connection, r: &Run) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO runs ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)"),
        params![
            r.id,
            path_str(&r.workspace),
            r.kind.as_str(),
            r.title,
            r.cli,
            serde_json::to_string(&r.argv)?,
            path_str(&r.cwd),
            r.branch,
            r.worktree.as_deref().map(path_str),
            r.ref_,
            serde_json::to_string(&r.tags)?,
            r.state.as_str(),
            r.source.as_str(),
            r.confidence.as_str(),
            to_millis(r.state_at),
            to_millis(r.started_at),
            r.ended_at.map(to_millis),
            r.exit_code,
            r.cost.as_ref().map(serde_json::to_string).transpose()?,
            r.cause.to_string(),
            r.flow,
            r.parent_run,
            r.last_line,
            r.seen_at.map(to_millis),
            r.detail,
            r.archived_at.map(to_millis),
            r.pinned_at.map(to_millis),
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Run>> {
    Ok(conn
        .query_row(&format!("SELECT {COLS} FROM runs WHERE id = ?1"), params![id], from_row)
        .optional()?)
}

pub fn list(conn: &Connection, f: &RunFilter) -> Result<Vec<Run>> {
    let mut sql = format!("SELECT {COLS} FROM runs WHERE 1=1");
    let mut args: Vec<Value> = Vec::new();
    if let Some(c) = &f.cause {
        sql.push_str(" AND cause = ?");
        args.push(Value::from(c.clone()));
    }
    if !f.states.is_empty() {
        let marks = std::iter::repeat_n("?", f.states.len()).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(" AND state IN ({marks})"));
        args.extend(f.states.iter().map(|s| Value::from(s.as_str().to_string())));
    }
    if f.active_only {
        sql.push_str(&format!(" AND state NOT IN ({TERMINAL_STATES_SQL})"));
    }
    if !f.archived {
        sql.push_str(" AND archived_at IS NULL");
    }
    if let Some(ws) = &f.workspace {
        sql.push_str(" AND workspace = ?");
        args.push(Value::from(path_str(ws)));
    }
    if let Some(g) = &f.group {
        sql.push_str(" AND COALESCE(\"ref\", branch, workspace) = ?");
        args.push(Value::from(g.clone()));
    }
    if let Some(t) = &f.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(tags_json) WHERE value = ?)");
        args.push(Value::from(t.clone()));
    }
    sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
    args.push(Value::from(f.limit.unwrap_or(DEFAULT_LIMIT) as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Records a transition. `state_at` only moves when the state itself does,
/// so a confidence upgrade does not reset "age in state". Returns `false` if
/// the run does not exist.
pub fn set_state(
    conn: &Connection,
    id: &str,
    state: RunState,
    source: Source,
    confidence: Confidence,
    at: Timestamp,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE runs SET state_at = CASE WHEN state = ?2 THEN state_at ELSE ?5 END, \
         state = ?2, source = ?3, confidence = ?4 WHERE id = ?1",
        params![id, state.as_str(), source.as_str(), confidence.as_str(), to_millis(at)],
    )?;
    Ok(n == 1)
}

/// Moves a run to a terminal state with its exit code.
pub fn finish(
    conn: &Connection,
    id: &str,
    state: RunState,
    source: Source,
    exit_code: Option<i32>,
    at: Timestamp,
) -> Result<bool> {
    debug_assert!(state.is_terminal());
    let n = conn.execute(
        "UPDATE runs SET state = ?2, source = ?3, confidence = 'exact', state_at = ?4, ended_at = ?4, exit_code = ?5 WHERE id = ?1",
        params![id, state.as_str(), source.as_str(), to_millis(at), exit_code],
    )?;
    Ok(n == 1)
}

pub fn set_detail(conn: &Connection, id: &str, detail: &str) -> Result<()> {
    conn.execute("UPDATE runs SET detail = ?2 WHERE id = ?1", params![id, detail])?;
    Ok(())
}

/// A run's title, given by hand. Nothing else writes the column after the
/// run starts, so what is set here stands.
pub fn set_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute("UPDATE runs SET title = ?2 WHERE id = ?1", params![id, title])?;
    Ok(())
}

/// Pins a run (`Some`) or lets it go (`None`). Pinning what is already
/// pinned keeps the first time, so the pinned band keeps its order.
pub fn set_pinned(conn: &Connection, id: &str, at: Option<Timestamp>) -> Result<()> {
    conn.execute(
        "UPDATE runs SET pinned_at = CASE WHEN ?2 IS NULL THEN NULL ELSE COALESCE(pinned_at, ?2) END WHERE id = ?1",
        params![id, at.map(to_millis)],
    )?;
    Ok(())
}

pub fn set_last_line(conn: &Connection, id: &str, line: &str) -> Result<()> {
    conn.execute("UPDATE runs SET last_line = ?2 WHERE id = ?1", params![id, line])?;
    Ok(())
}

pub fn set_cost(conn: &Connection, id: &str, cost: &serde_json::Value) -> Result<()> {
    conn.execute(
        "UPDATE runs SET cost_json = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(cost)?],
    )?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    let argv: String = row.get(5)?;
    let tags: String = row.get(10)?;
    let cost: Option<String> = row.get(18)?;
    let state: String = row.get(11)?;
    let source: String = row.get(12)?;
    let confidence: String = row.get(13)?;
    let kind: String = row.get(2)?;
    Ok(Run {
        id: row.get(0)?,
        workspace: PathBuf::from(row.get::<_, String>(1)?),
        kind: kind.parse().map_err(|e| conv_str(2, e))?,
        title: row.get(3)?,
        cli: row.get(4)?,
        argv: serde_json::from_str(&argv).map_err(|e| conv(5, e))?,
        cwd: PathBuf::from(row.get::<_, String>(6)?),
        branch: row.get(7)?,
        worktree: row.get::<_, Option<String>>(8)?.map(PathBuf::from),
        ref_: row.get(9)?,
        tags: serde_json::from_str(&tags).map_err(|e| conv(10, e))?,
        state: state.parse().map_err(|e| conv_str(11, e))?,
        source: source.parse().map_err(|e| conv_str(12, e))?,
        confidence: confidence.parse().map_err(|e| conv_str(13, e))?,
        state_at: from_millis(row.get(14)?).map_err(|e| conv(14, e))?,
        started_at: from_millis(row.get(15)?).map_err(|e| conv(15, e))?,
        ended_at: optional_from_millis(row.get(16)?, 16, conv)?,
        exit_code: row.get(17)?,
        cost: cost
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| conv(18, e))?,
        cause: row.get::<_, String>(19)?.parse().map_err(|e| conv_str(19, e))?,
        flow: row.get(20)?,
        parent_run: row.get(21)?,
        last_line: row.get(22)?,
        seen_at: optional_from_millis(row.get(23)?, 23, conv)?,
        detail: row.get(24)?,
        archived_at: optional_from_millis(row.get(25)?, 25, conv)?,
        pinned_at: optional_from_millis(row.get(26)?, 26, conv)?,
    })
}

/// The digest's candidates: runs with an event at or after `since`, plus
/// every run still waiting on a human or pinned, whatever its age.
pub fn list_for_digest(conn: &Connection, since: Timestamp) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM runs WHERE archived_at IS NULL AND (id IN (SELECT DISTINCT run FROM events WHERE at >= ?1 AND run IS NOT NULL) \
         OR state IN ('needs_input','blocked','failed','stale') OR pinned_at IS NOT NULL) ORDER BY started_at DESC, id DESC LIMIT 1000"
    ))?;
    let rows = stmt.query_map(params![to_millis(since)], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Done with these: out of the panel, kept. Only a run that has ended can
/// be; the count is how many were. Archiving a pinned run is letting it go,
/// so the pin goes with it.
pub fn archive(conn: &Connection, ids: &[String], at: Timestamp) -> Result<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute(
            &format!(
                "UPDATE runs SET archived_at = ?2, pinned_at = NULL WHERE id = ?1 AND archived_at IS NULL \
                 AND state IN ({TERMINAL_STATES_SQL})"
            ),
            params![id, to_millis(at)],
        )?;
    }
    Ok(n)
}

/// Runs lost in a restart that are still on the list, oldest first: ended,
/// not archived, and marked `run.lost` when Relay came back without their
/// holder. Resuming one archives it, so it leaves this list as it is picked up.
pub fn lost(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.id FROM runs r WHERE r.archived_at IS NULL AND r.state IN ({TERMINAL_STATES_SQL}) \
         AND EXISTS (SELECT 1 FROM events e WHERE e.run = r.id AND e.kind = 'run.lost') \
         ORDER BY r.started_at, r.id"
    ))?;
    let ids = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(ids.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Done with everything that has ended and been looked at — except what is
/// pinned, which was kept on purpose and is only let go by name.
pub fn archive_seen(conn: &Connection, at: Timestamp) -> Result<usize> {
    Ok(conn.execute(
        &format!(
            "UPDATE runs SET archived_at = ?1 WHERE archived_at IS NULL AND seen_at IS NOT NULL AND pinned_at IS NULL \
             AND state IN ({TERMINAL_STATES_SQL})"
        ),
        params![to_millis(at)],
    )?)
}

/// The user has looked: these runs' current news is no longer news.
pub fn set_seen(conn: &Connection, ids: &[String], at: Timestamp) -> Result<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute("UPDATE runs SET seen_at = ?2 WHERE id = ?1", params![id, to_millis(at)])?;
    }
    Ok(n)
}

fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TERMINAL_STATES_SQL` is hand-typed SQL; catch it drifting from
    /// `RunState::is_terminal()` if a new state is ever added.
    #[test]
    fn terminal_states_sql_matches_is_terminal() {
        for s in RunState::ALL {
            let listed = TERMINAL_STATES_SQL.contains(&format!("'{}'", s.as_str()));
            assert_eq!(
                listed,
                s.is_terminal(),
                "state {s:?} disagrees with TERMINAL_STATES_SQL"
            );
        }
    }
}
