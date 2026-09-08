//! `runs` table.

use crate::{conv, conv_str};
use anyhow::Result;
use jiff::Timestamp;
use relay_core::time::{from_millis, to_millis};
use relay_proto::methods::RunFilter;
use relay_proto::types::{Confidence, Run, RunState, Source};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use std::path::PathBuf;

const COLS: &str = "id, workspace, kind, title, cli, argv_json, cwd, branch, worktree, \"ref\", tags_json, \
    state, source, confidence, state_at, started_at, ended_at, exit_code, cost_json, \
    schedule_run, plugin, parent_run, last_line";

const DEFAULT_LIMIT: u32 = 200;

pub fn insert(conn: &Connection, r: &Run) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO runs ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)"),
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
            r.schedule_run,
            r.plugin,
            r.parent_run,
            r.last_line,
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
    if !f.states.is_empty() {
        let marks = std::iter::repeat_n("?", f.states.len()).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(" AND state IN ({marks})"));
        args.extend(f.states.iter().map(|s| Value::from(s.as_str().to_string())));
    }
    if f.active_only {
        sql.push_str(" AND state NOT IN ('finished','failed','stopped')");
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
        ended_at: row
            .get::<_, Option<i64>>(16)?
            .map(from_millis)
            .transpose()
            .map_err(|e| conv(16, e))?,
        exit_code: row.get(17)?,
        cost: cost
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| conv(18, e))?,
        schedule_run: row.get(19)?,
        plugin: row.get(20)?,
        parent_run: row.get(21)?,
        last_line: row.get(22)?,
    })
}

fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}
