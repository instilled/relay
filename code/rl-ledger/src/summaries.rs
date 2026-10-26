//! `summaries` table: a recap of a run, live or ended — the agent's own, or an
//! assistant's account — one per run, replaced when a newer one is made.
//! `transcripts`: the agent's own session file for a run, once known, and how
//! far into it the agent's recap has been read.

use anyhow::Result;
use rl_core::time::{from_millis, to_millis};
use rl_proto::brief::Brief;
use rl_proto::types::Summary;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::HashMap;
use std::path::PathBuf;

/// The most of a recap a row carries, in characters.
const LINE_MAX: usize = 200;

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

/// The line of each of these runs' recaps, cut for a row: one query per
/// table for the whole list. A run's written recap if it has one, else its
/// seed — the agent's own. "Nothing new" is not a recap.
pub fn lines(conn: &Connection, runs: &[String]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    // Well under what SQLite takes as parameters in one statement.
    for chunk in runs.chunks(500) {
        let marks = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
        for sql in [
            // Written ones first; a seed only fills in where none was.
            format!(
                "SELECT run, substr(text, 1, {}) FROM summaries WHERE source != 'nothing new' AND run IN ({marks})",
                LINE_MAX * 4
            ),
            format!(
                "SELECT run, substr(text, 1, {}) FROM recap_seeds WHERE run IN ({marks})",
                LINE_MAX * 4
            ),
        ] {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(chunk), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (run, text) = row?;
                if out.contains_key(&run) {
                    continue;
                }
                if let Some(line) = first_line(&text) {
                    out.insert(run, line);
                }
            }
        }
    }
    Ok(out)
}

/// A recap's line for a row: when it has labelled parts, the first of
/// `Worked on:`, `Next:`, `Open questions:` that has anything, without its
/// label (a raw label is never shown); else the first line with anything on
/// it — at most `LINE_MAX` characters.
fn first_line(text: &str) -> Option<String> {
    let brief = Brief::parse(text);
    let part = brief.worked_on.or(brief.next).or(brief.questions);
    let text = match part {
        Some(p) => p,
        None => {
            // Labelled, but every part `none`: no line, not a raw label.
            if Brief::is_labelled(text) {
                return None;
            }
            text.to_string()
        }
    };
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.chars().count() <= LINE_MAX {
        return Some(line.to_string());
    }
    let cut: String = line.chars().take(LINE_MAX - 1).collect();
    Some(format!("{}…", cut.trim_end()))
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
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            runs: 1,
        })),
        None => Ok(None),
    }
}

/// Remembers the agent's session file for a run; a later word replaces it
/// (a resumed session writes to a new file), and another file is read from
/// its start.
pub fn set_transcript(conn: &Connection, run: &str, path: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO transcripts (run, path) VALUES (?1, ?2)
         ON CONFLICT(run) DO UPDATE SET path = excluded.path,
           read_to = CASE WHEN transcripts.path = excluded.path THEN transcripts.read_to ELSE 0 END",
        params![run, path],
    )?;
    Ok(())
}

/// How far into its session file a run's recap has been read, in bytes.
pub fn read_to(conn: &Connection, run: &str) -> Result<u64> {
    let n = conn
        .query_row("SELECT read_to FROM transcripts WHERE run = ?1", params![run], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?;
    Ok(n.map_or(0, |n| n.max(0) as u64))
}

/// The next read of `path` starts at `at` — unless the run has named another
/// file since.
pub fn set_read_to(conn: &Connection, run: &str, path: &str, at: u64) -> Result<()> {
    conn.execute(
        "UPDATE transcripts SET read_to = ?3 WHERE run = ?1 AND path = ?2",
        params![run, path, at as i64],
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
    fn a_line_is_a_part_never_a_raw_label() {
        let line = |t: &str| first_line(t);
        assert_eq!(
            line("Worked on: the parser\nNext: tests").as_deref(),
            Some("the parser")
        );
        assert_eq!(
            line("Worked on: none\nOpen questions: keep the flag?\nNext: ship it").as_deref(),
            Some("ship it"),
            "next before questions"
        );
        assert_eq!(
            line("Worked on: none\nOpen questions: keep the flag?\nNext: none").as_deref(),
            Some("keep the flag?")
        );
        assert_eq!(line("Worked on: none\nOpen questions: none\nNext: none"), None);
        assert_eq!(
            line("\nSearch fixed.\nNothing else.").as_deref(),
            Some("Search fixed."),
            "no label: the first line"
        );
    }

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
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            runs: 1,
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

    fn ledger() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate::run(&mut c).unwrap();
        let t: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        for id in ["r1", "r2", "r3"] {
            let r = Run::new(id, RunKind::Agent, "fix", vec!["claude".into()], "/w", "/w", t);
            crate::runs::insert(&c, &r).unwrap();
        }
        c
    }

    fn recap(run: &str, at: &str, text: &str) -> Summary {
        Summary {
            run: run.into(),
            agent: "claude".into(),
            by: String::new(),
            at: at.parse().unwrap(),
            text: text.into(),
            since: None,
            source: "claude recap".into(),
            reach: "t:10".into(),
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            runs: 1,
        }
    }

    #[test]
    fn the_list_gets_each_recaps_first_line_in_one_go() {
        let c = ledger();
        put(
            &c,
            &recap("r1", "2026-09-10T12:05:00Z", "\n  Fixed the search.  \nThen commit."),
        )
        .unwrap();
        put(&c, &recap("r2", "2026-09-10T12:05:00Z", &"word ".repeat(100))).unwrap();
        let quiet = Summary {
            source: "nothing new".into(),
            ..recap("r3", "2026-09-10T12:05:00Z", "Nothing to go on yet.")
        };
        put(&c, &quiet).unwrap();
        let ids = ["r1", "r2", "r3", "nope"].map(String::from);
        let got = lines(&c, &ids).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got["r1"], "Fixed the search.");
        assert_eq!(got["r2"].chars().count(), LINE_MAX);
        assert!(got["r2"].ends_with('…'));
        assert!(lines(&c, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_transcript_is_read_on_from_where_it_was_left() {
        let c = ledger();
        assert_eq!(read_to(&c, "r1").unwrap(), 0);
        set_transcript(&c, "r1", "/a.jsonl").unwrap();
        set_read_to(&c, "r1", "/a.jsonl", 4096).unwrap();
        assert_eq!(read_to(&c, "r1").unwrap(), 4096);
        // The same file named again keeps its place; a stale write for
        // another file does nothing.
        set_transcript(&c, "r1", "/a.jsonl").unwrap();
        set_read_to(&c, "r1", "/old.jsonl", 9).unwrap();
        assert_eq!(read_to(&c, "r1").unwrap(), 4096);
        // Another file starts from the top.
        set_transcript(&c, "r1", "/b.jsonl").unwrap();
        assert_eq!(read_to(&c, "r1").unwrap(), 0);
    }
}
