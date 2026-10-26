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
    (9, "0009_reactions", include_str!("../migrations/0009_reactions.sql")),
    (10, "0010_archive", include_str!("../migrations/0010_archive.sql")),
    (11, "0011_summaries", include_str!("../migrations/0011_summaries.sql")),
    (12, "0012_pin", include_str!("../migrations/0012_pin.sql")),
    (13, "0013_recap", include_str!("../migrations/0013_recap.sql")),
    (14, "0014_title_auto", include_str!("../migrations/0014_title_auto.sql")),
    (15, "0015_hide", include_str!("../migrations/0015_hide.sql")),
    (16, "0016_order", include_str!("../migrations/0016_order.sql")),
    (17, "0017_recap_read", include_str!("../migrations/0017_recap_read.sql")),
    (
        18,
        "0018_relay_title",
        include_str!("../migrations/0018_relay_title.sql"),
    ),
    (
        19,
        "0019_summary_kept",
        include_str!("../migrations/0019_summary_kept.sql"),
    ),
    (20, "0020_chain", include_str!("../migrations/0020_chain.sql")),
    (21, "0021_flows", include_str!("../migrations/0021_flows.sql")),
    (22, "0022_drop_rules", include_str!("../migrations/0022_drop_rules.sql")),
    (
        23,
        "0023_group_owner",
        include_str!("../migrations/0023_group_owner.sql"),
    ),
    (24, "0024_values", include_str!("../migrations/0024_values.sql")),
    (
        25,
        "0025_session_io_files",
        include_str!("../migrations/0025_session_io_files.sql"),
    ),
    (26, "0026_leads", include_str!("../migrations/0026_leads.sql")),
    (
        27,
        "0027_source_tags",
        include_str!("../migrations/0027_source_tags.sql"),
    ),
    (28, "0028_dismissals", include_str!("../migrations/0028_dismissals.sql")),
    (29, "0029_run_facts", include_str!("../migrations/0029_run_facts.sql")),
    (
        30,
        "0030_recap_seeds",
        include_str!("../migrations/0030_recap_seeds.sql"),
    ),
];

pub fn run(conn: &mut Connection) -> Result<()> {
    run_to(conn, latest())
}

