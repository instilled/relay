//! Decisions: questions waiting on a human, and how they were answered.
//!
//! One row per question. A `group_key` is unique among open rows, so raising
//! the same question again replaces the one already waiting instead of piling
//! another line on top of it.

use crate::{conv, conv_str};
use anyhow::{Result, bail};
use jiff::Timestamp;
use rl_core::time::{from_millis, optional_from_millis, to_millis};
use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
use rl_proto::methods::DecisionFilter;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use std::path::PathBuf;

const COLS: &str = "id, created_at, updated_at, workspace, run, flow, group_key, title, summary, options_json, \
     multi, status, expires_at, resolved_at, chosen_json, outcomes_json, note";
const DEFAULT_LIMIT: u32 = 100;

/// Writes a new question. Returns it as stored.
pub fn insert(conn: &Connection, d: &Decision) -> Result<Decision> {
    conn.execute(
        "INSERT INTO decisions (id, created_at, updated_at, workspace, run, flow, group_key, title, summary, \
         options_json, multi, status, expires_at, resolved_at, chosen_json, outcomes_json, note) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            d.id,
            to_millis(d.created_at),
            to_millis(d.updated_at),
            d.workspace.to_string_lossy(),
            d.run,
            d.flow,
            d.group,
            d.title,
            d.summary,
            serde_json::to_string(&d.options)?,
            d.multi,
            d.status.as_str(),
            d.expires_at.map(to_millis),
            d.resolved_at.map(to_millis),
            serde_json::to_string(&d.chosen)?,
            serde_json::to_string(&d.outcome_runs)?,
            d.note,
        ],
    )?;
    get(conn, &d.id)?.ok_or_else(|| anyhow::anyhow!("decision {} vanished", d.id))
}

/// Replaces the wording and options of an open question, keeping its id.
pub fn restate(conn: &Connection, id: &str, d: &Decision, now: Timestamp) -> Result<Decision> {
    conn.execute(
        "UPDATE decisions SET updated_at = ?2, run = ?3, flow = ?4, title = ?5, summary = ?6, options_json = ?7, \
         multi = ?8, expires_at = ?9, workspace = ?10 WHERE id = ?1 AND status = 'open'",
        params![
            id,
            to_millis(now),
            d.run,
            d.flow,
            d.title,
            d.summary,
            serde_json::to_string(&d.options)?,
            d.multi,
            d.expires_at.map(to_millis),
            d.workspace.to_string_lossy(),
        ],
    )?;
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("decision {id} vanished"))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Decision>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM decisions WHERE id = ?1"),
            params![id],
            from_row,
        )
        .optional()?)
}

