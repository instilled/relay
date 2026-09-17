//! `summaries` table: a recap of a run, live or ended — the agent's own, or an
//! assistant's account — one per run, replaced when a newer one is made.
//! `transcripts`: the agent's own session file for a run, once known.

use anyhow::Result;
use rl_core::time::{from_millis, to_millis};
use rl_proto::types::Summary;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

pub fn put(conn: &Connection, s: &Summary) -> Result<()> {
    conn.execute(
        "INSERT INTO summaries (run, agent, by_run, at, text, since, source, reach)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(run) DO UPDATE SET agent = excluded.agent, by_run = excluded.by_run, at = excluded.at,
           text = excluded.text, since = excluded.since, source = excluded.source, reach = excluded.reach",
        params![
            s.run,
            s.agent,
            s.by,
            to_millis(s.at),
            s.text,
            s.since,
            s.source,
            s.reach
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, run: &str) -> Result<Option<Summary>> {
    let found = conn
        .query_row(
            "SELECT run, agent, by_run, at, text, since, source, reach FROM summaries WHERE run = ?1",
            params![run],
            |r| {
                Ok((
                    (
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ),
                    (
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ),
                ))
            },
        )
        .optional()?;
    match found {
        Some(((run, agent, by, at), (text, since, source, reach))) => Ok(Some(Summary {
            run,
            agent,
            by,
            at: from_millis(at)?,
            text,
            since,
            source,
            reach,
            stale: false,
        })),
        None => Ok(None),
    }
}

/// Remembers the agent's session file for a run; a later word replaces it
/// (a resumed session writes to a new file).
pub fn set_transcript(conn: &Connection, run: &str, path: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO transcripts (run, path) VALUES (?1, ?2)
         ON CONFLICT(run) DO UPDATE SET path = excluded.path",
        params![run, path],
    )?;
    Ok(())
}

pub fn transcript(conn: &Connection, run: &str) -> Result<Option<PathBuf>> {
    Ok(conn
        .query_row("SELECT path FROM transcripts WHERE run = ?1", params![run], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::types::{Run, RunKind};

    #[test]
    fn one_per_run_the_latest_kept() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        let t: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let r = Run::new("r1", RunKind::Command, "tests", vec!["make".into()], "/w", "/w", t);
        crate::runs::insert(&c, &r).unwrap();
        assert!(get(&c, "r1").unwrap().is_none());
        let s = Summary {
            run: "r1".into(),
            agent: "claude".into(),
            by: "a1".into(),
            at: t,
            text: "It ran the tests; all passed.".into(),
            since: None,
            source: "summarised".into(),
            reach: "o:12".into(),
            stale: false,
        };
        put(&c, &s).unwrap();
        assert_eq!(get(&c, "r1").unwrap().unwrap(), s);
        let again = Summary {
            by: String::new(),
            text: "Second thoughts.".into(),
            since: Some("2h".into()),
            source: "claude recap".into(),
            reach: "t:4096".into(),
            ..s
        };
        put(&c, &again).unwrap();
        assert_eq!(get(&c, "r1").unwrap().unwrap(), again);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);

        assert!(transcript(&c, "r1").unwrap().is_none());
        set_transcript(&c, "r1", "/h/.claude/projects/-w/a.jsonl").unwrap();
        set_transcript(&c, "r1", "/h/.claude/projects/-w/b.jsonl").unwrap();
        assert_eq!(
            transcript(&c, "r1").unwrap(),
            Some(PathBuf::from("/h/.claude/projects/-w/b.jsonl"))
        );
    }
}