/// Applies the migrations after the ledger's own, up to `last`.
fn run_to(conn: &mut Connection, last: i32) -> Result<()> {
    let current: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (n, name, sql) in MIGRATIONS {
        if *n <= current || *n > last {
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
                "dismissals",
                "events",
                "firings",
                "flows",
                "hidden",
                "leads",
                "meta",
                "presence",
                "recap_seeds",
                "row_order",
                "run_facts",
                "runs",
                "session_io",
                "sessions",
                "source_tags",
                "step_values",
                "steps",
                "summaries",
                "summary_kept",
                "transcripts",
                "workspaces"
            ]
        );
    }
    /// What the old rules left at 21 is cleared at 22: a schedule's run with
    /// all of it, the question it raised and the run the answer started, the
    /// question that run raised and the run its answer started, a
    /// reaction's question, the job rows' keys and the reactor's cursor. A
    /// run started by hand stays, with everything of its own.
    #[test]
    fn what_the_old_rules_left_goes_and_the_rest_stays() {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        run_to(&mut c, 21).unwrap();
        c.execute_batch(
            "INSERT INTO runs (id, workspace, kind, title, argv_json, cwd, state, source, confidence, state_at, started_at, cause)
               VALUES ('sched01', '/w', 'command', 'probe', '[]', '/w', 'finished', 'relay', 'exact', 1, 1, 'schedule:nightly'),
                      ('answer1', '/w', 'command', 'act', '[]', '/w', 'finished', 'relay', 'exact', 2, 2, 'decision:dq000001'),
                      ('react01', '/w', 'command', 'tell', '[]', '/w', 'finished', 'relay', 'exact', 2, 2, 'reaction:tell'),
                      ('answer2', '/w', 'agent', 'fix', '[]', '/w', 'finished', 'relay', 'exact', 2, 2, 'decision:dq000004'),
                      ('manual1', '/w', 'shell', 'zsh', '[]', '/w', 'finished', 'relay', 'exact', 3, 3, 'manual');
             INSERT INTO sessions (run, socket, exit_file, log, pid, holder_pid, tty, cols, rows, started_at)
               VALUES ('sched01', 's', 'e', 'l', 1, 1, 0, 80, 24, 1), ('manual1', 's', 'e', 'l', 1, 1, 1, 80, 24, 3);
             INSERT INTO session_io (run, seq, direction, data, ts)
               VALUES ('sched01', 0, 'out', x'00', 1), ('manual1', 0, 'out', x'00', 3);
             INSERT INTO summaries (run, agent, by_run, at, text) VALUES ('sched01', 'a', 'x', 1, 't'), ('manual1', 'a', 'x', 3, 't');
             INSERT INTO transcripts (run, path) VALUES ('sched01', '/t'), ('manual1', '/t');
             INSERT INTO decisions (id, created_at, updated_at, workspace, run, chain, title, options_json, status, by_reaction)
               VALUES ('dq000001', 1, 1, '/w', 'sched01', 'sched01', 'act?', '[]', 'taken', NULL),
                      ('dq000002', 1, 1, '/w', NULL, 'dq000002', 'tell?', '[]', 'open', 'tell'),
                      ('dq000003', 3, 3, '/w', 'manual1', 'manual1', 'mine?', '[]', 'open', NULL),
                      ('dq000004', 2, 2, '/w', 'answer1', 'sched01', 'and then?', '[]', 'taken', NULL);
             INSERT INTO events (at, workspace, run, group_key, kind, summary, data_json)
               VALUES (1, '/w', 'sched01', '/w', 'run.started', 's', NULL),
                      (1, '/w', NULL, '/w', 'schedule.skipped', 's', NULL),
                      (1, '/w', NULL, '/w', 'decision.taken', 's', '{\"decision\":\"dq000001\"}'),
                      (1, '/w', NULL, '/w', 'decision.raised', 's', '{\"decision\":\"dq000002\"}'),
                      (2, '/w', 'answer1', '/w', 'run.started', 's', NULL),
                      (2, '/w', NULL, '/w', 'decision.taken', 's', '{\"decision\":\"dq000004\"}'),
                      (2, '/w', 'answer2', '/w', 'run.started', 's', NULL),
                      (3, '/w', 'manual1', '/w', 'run.started', 's', NULL),
                      (3, '/w', 'manual1', '/w', 'decision.raised', 's', '{\"decision\":\"dq000003\"}');
             INSERT INTO summary_kept (target, deep, agent, by_run, at, text, source, reach)
               VALUES ('r:sched01', 1, 'a', 'x', 1, 't', 's', 'r'), ('j:schedule:nightly@/w', 0, 'a', 'x', 1, 't', 's', 'r'),
                      ('r:manual1', 1, 'a', 'x', 3, 't', 's', 'r');
             INSERT INTO row_order (group_key, row_key, pos)
               VALUES ('g:/w', 'r:answer1', 0), ('g:/w', 'j:schedule:nightly@/w', 1), ('g:/w', 'r:manual1', 2);
             INSERT INTO hidden (key, hidden_at) VALUES ('j:reaction:tell@everywhere', 1), ('g:/elsewhere', 1);
             INSERT INTO meta (key, value) VALUES ('reactions.cursor', '7'), ('flows.cursor', '7');
             INSERT INTO schedule_specs (hash, spec_json, first_seen_at) VALUES ('h', '{}', 1);
             INSERT INTO schedules (scope, name, hash, source) VALUES ('global', 'nightly', 'h', '/s');
             INSERT INTO reaction_specs (hash, spec_json, first_seen_at) VALUES ('h', '{}', 1);
             INSERT INTO reactions (scope, name, hash, source) VALUES ('global', 'tell', 'h', '/r');",
        )
        .unwrap();
        run_to(&mut c, 22).unwrap();
        assert_eq!(version(&c).unwrap(), 22);
        let col = |sql: &str| -> Vec<String> {
            c.prepare(sql)
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(col("SELECT id FROM runs"), ["manual1"]);
        assert_eq!(col("SELECT run FROM sessions"), ["manual1"]);
        assert_eq!(col("SELECT run FROM session_io"), ["manual1"]);
        assert_eq!(col("SELECT run FROM summaries"), ["manual1"]);
        assert_eq!(col("SELECT run FROM transcripts"), ["manual1"]);
        assert_eq!(col("SELECT id FROM decisions"), ["dq000003"]);
        assert_eq!(
            col("SELECT kind FROM events ORDER BY id"),
            ["run.started", "decision.raised"],
            "only the manual run's"
        );
        assert_eq!(col("SELECT target FROM summary_kept"), ["r:manual1"]);
        assert_eq!(col("SELECT row_key FROM row_order"), ["r:manual1"]);
        assert_eq!(col("SELECT key FROM hidden"), ["g:/elsewhere"]);
        assert_eq!(col("SELECT key FROM meta"), ["flows.cursor"]);
        let gone = col(
            "SELECT name FROM sqlite_master WHERE name IN ('schedules', 'schedule_specs', 'reactions', 'reaction_specs')",
        );
        assert!(gone.is_empty(), "{gone:?}");
        let temp = col("SELECT name FROM sqlite_temp_master");
        assert!(temp.is_empty(), "{temp:?}");
        let columns = col("SELECT name FROM pragma_table_info('decisions')");
        assert!(!columns.iter().any(|c| c == "by_reaction"), "{columns:?}");
        let broken = col("SELECT \"table\" FROM pragma_foreign_key_check");
        assert!(broken.is_empty(), "{broken:?}");
    }

    #[test]
    fn session_io_keeps_only_where_bytes_sit() {
        let mut c = Connection::open_in_memory().unwrap();
        run(&mut c).unwrap();
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('session_io')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(cols, ["run", "seq", "direction", "pos", "len", "ts"]);
        assert!(!cols.iter().any(|c| c == "data"), "the bytes live on disk now");
    }
}
