//! Reactions: the entry tables of [`crate::entries`] read as
//! [`ReactionSpec`]s. Everything a schedule and a reaction share lives there;
//! this is only the reading of the spec.

use crate::entries::{self, Loaded, REACTIONS as T, Row};
use anyhow::Result;
use jiff::Timestamp;
use rl_proto::reaction::Reaction;
use rl_proto::schedule::ReloadReport;
use rusqlite::Connection;
use std::path::Path;

fn reaction(row: Row) -> Result<Reaction> {
    Ok(Reaction {
        spec: serde_json::from_str(&row.spec_json)?,
        scope: row.scope,
        name: row.name,
        hash: row.hash,
        source: row.source,
        paused: row.paused,
        last_fired_at: row.last_fired_at,
        last_run: row.last_run,
        removed_at: row.removed_at,
        approved_hash: row.approved_hash,
    })
}

fn all(rows: Vec<Row>) -> Result<Vec<Reaction>> {
    rows.into_iter().map(reaction).collect()
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

pub fn get(conn: &Connection, scope: &str, name: &str) -> Result<Option<Reaction>> {
    entries::get(conn, T, scope, name)?.map(reaction).transpose()
}

/// Every reaction, removed ones only when asked, by scope then name.
pub fn list(conn: &Connection, include_removed: bool) -> Result<Vec<Reaction>> {
    all(entries::list(conn, T, include_removed)?)
}

/// One scope's reactions, removed ones included.
pub fn list_scope(conn: &Connection, scope: &str) -> Result<Vec<Reaction>> {
    all(entries::list_scope(conn, T, scope)?)
}

/// Reactions named `name` in any scope, live ones only.
pub fn find(conn: &Connection, name: &str) -> Result<Vec<Reaction>> {
    all(entries::find(conn, T, name)?)
}

pub fn set_paused(conn: &Connection, scope: &str, name: &str, paused: bool) -> Result<bool> {
    entries::set_paused(conn, T, scope, name, paused)
}

/// Records that a rule fired, which is also what the throttle reads.
pub fn set_fired(conn: &Connection, scope: &str, name: &str, at: Timestamp, run: Option<&str>) -> Result<()> {
    entries::set_fired(conn, T, scope, name, at, run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use rl_proto::action::Task;
    use rl_proto::reaction::ReactionSpec;

    fn loaded(name: &str, on: &str) -> Loaded {
        let spec = ReactionSpec {
            on: vec![on.into()],
            run: Some(Task::Shell("notify".into())),
            cwd: Some("/tmp".into()),
            ..Default::default()
        };
        Loaded {
            name: name.into(),
            spec_json: serde_json::to_string(&spec).unwrap(),
            hash: format!("h-{name}-{on}"),
        }
    }

    #[test]
    fn reactions_use_the_same_tables_and_the_same_rules() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now = Timestamp::UNIX_EPOCH;
        let src = Path::new("/w/.relay/reactions.yaml");
        let r = ledger
            .call_blocking(move |c| {
                sync(
                    c,
                    "/w",
                    src,
                    &[loaded("notify", "run.failed"), loaded("watch", "run.*")],
                    false,
                    now,
                )
            })
            .unwrap();
        assert_eq!(r.added, ["notify", "watch"]);
        let live = ledger.call_blocking(|c| list(c, false)).unwrap();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].spec.on, ["run.failed"]);
        assert!(!live[0].approved(), "a workspace's reactions wait too");

        ledger.call_blocking(|c| approve(c, "/w", &[])).unwrap();
        assert!(
            ledger
                .call_blocking(|c| get(c, "/w", "notify"))
                .unwrap()
                .unwrap()
                .approved()
        );

        // Firing is remembered; it is what the throttle reads.
        let t: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        ledger
            .call_blocking(move |c| set_fired(c, "/w", "notify", t, Some("run1")))
            .unwrap();
        let n = ledger.call_blocking(|c| get(c, "/w", "notify")).unwrap().unwrap();
        assert_eq!(n.last_fired_at, Some(t));
        assert_eq!(n.last_run.as_deref(), Some("run1"));

        // A reaction with no run at all still records that it fired.
        ledger
            .call_blocking(move |c| set_fired(c, "/w", "watch", t, None))
            .unwrap();
        assert!(
            ledger
                .call_blocking(|c| get(c, "/w", "watch"))
                .unwrap()
                .unwrap()
                .last_run
                .is_none()
        );

        assert_eq!(ledger.call_blocking(|c| find(c, "notify")).unwrap().len(), 1);
        ledger.call_blocking(move |c| remove_scope(c, "/w", now)).unwrap();
        assert!(ledger.call_blocking(|c| list(c, false)).unwrap().is_empty());
    }
}
