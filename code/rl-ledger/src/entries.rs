//! What a YAML file and the ledger share, for every kind of entry the user
//! writes down: schedules today, reactions beside them, workflows later.
//!
//! The file is the interface. Two tables per kind remember what it cannot: one
//! holding every version ever loaded, by the sha256 of its spec, and one
//! holding the identity — scope and name — with its current version, the
//! version the user approved, and what happened to it. Everything here is
//! spec-agnostic: the JSON is carried as text and the caller parses it.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{optional_from_millis, to_millis};
use rl_proto::schedule::ReloadReport;
use rusqlite::{Connection, OptionalExtension, Row as SqlRow, params};
use std::path::{Path, PathBuf};

/// Which pair of tables an entry kind lives in.
#[derive(Debug, Clone, Copy)]
pub struct Tables {
    pub entries: &'static str,
    pub specs: &'static str,
    /// Whether the entry table has a `next_at` column (only schedules do).
    pub next_at: bool,
}

pub const SCHEDULES: Tables = Tables {
    entries: "schedules",
    specs: "schedule_specs",
    next_at: true,
};

pub const REACTIONS: Tables = Tables {
    entries: "reactions",
    specs: "reaction_specs",
    next_at: false,
};

/// One entry as a file says it: its name, its spec as JSON, and the hash of
/// that JSON, which is its version.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub name: String,
    pub spec_json: String,
    pub hash: String,
}

/// One entry as the ledger has it. The spec stays JSON; the caller knows what
/// shape it is.
#[derive(Debug, Clone)]
pub struct Row {
    pub scope: String,
    pub name: String,
    pub hash: String,
    pub source: PathBuf,
    pub spec_json: String,
    pub paused: bool,
    pub last_fired_at: Option<Timestamp>,
    pub last_run: Option<String>,
    pub next_at: Option<Timestamp>,
    pub removed_at: Option<Timestamp>,
    pub approved_hash: Option<String>,
}

impl Tables {
    fn cols(&self) -> String {
        let next = if self.next_at { "e.next_at" } else { "NULL" };
        format!(
            "e.scope, e.name, e.hash, e.source, p.spec_json, e.paused, e.last_fired_at, e.last_run, \
             {next}, e.removed_at, e.approved_hash"
        )
    }
    fn from(&self) -> String {
        format!("FROM {} e JOIN {} p ON p.hash = e.hash", self.entries, self.specs)
    }
    /// `SET next_at = NULL`, where there is such a column to clear.
    fn clear_next(&self) -> &'static str {
        if self.next_at { ", next_at = NULL" } else { "" }
    }
}

/// Brings the rows of one scope in line with what its file says. New names are
/// added, changed hashes replace the version (and clear `next_at` so it is
/// recomputed), names gone from the file are marked removed, names back again
/// are revived. A `trusted` scope (the user's own file) approves every version
/// as it loads; a workspace's versions wait for [`approve`].
pub fn sync(
    conn: &Connection,
    t: Tables,
    scope: &str,
    source: &Path,
    loaded: &[Loaded],
    trusted: bool,
    now: Timestamp,
) -> Result<ReloadReport> {
    let mut report = ReloadReport {
        loaded: loaded.len(),
        ..Default::default()
    };
    let src = source.to_string_lossy().into_owned();
    for l in loaded {
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {} (hash, spec_json, first_seen_at) VALUES (?1, ?2, ?3)",
                t.specs
            ),
            params![l.hash, l.spec_json, to_millis(now)],
        )?;
        let approved = trusted.then(|| l.hash.clone());
        let existing: Option<(String, Option<i64>)> = conn
            .query_row(
                &format!(
                    "SELECT hash, removed_at FROM {} WHERE scope = ?1 AND name = ?2",
                    t.entries
                ),
                params![scope, l.name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                conn.execute(
                    &format!(
                        "INSERT INTO {} (scope, name, hash, source, approved_hash) VALUES (?1, ?2, ?3, ?4, ?5)",
                        t.entries
                    ),
                    params![scope, l.name, l.hash, src, approved],
                )?;
                report.added.push(l.name.clone());
            }
            Some((hash, removed)) if hash != l.hash || removed.is_some() => {
                conn.execute(
                    &format!(
                        "UPDATE {} SET hash = ?3, source = ?4, removed_at = NULL{} \
                         , approved_hash = COALESCE(?5, approved_hash) WHERE scope = ?1 AND name = ?2",
                        t.entries,
                        t.clear_next()
                    ),
                    params![scope, l.name, l.hash, src, approved],
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
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM {} WHERE scope = ?1 AND removed_at IS NULL",
        t.entries
    ))?;
    let present: Vec<String> = stmt
        .query_map(params![scope], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for name in present {
        if !names.contains(&name) {
            conn.execute(
                &format!(
                    "UPDATE {} SET removed_at = ?3{} WHERE scope = ?1 AND name = ?2",
                    t.entries,
                    t.clear_next()
                ),
                params![scope, name, to_millis(now)],
            )?;
            report.removed.push(name);
        }
    }
    Ok(report)
}

/// Approves the current version of every listed entry in `scope` (all of them
/// when `names` is empty). Returns the names approved.
pub fn approve(conn: &Connection, t: Tables, scope: &str, names: &[String]) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for e in list_scope(conn, t, scope)? {
        if e.removed_at.is_some() || e.approved_hash.as_deref() == Some(&e.hash) {
            continue;
        }
        if !names.is_empty() && !names.contains(&e.name) {
            continue;
        }
        conn.execute(
            &format!(
                "UPDATE {} SET approved_hash = hash{} WHERE scope = ?1 AND name = ?2",
                t.entries,
                t.clear_next()
            ),
            params![scope, e.name],
        )?;
        done.push(e.name);
    }
    Ok(done)
}

