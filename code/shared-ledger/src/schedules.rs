//! Schedules: the YAML is the interface, these tables remember what it
//! cannot. `schedule_specs` is every version ever loaded, by hash;
//! `schedules` is the identity (scope + name) with its current version and
//! runtime state.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use relay_core::time::{from_millis, to_millis};
use relay_proto::schedule::{ReloadReport, Schedule, ScheduleSpec};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::{Path, PathBuf};

const COLS: &str = "s.scope, s.name, s.hash, s.source, p.spec_json, s.paused, s.last_fired_at, s.last_run, s.next_at, \
    s.removed_at";
const FROM: &str = "FROM schedules s JOIN schedule_specs p ON p.hash = s.hash";

/// A spec as loaded from a file: name, spec, and its hash.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub name: String,
    pub spec: ScheduleSpec,
    pub hash: String,
}

/// Brings the rows of one scope in line with what its file says. New names
/// are added, changed hashes replace the version (and clear `next_at` so it
/// is recomputed), names gone from the file are marked removed, names back
/// again are revived.
pub fn sync(conn: &Connection, scope: &str, source: &Path, loaded: &[Loaded], now: Timestamp) -> Result<ReloadReport> {
    let mut report = ReloadReport {
        loaded: loaded.len(),
        ..Default::default()
    };
    let src = source.to_string_lossy().into_owned();
    for l in loaded {
        conn.execute(
            "INSERT OR IGNORE INTO schedule_specs (hash, spec_json, first_seen_at) VALUES (?1, ?2, ?3)",
            params![l.hash, serde_json::to_string(&l.spec)?, to_millis(now)],
        )?;
        let existing: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT hash, removed_at FROM schedules WHERE scope = ?1 AND name = ?2",
                params![scope, l.name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                conn.execute(
                    "INSERT INTO schedules (scope, name, hash, source) VALUES (?1, ?2, ?3, ?4)",
                    params![scope, l.name, l.hash, src],
                )?;
                report.added.push(l.name.clone());
            }
            Some((hash, removed)) if hash != l.hash || removed.is_some() => {
                conn.execute(
                    "UPDATE schedules SET hash = ?3, source = ?4, next_at = NULL, removed_at = NULL \
                     WHERE scope = ?1 AND name = ?2",
                    params![scope, l.name, l.hash, src],
                )?;
                if removed.is_some() {
                    report.added.push(l.name.clone());
                } else {
                    report.changed.push(l.name.clone());
                }
            }
            Some(_) => {}
        }
    }
    let names: Vec<String> = loaded.iter().map(|l| l.name.clone()).collect();
    let mut stmt = conn.prepare("SELECT name FROM schedules WHERE scope = ?1 AND removed_at IS NULL")?;
    let present: Vec<String> = stmt
        .query_map(params![scope], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for name in present {
        if !names.contains(&name) {
            conn.execute(
                "UPDATE schedules SET removed_at = ?3, next_at = NULL WHERE scope = ?1 AND name = ?2",
                params![scope, name, to_millis(now)],
            )?;
            report.removed.push(name);
        }
    }
    Ok(report)
}

pub fn get(conn: &Connection, scope: &str, name: &str) -> Result<Option<Schedule>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} {FROM} WHERE s.scope = ?1 AND s.name = ?2"),
            params![scope, name],
            from_row,
        )
        .optional()?)
}

/// Every schedule, removed ones only when asked, by scope then name.
pub fn list(conn: &Connection, include_removed: bool) -> Result<Vec<Schedule>> {
    let filter = if include_removed {
        ""
    } else {
        "WHERE s.removed_at IS NULL"
    };
    let mut stmt = conn.prepare(&format!("SELECT {COLS} {FROM} {filter} ORDER BY s.scope, s.name"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn set_paused(conn: &Connection, scope: &str, name: &str, paused: bool) -> Result<bool> {
    let n = conn.execute(
        "UPDATE schedules SET paused = ?3 WHERE scope = ?1 AND name = ?2",
        params![scope, name, paused as i32],
    )?;
    Ok(n == 1)
}

pub fn set_next(conn: &Connection, scope: &str, name: &str, next_at: Option<Timestamp>) -> Result<()> {
    conn.execute(
        "UPDATE schedules SET next_at = ?3 WHERE scope = ?1 AND name = ?2",
        params![scope, name, next_at.map(to_millis)],
    )?;
    Ok(())
}

pub fn set_fired(conn: &Connection, scope: &str, name: &str, at: Timestamp, run: &str) -> Result<()> {
    conn.execute(
        "UPDATE schedules SET last_fired_at = ?3, last_run = ?4 WHERE scope = ?1 AND name = ?2",
        params![scope, name, to_millis(at), run],
    )?;
    Ok(())
}

fn from_row(row: &Row) -> rusqlite::Result<Schedule> {
    let spec: String = row.get(4)?;
    let millis = |i: usize| -> rusqlite::Result<Option<Timestamp>> {
        row.get::<_, Option<i64>>(i)?
            .map(from_millis)
            .transpose()
            .map_err(|e| conv(i, e))
    };
    Ok(Schedule {
        scope: row.get(0)?,
        name: row.get(1)?,
        hash: row.get(2)?,
        source: PathBuf::from(row.get::<_, String>(3)?),
        spec: serde_json::from_str(&spec).map_err(|e| conv(4, e))?,
        paused: row.get::<_, i32>(5)? != 0,
        last_fired_at: millis(6)?,
        last_run: row.get(7)?,
        next_at: millis(8)?,
        removed_at: millis(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    fn spec(every: &str) -> ScheduleSpec {
        ScheduleSpec {
            every: Some(every.into()),
            run: vec!["true".into()],
            cwd: Some("/tmp".into()),
            ..Default::default()
        }
    }
    fn loaded(name: &str, every: &str) -> Loaded {
        Loaded {
            name: name.into(),
            spec: spec(every),
            hash: format!("h-{every}"),
        }
    }

    #[test]
    fn sync_is_a_diff_against_the_file() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        let src = Path::new("/home/u/.relay/schedules.yaml");
        let r = ledger
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "1h"), loaded("b", "2h")], now))
            .unwrap();
        assert_eq!(
            (r.loaded, r.added.len(), r.changed.len(), r.removed.len()),
            (2, 2, 0, 0)
        );

        // Pause survives an edit; the version and next_at do not.
        ledger
            .call_blocking(|c| {
                set_paused(c, "global", "a", true)?;
                set_next(c, "global", "a", Some("2026-09-09T11:00:00Z".parse().unwrap()))
            })
            .unwrap();
        let r = ledger
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "30m")], now))
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
            .call_blocking(move |c| sync(c, "global", src, &[loaded("a", "30m"), loaded("b", "2h")], now))
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
}
