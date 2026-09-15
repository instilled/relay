//! `summaries` table: an assistant's account of an ended run, one per run,
//! replaced when asked again.

use anyhow::Result;
use rl_core::time::{from_millis, to_millis};
use rl_proto::types::Summary;
use rusqlite::{Connection, OptionalExtension, params};

pub fn put(conn: &Connection, s: &Summary) -> Result<()> {
    conn.execute(
        "INSERT INTO summaries (run, agent, by_run, at, text) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(run) DO UPDATE SET agent = excluded.agent, by_run = excluded.by_run, at = excluded.at, text = excluded.text",
        params![s.run, s.agent, s.by, to_millis(s.at), s.text],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, run: &str) -> Result<Option<Summary>> {
    let found = conn
        .query_row(
            "SELECT run, agent, by_run, at, text FROM summaries WHERE run = ?1",
            params![run],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    match found {
        Some((run, agent, by, at, text)) => Ok(Some(Summary {
            run,
            agent,
            by,
            at: from_millis(at)?,
            text,
        })),
        None => Ok(None),
    }
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
        };
        put(&c, &s).unwrap();
        assert_eq!(get(&c, "r1").unwrap().unwrap(), s);
        let again = Summary {
            by: "a2".into(),
            text: "Second thoughts.".into(),
            ..s
        };
        put(&c, &again).unwrap();
        assert_eq!(get(&c, "r1").unwrap().unwrap(), again);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