/// The open question of a group, if there is one.
pub fn open_in_group(conn: &Connection, group: &str) -> Result<Option<Decision>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM decisions WHERE group_key = ?1 AND status = 'open'"),
            params![group],
            from_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection, f: &DecisionFilter) -> Result<Vec<Decision>> {
    let mut sql = format!("SELECT {COLS} FROM decisions WHERE 1=1");
    let mut args: Vec<Value> = Vec::new();
    if !f.all {
        sql.push_str(" AND status = 'open'");
    }
    if let Some(run) = &f.run {
        args.push(run.clone().into());
        sql.push_str(&format!(" AND run = ?{}", args.len()));
    }
    if let Some(flow) = &f.flow {
        args.push(flow.clone().into());
        sql.push_str(&format!(" AND flow = ?{}", args.len()));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ");
    sql.push_str(&f.limit.unwrap_or(DEFAULT_LIMIT).to_string());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The open questions asked in one folder, as when its project is removed.
pub fn open_in(conn: &Connection, workspace: &std::path::Path) -> Result<Vec<Decision>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM decisions WHERE status = 'open' AND workspace = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map(params![workspace.to_string_lossy()], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Open questions whose time is up.
pub fn due(conn: &Connection, now: Timestamp) -> Result<Vec<Decision>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM decisions WHERE status = 'open' AND expires_at IS NOT NULL AND expires_at <= ?1 \
         ORDER BY expires_at"
    ))?;
    let rows = stmt.query_map(params![to_millis(now)], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// When the earliest open question runs out of time, if any does.
pub fn next_expiry(conn: &Connection) -> Result<Option<Timestamp>> {
    let ms: Option<i64> = conn.query_row(
        "SELECT MIN(expires_at) FROM decisions WHERE status = 'open' AND expires_at IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    ms.map(from_millis).transpose()
}

/// Answers an open question. Refuses one already answered, so two `rl decide`
/// racing cannot both start the follow-up runs.
pub fn resolve(
    conn: &Connection,
    id: &str,
    status: DecisionStatus,
    chosen: &[String],
    outcomes: &[String],
    note: Option<&str>,
    now: Timestamp,
) -> Result<Decision> {
    let n = conn.execute(
        "UPDATE decisions SET status = ?2, chosen_json = ?3, outcomes_json = ?4, note = ?5, resolved_at = ?6, \
         updated_at = ?6 WHERE id = ?1 AND status = 'open'",
        params![
            id,
            status.as_str(),
            serde_json::to_string(chosen)?,
            serde_json::to_string(outcomes)?,
            note,
            to_millis(now),
        ],
    )?;
    if n == 0 {
        match get(conn, id)? {
            Some(d) => bail!("decision {id} is already {}", d.status),
            None => bail!("no decision {id}"),
        }
    }
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("decision {id} vanished"))
}

/// Which reaction raised this question, if one did.
pub fn by_reaction(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT by_reaction FROM decisions WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten())
}

/// Records that a reaction is behind a question.
pub fn set_by_reaction(conn: &Connection, id: &str, name: &str) -> Result<()> {
    conn.execute("UPDATE decisions SET by_reaction = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

/// Records the runs an answer started, once they are up.
pub fn set_outcomes(conn: &Connection, id: &str, outcomes: &[String]) -> Result<()> {
    conn.execute(
        "UPDATE decisions SET outcomes_json = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(outcomes)?],
    )?;
    Ok(())
}

fn from_row(row: &Row) -> rusqlite::Result<Decision> {
    let options: Vec<DecisionOption> = serde_json::from_str(&row.get::<_, String>(9)?).map_err(|e| conv(9, e))?;
    let chosen: Vec<String> = row
        .get::<_, Option<String>>(14)?
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| conv(14, e))?
        .unwrap_or_default();
    let outcome_runs: Vec<String> = row
        .get::<_, Option<String>>(15)?
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| conv(15, e))?
        .unwrap_or_default();
    Ok(Decision {
        id: row.get(0)?,
        created_at: from_millis(row.get(1)?).map_err(|e| conv(1, e))?,
        updated_at: from_millis(row.get(2)?).map_err(|e| conv(2, e))?,
        workspace: PathBuf::from(row.get::<_, String>(3)?),
        run: row.get(4)?,
        flow: row.get(5)?,
        group: row.get(6)?,
        title: row.get(7)?,
        summary: row.get(8)?,
        options,
        multi: row.get(10)?,
        status: row.get::<_, String>(11)?.parse().map_err(|e| conv_str(11, e))?,
        expires_at: optional_from_millis(row.get(12)?, 12, conv)?,
        resolved_at: optional_from_millis(row.get(13)?, 13, conv)?,
        chosen,
        outcome_runs,
        note: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use rl_proto::action::Task;

    fn decision(id: &str, group: Option<&str>) -> Decision {
        Decision {
            id: id.into(),
            workspace: "/w".into(),
            run: Some("r1".into()),
            flow: "f1".into(),
            group: group.map(Into::into),
            title: format!("question {id}"),
            summary: None,
            options: vec![
                DecisionOption {
                    key: "fix".into(),
                    label: "Open a PR".into(),
                    default: false,
                    run: Some(Task::Argv(vec!["true".into()])),
                    ..Default::default()
                },
                DecisionOption::new("ignore", "Ignore"),
            ],
            multi: false,
            status: DecisionStatus::Open,
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        }
    }

    #[test]
    fn round_trips_and_answers_once() {
        let ledger = Ledger::open_in_memory().unwrap();
        let stored = ledger.call_blocking(|c| insert(c, &decision("d1", None))).unwrap();
        assert_eq!(stored.options[0].action().label(), "true");
        assert!(stored.status.is_open());

        let open = ledger.call_blocking(|c| list(c, &DecisionFilter::default())).unwrap();
        assert_eq!(open.len(), 1);

        let taken = ledger
            .call_blocking(|c| {
                resolve(
                    c,
                    "d1",
                    DecisionStatus::Taken,
                    &["fix".into()],
                    &["run9".into()],
                    Some("go on then"),
                    Timestamp::UNIX_EPOCH,
                )
            })
            .unwrap();
        assert_eq!(taken.chosen, ["fix"]);
        assert_eq!(taken.outcome_runs, ["run9"]);
        assert_eq!(taken.note.as_deref(), Some("go on then"));

        // Answering again is refused, and it is gone from the open list.
        let again = ledger.call_blocking(|c| {
            resolve(
                c,
                "d1",
                DecisionStatus::Dismissed,
                &[],
                &[],
                None,
                Timestamp::UNIX_EPOCH,
            )
        });
        assert!(again.unwrap_err().to_string().contains("already taken"));
        assert!(
            ledger
                .call_blocking(|c| list(c, &DecisionFilter::default()))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            ledger
                .call_blocking(|c| list(
                    c,
                    &DecisionFilter {
                        all: true,
                        ..Default::default()
                    }
                ))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_group_keeps_one_open_question() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .call_blocking(|c| insert(c, &decision("d1", Some("nightly/errors"))))
            .unwrap();
        let open = ledger
            .call_blocking(|c| open_in_group(c, "nightly/errors"))
            .unwrap()
            .unwrap();
        assert_eq!(open.id, "d1");

        // A second open row in the same group cannot exist…
        assert!(
            ledger
                .call_blocking(|c| insert(c, &decision("d2", Some("nightly/errors"))))
                .is_err()
        );
        // …but restating the open one is how it is asked again.
        let mut next = decision("ignored", Some("nightly/errors"));
        next.title = "3 errors, not 2".into();
        let restated = ledger
            .call_blocking(move |c| restate(c, "d1", &next, Timestamp::UNIX_EPOCH))
            .unwrap();
        assert_eq!(restated.id, "d1");
        assert_eq!(restated.title, "3 errors, not 2");

        // Once answered, the group is free again.
        ledger
            .call_blocking(|c| {
                resolve(
                    c,
                    "d1",
                    DecisionStatus::Dismissed,
                    &[],
                    &[],
                    None,
                    Timestamp::UNIX_EPOCH,
                )
            })
            .unwrap();
        assert!(
            ledger
                .call_blocking(|c| insert(c, &decision("d2", Some("nightly/errors"))))
                .is_ok()
        );
    }

    #[test]
    fn due_finds_only_expired_open_ones() {
        let ledger = Ledger::open_in_memory().unwrap();
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        let mut soon = decision("d1", None);
        soon.expires_at = Some(t("2026-01-01T00:00:00Z"));
        let mut later = decision("d2", None);
        later.expires_at = Some(t("2026-01-02T00:00:00Z"));
        let never = decision("d3", None);
        ledger
            .call_blocking(move |c| {
                insert(c, &soon)?;
                insert(c, &later)?;
                insert(c, &never)
            })
            .unwrap();
        let ids: Vec<String> = ledger
            .call_blocking(move |c| due(c, "2026-01-01T12:00:00Z".parse().unwrap()))
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(ids, ["d1"]);
    }
}