/// Marks every entry of a scope removed, as when its workspace is unregistered,
/// and forgets every yes given there: a folder registered again is a stranger
/// again, and its rules ask before they run. Returns how many were marked.
pub fn remove_scope(conn: &Connection, t: Tables, scope: &str, now: Timestamp) -> Result<usize> {
    let n = conn.execute(
        &format!(
            "UPDATE {} SET removed_at = ?2{} WHERE scope = ?1 AND removed_at IS NULL",
            t.entries,
            t.clear_next()
        ),
        params![scope, to_millis(now)],
    )?;
    conn.execute(
        &format!("UPDATE {} SET approved_hash = NULL WHERE scope = ?1", t.entries),
        params![scope],
    )?;
    Ok(n)
}

pub fn get(conn: &Connection, t: Tables, scope: &str, name: &str) -> Result<Option<Row>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} {} WHERE e.scope = ?1 AND e.name = ?2", t.cols(), t.from()),
            params![scope, name],
            from_row,
        )
        .optional()?)
}

/// Every entry, removed ones only when asked, by scope then name.
pub fn list(conn: &Connection, t: Tables, include_removed: bool) -> Result<Vec<Row>> {
    let filter = if include_removed {
        ""
    } else {
        "WHERE e.removed_at IS NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {} {} {filter} ORDER BY e.scope, e.name",
        t.cols(),
        t.from()
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// One scope's entries, removed ones included.
pub fn list_scope(conn: &Connection, t: Tables, scope: &str) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} {} WHERE e.scope = ?1 ORDER BY e.name",
        t.cols(),
        t.from()
    ))?;
    let rows = stmt.query_map(params![scope], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Entries named `name` in any scope, live ones only.
pub fn find(conn: &Connection, t: Tables, name: &str) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} {} WHERE e.name = ?1 AND e.removed_at IS NULL ORDER BY e.scope",
        t.cols(),
        t.from()
    ))?;
    let rows = stmt.query_map(params![name], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn set_paused(conn: &Connection, t: Tables, scope: &str, name: &str, paused: bool) -> Result<bool> {
    let n = conn.execute(
        &format!("UPDATE {} SET paused = ?3 WHERE scope = ?1 AND name = ?2", t.entries),
        params![scope, name, paused as i32],
    )?;
    Ok(n == 1)
}

pub fn set_fired(
    conn: &Connection,
    t: Tables,
    scope: &str,
    name: &str,
    at: Timestamp,
    run: Option<&str>,
) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE {} SET last_fired_at = ?3, last_run = ?4 WHERE scope = ?1 AND name = ?2",
            t.entries
        ),
        params![scope, name, to_millis(at), run],
    )?;
    Ok(())
}

fn from_row(row: &SqlRow) -> rusqlite::Result<Row> {
    Ok(Row {
        scope: row.get(0)?,
        name: row.get(1)?,
        hash: row.get(2)?,
        source: PathBuf::from(row.get::<_, String>(3)?),
        spec_json: row.get(4)?,
        paused: row.get::<_, i32>(5)? != 0,
        last_fired_at: optional_from_millis(row.get(6)?, 6, conv)?,
        last_run: row.get(7)?,
        next_at: optional_from_millis(row.get(8)?, 8, conv)?,
        removed_at: optional_from_millis(row.get(9)?, 9, conv)?,
        approved_hash: row.get(10)?,
    })
}
