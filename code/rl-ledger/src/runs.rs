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
    cause, flow, parent_run, last_line, seen_at, detail, archived_at, pinned_at, hidden_at";

const DEFAULT_LIMIT: u32 = 200;

/// The terminal `RunState`s as a SQL `IN (...)` fragment. Must be kept in
/// sync with `RunState::is_terminal()` (checked by a test below).
const TERMINAL_STATES_SQL: &str = "'finished','failed','stopped'";

/// A run, as it starts. The title it starts with is Relay's own as well
/// (`relay_title`), for an empty rename to go back to.
pub fn insert(conn: &Connection, r: &Run) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO runs ({COLS}, relay_title) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?4)"),
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
            r.hidden_at.map(to_millis),
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

/// A run's title, given by hand. It is yours from then on: nothing of
/// Relay's writes the column again (`name_once` leaves it be) — until
/// `restore_title` gives it back.
pub fn set_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute("UPDATE runs SET title = ?2 WHERE id = ?1", params![id, title])?;
    Ok(())
}

/// Undoes a rename by hand: the run goes back to the name Relay gave it, and
/// one Relay has yet to name for its first prompt still will be. False when
/// there is none to go back to — a run from before Relay kept it — or no
/// such run.
pub fn restore_title(conn: &Connection, id: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE runs SET title = relay_title WHERE id = ?1 AND relay_title IS NOT NULL",
        params![id],
    )?;
    Ok(n == 1)
}

