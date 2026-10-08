//! Firings: each run of a flow, the definition it runs on, and where each of
//! its steps is — the run a step started, the question it raised,
//! the values it set.

use crate::{conv, conv_str};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, optional_from_millis, to_millis};
use rl_proto::flow::{Firing, FiringStatus, StepState, StepStatus};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::BTreeMap;

/// A firing as it starts.
#[derive(Debug, Clone)]
pub struct NewFiring<'a> {
    pub id: &'a str,
    pub flow: &'a str,
    /// The definition it runs on, whatever is edited meanwhile.
    pub def_json: &'a str,
    /// The hash of the flow's folder it began on: the version said yes to.
    pub hash: &'a str,
    /// `when`, `on` or `hand`.
    pub trigger: &'a str,
    pub event: Option<i64>,
    pub at: Timestamp,
    /// Its steps, in the order the flow writes them.
    pub steps: &'a [String],
}

/// Writes a firing, running, and its steps, all pending.
pub fn insert(conn: &Connection, f: &NewFiring) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO firings (id, flow, def_json, hash, trigger, event, status, started_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            f.id,
            f.flow,
            f.def_json,
            f.hash,
            f.trigger,
            f.event,
            FiringStatus::Running.as_str(),
            to_millis(f.at)
        ],
    )?;
    for step in f.steps {
        tx.execute(
            "INSERT INTO steps (firing, name, status) VALUES (?1, ?2, ?3)",
            params![f.id, step, StepStatus::Pending.as_str()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

const COLS: &str = "id, flow, trigger, event, status, started_at, ended_at";

pub fn get(conn: &Connection, id: &str) -> Result<Option<Firing>> {
    let found = conn
        .query_row(
            &format!("SELECT {COLS} FROM firings WHERE id = ?1"),
            params![id],
            from_row,
        )
        .optional()?;
    found.map(|f| with_steps(conn, f)).transpose()
}

/// The definition a firing runs on, as it was when it started.
pub fn def_json(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT def_json FROM firings WHERE id = ?1", params![id], |r| r.get(0))
        .optional()?)
}

/// The hash of the folder firing `id` began on; `None` for none such, or one from before it was kept.
pub fn hash(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT hash FROM firings WHERE id = ?1", params![id], |r| r.get(0))
        .optional()?
        .flatten())
}

/// When the flow's firing before `id` started.
pub fn before(conn: &Connection, id: &str) -> Result<Option<Timestamp>> {
    let at: Option<i64> = conn
        .query_row(
            "SELECT p.started_at FROM firings f JOIN firings p ON p.flow = f.flow \
             WHERE f.id = ?1 AND (p.started_at, p.rowid) < (f.started_at, f.rowid) \
             ORDER BY p.started_at DESC, p.rowid DESC LIMIT 1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    at.map(from_millis).transpose()
}

/// Each step's status, by name.
pub fn steps(conn: &Connection, id: &str) -> Result<BTreeMap<String, StepStatus>> {
    Ok(states(conn, id)?.into_iter().map(|s| (s.name, s.status)).collect())
}

/// Writes the steps' statuses and the firing's, ending it when `ended` is given.
pub fn save(
    conn: &Connection,
    id: &str,
    steps: &BTreeMap<String, StepStatus>,
    status: FiringStatus,
    ended: Option<Timestamp>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (name, s) in steps {
        tx.execute(
            "UPDATE steps SET status = ?3 WHERE firing = ?1 AND name = ?2",
            params![id, name, s.as_str()],
        )?;
    }
    tx.execute(
        "UPDATE firings SET status = ?2, ended_at = COALESCE(?3, ended_at) WHERE id = ?1",
        params![id, status.as_str(), ended.map(to_millis)],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_step_run(conn: &Connection, firing: &str, step: &str, run: &str) -> Result<()> {
    conn.execute(
        "UPDATE steps SET run = ?3 WHERE firing = ?1 AND name = ?2",
        params![firing, step, run],
    )?;
    Ok(())
}

pub fn set_step_decision(conn: &Connection, firing: &str, step: &str, decision: &str) -> Result<()> {
    conn.execute(
        "UPDATE steps SET decision = ?3 WHERE firing = ?1 AND name = ?2",
        params![firing, step, decision],
    )?;
    Ok(())
}

/// Keeps values a step set, each over any it set before under the same key.
pub fn set_values(conn: &Connection, firing: &str, step: &str, values: &[(String, String)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (key, value) in values {
        tx.execute(
            "INSERT INTO step_values (firing, step, key, value) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (firing, step, key) DO UPDATE SET value = excluded.value",
            params![firing, step, key, value],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One value a step set.
pub fn value(conn: &Connection, firing: &str, step: &str, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM step_values WHERE firing = ?1 AND step = ?2 AND key = ?3",
            params![firing, step, key],
            |r| r.get(0),
        )
        .optional()?)
}

/// Every value a step set, by key.
pub fn values(conn: &Connection, firing: &str, step: &str) -> Result<BTreeMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM step_values WHERE firing = ?1 AND step = ?2")?;
    let rows = stmt.query_map(params![firing, step], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The firing and step a run is.
pub fn by_run(conn: &Connection, run: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row("SELECT firing, name FROM steps WHERE run = ?1", params![run], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?)
}

/// The firing and step a question belongs to. A question restated by another step is that
/// step's: the one still waiting on it, else the newest.
pub fn by_decision(conn: &Connection, decision: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT firing, name FROM steps WHERE decision = ?1 \
             ORDER BY status = 'waiting' DESC, rowid DESC LIMIT 1",
            params![decision],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Which of `ids` are firings of `flow` that ended before `before`: one still open, another
/// flow's, or no firing at all is not among them.
pub fn ended_before(conn: &Connection, flow: &str, ids: &[String], before: Timestamp) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT 1 FROM firings WHERE id = ?1 AND flow = ?2 AND ended_at IS NOT NULL AND ended_at < ?3")?;
    let mut gone = Vec::new();
    for id in ids {
        if stmt.exists(params![id, flow, to_millis(before)])? {
            gone.push(id.clone());
        }
    }
    Ok(gone)
}

/// Every firing not ended, oldest first.
pub fn open(conn: &Connection) -> Result<Vec<Firing>> {
    query(conn, "WHERE ended_at IS NULL ORDER BY started_at, id", params![])
}

/// A flow's newest firing.
pub fn latest(conn: &Connection, flow: &str) -> Result<Option<Firing>> {
    Ok(list(conn, flow, 1)?.into_iter().next())
}

/// A flow's firings, newest first.
pub fn list(conn: &Connection, flow: &str, limit: u32) -> Result<Vec<Firing>> {
    query(
        conn,
        "WHERE flow = ?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        params![flow, limit],
    )
}

fn query(conn: &Connection, tail: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Firing>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM firings {tail}"))?;
    let found: Vec<Firing> = stmt.query_map(args, from_row)?.collect::<std::result::Result<_, _>>()?;
    found.into_iter().map(|f| with_steps(conn, f)).collect()
}

fn with_steps(conn: &Connection, mut f: Firing) -> Result<Firing> {
    f.steps = states(conn, &f.id)?;
    Ok(f)
}

/// A firing's steps, in the order they were written in.
fn states(conn: &Connection, id: &str) -> Result<Vec<StepState>> {
    let mut stmt = conn.prepare("SELECT name, status, run, decision FROM steps WHERE firing = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![id], |r| {
        let status: String = r.get(1)?;
        Ok(StepState {
            name: r.get(0)?,
            status: status.parse().map_err(|e| conv_str(1, e))?,
            run: r.get(2)?,
            decision: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn from_row(row: &Row) -> rusqlite::Result<Firing> {
    let status: String = row.get(4)?;
    Ok(Firing {
        id: row.get(0)?,
        flow: row.get(1)?,
        trigger: row.get(2)?,
        event: row.get(3)?,
        status: status.parse().map_err(|e| conv_str(4, e))?,
        started_at: from_millis(row.get(5)?).map_err(|e| conv(5, e))?,
        ended_at: optional_from_millis(row.get(6)?, 6, conv)?,
        steps: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn fire(ledger: &Ledger, id: &'static str, when: &'static str) {
        ledger
            .call_blocking(move |c| {
                let steps = ["probe", "triage", "review"].map(String::from);
                insert(
                    c,
                    &NewFiring {
                        id,
                        flow: "prod-logs",
                        def_json: "{}",
                        hash: "h",
                        trigger: "when",
                        event: None,
                        at: at(when),
                        steps: &steps,
                    },
                )
            })
            .unwrap();
    }

    #[test]
    fn a_firing_starts_with_every_step_pending_and_is_saved_as_it_moves() {
        let ledger = Ledger::open_in_memory().unwrap();
        fire(&ledger, "f1", "2026-10-07T10:00:00Z");
        let f = ledger.call_blocking(|c| get(c, "f1")).unwrap().unwrap();
        assert_eq!(f.status, FiringStatus::Running);
        assert_eq!(f.ended_at, None);
        assert_eq!(
            f.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["probe", "triage", "review"],
            "in the order the flow writes them"
        );
        let s = ledger.call_blocking(|c| steps(c, "f1")).unwrap();
        assert!(s.values().all(|s| *s == StepStatus::Pending), "{s:?}");

        let mut s = s;
        s.insert("probe".into(), StepStatus::Running);
        let moved = s.clone();
        ledger
            .call_blocking(move |c| {
                save(c, "f1", &moved, FiringStatus::Running, None)?;
                set_step_run(c, "f1", "probe", "r1")
            })
            .unwrap();
        assert_eq!(ledger.call_blocking(|c| steps(c, "f1")).unwrap(), s);
        assert_eq!(
            ledger.call_blocking(|c| by_run(c, "r1")).unwrap(),
            Some(("f1".to_string(), "probe".to_string()))
        );
        assert_eq!(ledger.call_blocking(|c| by_run(c, "r2")).unwrap(), None);
        assert_eq!(ledger.call_blocking(|c| open(c)).unwrap().len(), 1);

        s.insert("probe".into(), StepStatus::Done);
        s.insert("triage".into(), StepStatus::Skipped);
        s.insert("review".into(), StepStatus::Skipped);
        let ended = s.clone();
        let end = at("2026-10-07T10:01:00Z");
        ledger
            .call_blocking(move |c| {
                set_step_decision(c, "f1", "review", "d1")?;
                save(c, "f1", &ended, FiringStatus::Done, Some(end))
            })
            .unwrap();
        let f = ledger.call_blocking(|c| get(c, "f1")).unwrap().unwrap();
        assert_eq!((f.status, f.ended_at), (FiringStatus::Done, Some(end)));
        assert_eq!(f.steps[0].run.as_deref(), Some("r1"));
        assert_eq!(f.steps[2].status, StepStatus::Skipped);
        assert_eq!(
            ledger.call_blocking(|c| by_decision(c, "d1")).unwrap(),
            Some(("f1".to_string(), "review".to_string()))
        );
        assert!(ledger.call_blocking(|c| open(c)).unwrap().is_empty());
    }

    #[test]
    fn a_restated_question_belongs_to_the_step_still_waiting_on_it() {
        let ledger = Ledger::open_in_memory().unwrap();
        fire(&ledger, "old", "2026-10-07T10:00:00Z");
        fire(&ledger, "new", "2026-10-07T09:00:00Z");
        let waiting = |step: &str| BTreeMap::from([(step.to_string(), StepStatus::Waiting)]);
        let (old, new) = (waiting("review"), waiting("review"));
        ledger
            .call_blocking(move |c| {
                save(c, "old", &old, FiringStatus::Waiting, None)?;
                set_step_decision(c, "old", "review", "d1")?;
                save(c, "new", &new, FiringStatus::Waiting, None)?;
                set_step_decision(c, "new", "review", "d1")
            })
            .unwrap();
        let owner = || ledger.call_blocking(|c| by_decision(c, "d1")).unwrap().unwrap().0;
        assert_eq!(owner(), "new", "both waiting: the newest written");
        let skipped = BTreeMap::from([("review".to_string(), StepStatus::Skipped)]);
        ledger
            .call_blocking(move |c| save(c, "new", &skipped, FiringStatus::Done, None))
            .unwrap();
        assert_eq!(owner(), "old", "the one still waiting");
    }

    #[test]
    fn a_flows_firings_come_newest_first() {
        let ledger = Ledger::open_in_memory().unwrap();
        fire(&ledger, "old", "2026-10-07T10:00:00Z");
        fire(&ledger, "new", "2026-10-07T11:00:00Z");
        let ids = |limit| {
            ledger
                .call_blocking(move |c| list(c, "prod-logs", limit))
                .unwrap()
                .into_iter()
                .map(|f| f.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(10), ["new", "old"]);
        assert_eq!(ids(1), ["new"]);
        assert_eq!(
            ledger.call_blocking(|c| latest(c, "prod-logs")).unwrap().unwrap().id,
            "new"
        );
        assert!(ledger.call_blocking(|c| latest(c, "other")).unwrap().is_none());
        assert_eq!(ledger.call_blocking(|c| open(c)).unwrap().len(), 2);
        assert_eq!(
            ledger.call_blocking(|c| before(c, "new")).unwrap(),
            Some(at("2026-10-07T10:00:00Z"))
        );
        assert_eq!(ledger.call_blocking(|c| before(c, "old")).unwrap(), None);
        assert_eq!(
            ledger.call_blocking(|c| def_json(c, "old")).unwrap().as_deref(),
            Some("{}")
        );
        assert_eq!(ledger.call_blocking(|c| def_json(c, "nope")).unwrap(), None);
    }

    #[test]
    fn a_steps_values_are_kept_the_last_said_winning() {
        let ledger = Ledger::open_in_memory().unwrap();
        fire(&ledger, "f1", "2026-10-07T10:00:00Z");
        ledger
            .call_blocking(|c| {
                let first = [
                    ("found".to_string(), "2".to_string()),
                    ("signature".to_string(), "abc".to_string()),
                ];
                set_values(c, "f1", "probe", &first)?;
                set_values(c, "f1", "probe", &[("found".to_string(), "3".to_string())])
            })
            .unwrap();
        let one =
            |step: &'static str, key: &'static str| ledger.call_blocking(move |c| value(c, "f1", step, key)).unwrap();
        assert_eq!(one("probe", "found").as_deref(), Some("3"));
        assert_eq!(one("probe", "signature").as_deref(), Some("abc"));
        assert_eq!(one("triage", "found"), None, "a value is its own step's");
        let all = ledger.call_blocking(|c| values(c, "f1", "probe")).unwrap();
        assert_eq!(
            all,
            BTreeMap::from([
                ("found".to_string(), "3".to_string()),
                ("signature".to_string(), "abc".to_string())
            ])
        );
        assert!(ledger.call_blocking(|c| values(c, "f1", "triage")).unwrap().is_empty());
    }

    #[test]
    fn only_a_flows_firings_that_ended_before_are_said_to_have() {
        let ledger = Ledger::open_in_memory().unwrap();
        fire(&ledger, "old", "2026-10-01T10:00:00Z");
        fire(&ledger, "open", "2026-10-01T11:00:00Z");
        fire(&ledger, "new", "2026-10-07T10:00:00Z");
        ledger
            .call_blocking(|c| {
                save(
                    c,
                    "old",
                    &BTreeMap::new(),
                    FiringStatus::Done,
                    Some(at("2026-10-01T10:05:00Z")),
                )?;
                save(
                    c,
                    "new",
                    &BTreeMap::new(),
                    FiringStatus::Done,
                    Some(at("2026-10-07T10:05:00Z")),
                )
            })
            .unwrap();
        let ids: Vec<String> = ["old", "open", "new", "seen", "notes"].map(String::from).into();
        let cut = at("2026-10-05T00:00:00Z");
        let i = ids.clone();
        assert_eq!(
            ledger
                .call_blocking(move |c| ended_before(c, "prod-logs", &i, cut))
                .unwrap(),
            ["old"]
        );
        assert!(
            ledger
                .call_blocking(move |c| ended_before(c, "other", &ids, cut))
                .unwrap()
                .is_empty(),
            "another flow's firing is not this one's"
        );
    }
}
