//! Numbered SQL migrations tracked in `PRAGMA user_version`. Each file runs
//! once, in a transaction, in order. Never edit a shipped migration; add one.

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

const MIGRATIONS: &[(i32, &str, &str)] = &[
    (1, "0001_init", include_str!("../migrations/0001_init.sql")),
    (2, "0002_sessions", include_str!("../migrations/0002_sessions.sql")),
    (3, "0003_seen", include_str!("../migrations/0003_seen.sql")),
    (4, "0004_detail", include_str!("../migrations/0004_detail.sql")),
    (5, "0005_schedules", include_str!("../migrations/0005_schedules.sql")),
    (6, "0006_workspaces", include_str!("../migrations/0006_workspaces.sql")),
    (7, "0007_decisions", include_str!("../migrations/0007_decisions.sql")),
    (
        8,
        "0008_run_grammar",
        include_str!("../migrations/0008_run_grammar.sql"),
    ),
];

pub fn run(conn: &mut Connection) -> Result<()> {
    let current: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (n, name, sql) in MIGRATIONS {
        if *n <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("applying migration {name}"))?;
        tx.pragma_update(None, "user_version", n)?;
        tx.commit()?;
        info!(migration = name, "ledger migrated");
    }
    Ok(())
}

pub fn version(conn: &Connection) -> Result<i32> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

pub fn latest() -> i32 {
    MIGRATIONS.last().map(|m| m.0).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_once_and_is_idempotent() {
        let mut c = Connection::open_in_memory().unwrap();
        run(&mut c).unwrap();
        assert_eq!(version(&c).unwrap(), latest());
        run(&mut c).unwrap();
        assert_eq!(version(&c).unwrap(), latest());
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            tables,
            [
                "decisions",
                "events",
                "presence",
                "runs",
                "schedule_specs",
                "schedules",
                "session_io",
                "sessions",
                "workspaces"
            ]
        );
    }
}
