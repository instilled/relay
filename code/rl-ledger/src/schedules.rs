//! Schedules: the entry tables of [`crate::entries`] read as [`ScheduleSpec`]s,
//! plus the one thing only a schedule has — when its next firing is due.

use crate::entries::{self, Loaded, Row, SCHEDULES as T};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::to_millis;
use rl_proto::schedule::{ReloadReport, Schedule};
use rusqlite::{Connection, params};
use std::path::Path;

fn schedule(row: Row) -> Result<Schedule> {
    Ok(Schedule {
        spec: serde_json::from_str(&row.spec_json)?,
        scope: row.scope,
        name: row.name,
        hash: row.hash,
        source: row.source,
        paused: row.paused,
        last_fired_at: row.last_fired_at,
        last_run: row.last_run,
        next_at: row.next_at,
        removed_at: row.removed_at,
        approved_hash: row.approved_hash,
    })
}

fn all(rows: Vec<Row>) -> Result<Vec<Schedule>> {
    rows.into_iter().map(schedule).collect()
}

pub fn sync(
    conn: &Connection,
    scope: &str,
    source: &Path,
    loaded: &[Loaded],
    trusted: bool,
    now: Timestamp,
) -> Result<ReloadReport> {
    entries::sync(conn, T, scope, source, loaded, trusted, now)
}

pub fn approve(conn: &Connection, scope: &str, names: &[String]) -> Result<Vec<String>> {
    entries::approve(conn, T, scope, names)
}

pub fn remove_scope(conn: &Connection, scope: &str, now: Timestamp) -> Result<usize> {
    entries::remove_scope(conn, T, scope, now)
}

pub fn get(conn: &Connection, scope: &str, name: &str) -> Result<Option<Schedule>> {
    entries::get(conn, T, scope, name)?.map(schedule).transpose()
}

/// Every schedule, removed ones only when asked, by scope then name.
pub fn list(conn: &Connection, include_removed: bool) -> Result<Vec<Schedule>> {
    all(entries::list(conn, T, include_removed)?)
}

/// One scope's schedules, removed ones included.
pub fn list_scope(conn: &Connection, scope: &str) -> Result<Vec<Schedule>> {
    all(entries::list_scope(conn, T, scope)?)
}

/// Schedules named `name` in any scope, live ones only.
pub fn find(conn: &Connection, name: &str) -> Result<Vec<Schedule>> {
    all(entries::find(conn, T, name)?)
}

pub fn set_paused(conn: &Connection, scope: &str, name: &str, paused: bool) -> Result<bool> {
    entries::set_paused(conn, T, scope, name, paused)
}

pub fn set_next(conn: &Connection, scope: &str, name: &str, next_at: Option<Timestamp>) -> Result<()> {
    conn.execute(
        "UPDATE schedules SET next_at = ?3 WHERE scope = ?1 AND name = ?2",
        params![scope, name, next_at.map(to_millis)],
    )?;
    Ok(())
}

pub fn set_fired(conn: &Connection, scope: &str, name: &str, at: Timestamp, run: &str) -> Result<()> {
    entries::set_fired(conn, T, scope, name, at, Some(run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use rl_proto::action::Task;
    use rl_proto::schedule::ScheduleSpec;

    fn spec(every: &str) -> ScheduleSpec {
        ScheduleSpec {
            every: Some(every.into()),
            run: Some(Task::Argv(vec!["true".into()])),
            cwd: Some("/tmp".into()),
            ..Default::default()
        }
    }
    fn loaded(name: &str, every: &str) -> Loaded {
        Loaded {
            name: name.into(),
            spec_json: serde_json::to_string(&spec(every)).unwrap(),
            hash: format!("h-{every}"),
        }
    }

    #[test]
    fn sync_is_a_diff_against_the_file() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        let src = Path::new("/home/u/.relay/schedules.yaml");
        let r = ledger
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "1h"), loaded("b", "2h")], true, now))
            .unwrap();
        assert_eq!(
            (r.loaded, r.added.len(), r.changed.len(), r.removed.len()),
            (2, 2, 0, 0)
        );
        assert!(
            ledger
                .call_blocking(|c| get(c, "global", "a"))
                .unwrap()
                .unwrap()
                .approved()
        );

        // Pause survives an edit; the version and next_at do not.
        ledger
            .call_blocking(|c| {
                set_paused(c, "global", "a", true)?;
                set_next(c, "global", "a", Some("2026-09-09T11:00:00Z".parse().unwrap()))
            })
            .unwrap();
        let r = ledger
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "30m")], true, now))
            .unwrap();
        assert_eq!(r.changed, ["a"]);
        assert_eq!(r.removed, ["b"]);
        let all = ledger.call_blocking(|c| list(c, true)).unwrap();
        let a = all.iter().find(|s| s.name == "a").unwrap();
        assert!(a.paused);
        assert_eq!(a.hash, "h-30m");
        assert_eq!(a.next_at, None);
        assert!(all.iter().find(|s| s.name == "b").unwrap().removed_at.is_some());
        assert_eq!(ledger.call_blocking(|c| list(c, false)).unwrap().len(), 1);

        // Back in the file: revived, counted as added.
        let r = ledger
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "30m"), loaded("b", "2h")], true, now))
            .unwrap();
        assert_eq!(r.added, ["b"]);
        assert!(
            ledger
                .call_blocking(|c| get(c, "global", "b"))
                .unwrap()
                .unwrap()
                .removed_at
                .is_none()
        );
    }

    #[test]
    fn a_workspace_version_waits_for_approval() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        let src = Path::new("/w/.relay/schedules.yaml");
        ledger
            .call_blocking(move |c| sync(c, "/w", src, &[loaded("a", "1h"), loaded("b", "1h")], false, now))
            .unwrap();
        let a = ledger.call_blocking(|c| get(c, "/w", "a")).unwrap().unwrap();
        assert!(!a.approved());
        let done = ledger.call_blocking(|c| approve(c, "/w", &["a".to_string()])).unwrap();
        assert_eq!(done, ["a"]);
        assert!(ledger.call_blocking(|c| get(c, "/w", "a")).unwrap().unwrap().approved());
        assert!(!ledger.call_blocking(|c| get(c, "/w", "b")).unwrap().unwrap().approved());

        // An edit needs approving again; the old approval is remembered.
        ledger
            .call_blocking(move |c| sync(c, "/w", src, &[loaded("a", "2h"), loaded("b", "1h")], false, now))
            .unwrap();
        let a = ledger.call_blocking(|c| get(c, "/w", "a")).unwrap().unwrap();
        assert!(!a.approved());
        assert_eq!(a.approved_hash.as_deref(), Some("h-1h"));
        let done = ledger.call_blocking(|c| approve(c, "/w", &[])).unwrap();
        assert_eq!(done, ["a", "b"]);
        assert_eq!(ledger.call_blocking(|c| find(c, "a")).unwrap().len(), 1);
        assert_eq!(ledger.call_blocking(move |c| remove_scope(c, "/w", now)).unwrap(), 2);
        assert!(ledger.call_blocking(|c| find(c, "a")).unwrap().is_empty());
    }
}