/// Says a run's title is the one Relay made up for it, nobody having given
/// it one — so the run may still be named for what it was asked (`name_once`).
pub fn set_title_auto(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE runs SET title_auto = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

/// Names a run Relay titled, once: the title is set only while it is still
/// Relay's own, and is not Relay's to change after. False when it was not
/// (named by hand, already named once, or no such run).
pub fn name_once(conn: &Connection, id: &str, title: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE runs SET title = ?2, relay_title = ?2, title_auto = 0 \
         WHERE id = ?1 AND title_auto = 1 AND title IS relay_title",
        params![id, title],
    )?;
    Ok(n == 1)
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

/// Takes a run off the panel's list (`Some`, from then) or brings it back
/// (`None`). Hiding what is already hidden moves the time on: whatever came
/// through since, because it needed you, goes back.
pub fn set_hidden(conn: &Connection, id: &str, at: Option<Timestamp>) -> Result<()> {
    conn.execute(
        "UPDATE runs SET hidden_at = ?2 WHERE id = ?1",
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
        hidden_at: optional_from_millis(row.get(27)?, 27, conv)?,
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

/// Claude and Codex runs whose own recap may have moved on: waiting on you,
/// or ended at or after `ended_since`. Never an assistant's own run.
pub fn recappable(conn: &Connection, ended_since: Timestamp) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM runs WHERE cli IN ('claude','codex') AND cause NOT LIKE 'assistant:%' \
         AND archived_at IS NULL AND (state IN ('idle','needs_input','blocked','stale') \
         OR (state IN ({TERMINAL_STATES_SQL}) AND ended_at >= ?1))"
    ))?;
    let rows = stmt.query_map(params![to_millis(ended_since)], from_row)?;
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

/// Done with everything that has ended in one folder, as when its project is
/// removed: the runs whose `workspace` is exactly `workspace` (a folder nested
/// inside is a project of its own) go, pins and all. Runs still going stay.
pub fn archive_workspace(conn: &Connection, workspace: &std::path::Path, at: Timestamp) -> Result<usize> {
    Ok(conn.execute(
        &format!(
            "UPDATE runs SET archived_at = ?2, pinned_at = NULL WHERE workspace = ?1 AND archived_at IS NULL \
             AND state IN ({TERMINAL_STATES_SQL})"
        ),
        params![path_str(workspace), to_millis(at)],
    )?)
}

/// The runs in one folder that have not ended, oldest first.
pub fn live_in(conn: &Connection, workspace: &std::path::Path) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM runs WHERE workspace = ?1 AND state NOT IN ({TERMINAL_STATES_SQL}) \
         ORDER BY started_at, id"
    ))?;
    let rows = stmt.query_map(params![path_str(workspace)], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
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

/// When the oldest run caused by `cause` that you have not seen since it
/// ended did end; None when every one is seen. With `workspace`, only that
/// folder's runs.
pub fn oldest_unseen(conn: &Connection, cause: &str, workspace: Option<&std::path::Path>) -> Result<Option<Timestamp>> {
    let ms: Option<i64> = conn.query_row(
        "SELECT MIN(ended_at) FROM runs WHERE cause = ?1 AND ended_at IS NOT NULL AND archived_at IS NULL \
         AND (seen_at IS NULL OR seen_at < ended_at) AND (?2 IS NULL OR workspace = ?2)",
        params![cause, workspace.map(path_str)],
        |r| r.get(0),
    )?;
    ms.map(from_millis).transpose()
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

    fn test_conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        c
    }

    /// A run of `cause` that ended at `ended_at`, looked at `seen_at`.
    fn insert_ended(c: &Connection, id: &str, cause: &str, ended_at: Timestamp, seen_at: Option<Timestamp>) {
        let mut r = Run::new(id, rl_proto::types::RunKind::Command, id, vec![], "/w", "/w", ended_at);
        r.cause = cause.parse().unwrap();
        r.state = RunState::Finished;
        r.ended_at = Some(ended_at);
        r.seen_at = seen_at;
        insert(c, &r).unwrap();
    }

    #[test]
    fn the_oldest_unseen_firing_is_where_a_job_starts() {
        let c = test_conn();
        let t = |ms| Timestamp::from_millisecond(ms).unwrap();
        insert_ended(&c, "a", "schedule:nightly", t(1_000), None); // unseen
        insert_ended(&c, "b", "schedule:nightly", t(2_000), Some(t(2_500))); // seen
        insert_ended(&c, "x", "schedule:other", t(500), None);
        assert_eq!(oldest_unseen(&c, "schedule:nightly", None).unwrap(), Some(t(1_000)));
        assert_eq!(
            oldest_unseen(&c, "schedule:nightly", Some(std::path::Path::new("/elsewhere"))).unwrap(),
            None,
            "only the rule's own folder's"
        );
        set_seen(&c, &["a".into()], t(3_000)).unwrap();
        assert_eq!(oldest_unseen(&c, "schedule:nightly", None).unwrap(), None);
    }

    #[test]
    fn recappable_runs_are_claude_and_codex_waiting_or_just_ended() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        let t: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let mk = |id: &str, cli: Option<&str>, state: RunState| {
            let mut r = Run::new(id, rl_proto::types::RunKind::Agent, id, vec![], "/w", "/w", t);
            r.cli = cli.map(String::from);
            r.state = state;
            r
        };
        for r in [
            mk("idle", Some("claude"), RunState::Idle),
            mk("asks", Some("codex"), RunState::NeedsInput),
            mk("busy", Some("claude"), RunState::Working),
            mk("shell", None, RunState::Idle),
            mk("gemini", Some("gemini"), RunState::Idle),
            mk("ended", Some("claude"), RunState::Working),
            mk("old", Some("claude"), RunState::Working),
        ] {
            insert(&c, &r).unwrap();
        }
        let mut asked = mk("asked", Some("claude"), RunState::Idle);
        asked.cause = rl_proto::types::Cause::Assistant("summary".into());
        insert(&c, &asked).unwrap();
        finish(
            &c,
            "ended",
            RunState::Finished,
            Source::Pty,
            Some(0),
            "2026-09-10T12:30:00Z".parse().unwrap(),
        )
        .unwrap();
        finish(
            &c,
            "old",
            RunState::Failed,
            Source::Pty,
            Some(1),
            "2026-09-10T11:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let mut ids: Vec<String> = recappable(&c, "2026-09-10T12:20:00Z".parse().unwrap())
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        assert_eq!(ids, ["asks", "ended", "idle"]);
    }
}
