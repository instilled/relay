//! `recap_seeds`: the agent's own recap of a run (Claude's `away_summary`,
//! Codex's last turn), read from its session file for free. One per run, the
//! newest kept. A seed is input and a fallback to show — never a written
//! recap: it lives apart from `summaries`, so it can neither replace one nor
//! be handed back as one.

use anyhow::Result;
use rl_core::time::{from_millis, to_millis};
use rl_proto::types::Summary;
use rusqlite::{Connection, OptionalExtension, params};

/// Keeps `s` unless the seed kept is as new or newer. Whether it was kept.
pub fn put_newer(conn: &Connection, s: &Summary) -> Result<bool> {
    let n = conn.execute(
        "INSERT INTO recap_seeds (run, agent, at, text, source, reach)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(run) DO UPDATE SET agent = excluded.agent, at = excluded.at,
           text = excluded.text, source = excluded.source, reach = excluded.reach
         WHERE recap_seeds.at < excluded.at",
        params![s.run, s.agent, to_millis(s.at), s.text, s.source, s.reach],
    )?;
    Ok(n > 0)
}

/// The seed kept for a run, as a summary with no assistant behind it.
pub fn get(conn: &Connection, run: &str) -> Result<Option<Summary>> {
    let found = conn
        .query_row(
            "SELECT run, agent, at, text, source, reach FROM recap_seeds WHERE run = ?1",
            params![run],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((run, agent, at, text, source, reach)) = found else {
        return Ok(None);
    };
    Ok(Some(Summary {
        run,
        agent,
        by: String::new(),
        at: from_millis(at)?,
        text,
        since: None,
        source,
        reach,
        stale: false,
        target: String::new(),
        deep: false,
        from: None,
        calls: 0,
        runs: 1,
    }))
}
