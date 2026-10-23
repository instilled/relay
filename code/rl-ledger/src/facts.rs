//! `run_facts` table: the facts collected about a run, one row per run,
//! replaced when collected again. The record is stored as JSON.

use anyhow::Result;
use rl_proto::RunFacts;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::HashMap;

pub fn set(conn: &Connection, f: &RunFacts) -> Result<()> {
    conn.execute(
        "INSERT INTO run_facts (run, facts_json, at) VALUES (?1, ?2, ?3)
         ON CONFLICT(run) DO UPDATE SET facts_json = excluded.facts_json, at = excluded.at",
        params![f.run, serde_json::to_string(f)?, f.at.to_string()],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, run: &str) -> Result<Option<RunFacts>> {
    let json: Option<String> = conn
        .query_row("SELECT facts_json FROM run_facts WHERE run = ?1", [run], |r| r.get(0))
        .optional()?;
    Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
}

/// The facts of each of these runs that has any, keyed by run.
pub fn get_many(conn: &Connection, ids: &[String]) -> Result<HashMap<String, RunFacts>> {
    let mut out = HashMap::new();
    // Well under what SQLite takes as parameters in one statement.
    for chunk in ids.chunks(500) {
        let marks = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
        let mut stmt = conn.prepare(&format!("SELECT run, facts_json FROM run_facts WHERE run IN ({marks})"))?;
        let rows = stmt.query_map(params_from_iter(chunk), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (run, json) = row?;
            out.insert(run, serde_json::from_str(&json)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    fn sample(run: &str) -> RunFacts {
        RunFacts {
            run: run.into(),
            commits: 3,
            added: 120,
            removed: 14,
            files: 5,
            top_paths: vec!["src/a.rs".into(), "src/b.rs".into()],
            branch: Some("feat/x".into()),
            pushed: true,
            active_secs: Some(900),
            tokens_in: Some(1000),
            tokens_out: Some(250),
            errors: 1,
            recap: Some("did a thing".into()),
            worked_on: Some("the thing".into()),
            questions: Some("which one?".into()),
            next: Some("ship it".into()),
            at: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        }
    }

    #[test]
    fn set_then_get_roundtrips() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            let f = sample("run:1");
            set(c, &f)?;
            assert_eq!(get(c, "run:1")?, Some(f));
            let mut g = sample("run:1");
            g.commits = 9;
            set(c, &g)?;
            assert_eq!(get(c, "run:1")?, Some(g));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn get_missing_is_none() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            assert_eq!(get(c, "nope")?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn get_many_returns_only_present() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            set(c, &sample("a"))?;
            set(c, &sample("b"))?;
            let got = get_many(c, &["a".into(), "zzz".into()])?;
            assert_eq!(got.len(), 1);
            assert_eq!(got["a"], sample("a"));
            Ok(())
        })
        .unwrap();
    }
}
