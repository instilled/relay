//! `summary_kept`: a summary of anything — a run read deep, a job, a project,
//! all of it — over a stretch, one per (what, how deep, from when), replaced
//! when a newer one is made. A run's own brief recap stays in `summaries`.

use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::types::Summary;
use rusqlite::{Connection, OptionalExtension, params};

/// `from_at` for the whole of it.
const WHOLE: i64 = -1;

/// Keeps `s` under its target, depth and start; one already there is
/// replaced.
pub fn put(conn: &Connection, s: &Summary) -> Result<()> {
    conn.execute(
        "INSERT INTO summary_kept (target, deep, from_at, agent, by_run, at, text, source, reach, calls)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(target, deep, from_at) DO UPDATE SET agent = excluded.agent, by_run = excluded.by_run,
           at = excluded.at, text = excluded.text, source = excluded.source, reach = excluded.reach,
           calls = excluded.calls",
        params![
            s.target,
            s.deep,
            s.from.map_or(WHOLE, to_millis),
            s.agent,
            s.by,
            to_millis(s.at),
            s.text,
            s.source,
            s.reach,
            s.calls
        ],
    )?;
    Ok(())
}

/// The one kept for this target, depth and start (None: the whole of it).
pub fn get(conn: &Connection, target: &str, deep: bool, from: Option<Timestamp>) -> Result<Option<Summary>> {
    let found = conn
        .query_row(
            "SELECT agent, by_run, at, text, source, reach, calls FROM summary_kept
             WHERE target = ?1 AND deep = ?2 AND from_at = ?3",
            params![target, deep, from.map_or(WHOLE, to_millis)],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, u32>(6)?,
                ))
            },
        )
        .optional()?;
    match found {
        Some((agent, by, at, text, source, reach, calls)) => Ok(Some(Summary {
            run: String::new(),
            agent,
            by,
            at: from_millis(at)?,
            text,
            since: None,
            source,
            reach,
            stale: false,
            target: target.to_string(),
            deep,
            from,
            calls,
        })),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(target: &str, deep: bool, from: Option<Timestamp>, text: &str) -> Summary {
        Summary {
            run: String::new(),
            agent: "fakeai".into(),
            by: "r1".into(),
            at: from.unwrap_or_default(),
            text: text.into(),
            since: None,
            source: "summarised".into(),
            reach: "t:1".into(),
            stale: false,
            target: target.into(),
            deep,
            from,
            calls: 0,
        }
    }

    #[test]
    fn a_summary_is_kept_by_what_how_deep_and_from_when() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        let from = Timestamp::from_millisecond(1_000).unwrap();
        let brief = sample("all", false, Some(from), "brief");
        let deep = sample("all", true, Some(from), "deep");
        put(&c, &brief).unwrap();
        put(&c, &deep).unwrap();
        assert_eq!(get(&c, "all", false, Some(from)).unwrap().unwrap().text, "brief");
        assert_eq!(get(&c, "all", true, Some(from)).unwrap().unwrap().text, "deep");
        assert!(
            get(&c, "all", false, None).unwrap().is_none(),
            "another stretch is another summary"
        );
        put(&c, &sample("all", false, Some(from), "again")).unwrap();
        assert_eq!(
            get(&c, "all", false, Some(from)).unwrap().unwrap().text,
            "again",
            "a new one replaces"
        );
    }
}
