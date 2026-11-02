//! Flows: the folders in `~/.relay/flows` as last loaded. The folder is the
//! interface; the table remembers what it cannot — the version said yes to,
//! a pause, when it fired and fires next, and that it is gone.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{optional_from_millis, to_millis};
use rl_proto::flow::Flow;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::PathBuf;

/// One flow as its folder says it now: the folder's hash and the checked
/// definition as JSON.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub name: String,
    pub hash: String,
    pub def_json: String,
}

/// What a [`sync`] changed, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

/// Makes the table say what is in the folder now: new and changed flows are
/// written (a change keeps the old approval, so it needs a yes), gone ones get
/// `removed_at`. Returns the names added, changed and removed. A flow back
/// after it was gone counts as added, and has its yes back if its folder is
/// the one said yes to. A change or a removal clears `next_at`, so when it
/// fires next is worked out again.
pub fn sync(conn: &Connection, loaded: &[Loaded], now: Timestamp) -> Result<Synced> {
    let mut synced = Synced::default();
    for l in loaded {
        let existing: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT hash, removed_at FROM flows WHERE name = ?1",
                params![l.name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                conn.execute(
                    "INSERT INTO flows (name, hash, def_json) VALUES (?1, ?2, ?3)",
                    params![l.name, l.hash, l.def_json],
                )?;
                synced.added.push(l.name.clone());
            }
            Some((hash, removed)) if hash != l.hash || removed.is_some() => {
                conn.execute(
                    "UPDATE flows SET hash = ?2, def_json = ?3, removed_at = NULL, next_at = NULL \
                     WHERE name = ?1",
                    params![l.name, l.hash, l.def_json],
                )?;
                if removed.is_some() {
                    synced.added.push(l.name.clone());
                } else {
                    synced.changed.push(l.name.clone());
                }
            }
            // The same folder: its definition is rewritten all the same, so a
            // newer `rld` reading it the same way leaves what it shows current.
            Some(_) => {
                conn.execute(
                    "UPDATE flows SET def_json = ?2 WHERE name = ?1",
                    params![l.name, l.def_json],
                )?;
            }
        }
    }
    let mut stmt = conn.prepare("SELECT name FROM flows WHERE removed_at IS NULL ORDER BY name")?;
    let present: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for name in present {
        if !loaded.iter().any(|l| l.name == name) {
            conn.execute(
                "UPDATE flows SET removed_at = ?2, next_at = NULL WHERE name = ?1",
                params![name, to_millis(now)],
            )?;
            synced.removed.push(name);
        }
    }
    Ok(synced)
}

const COLS: &str = "name, hash, def_json, paused, approved_hash, last_fired_at, next_at, removed_at";

/// Every flow by name, removed ones only when asked. `source` is left empty:
/// where the flows folder is is the daemon's to say.
pub fn list(conn: &Connection, include_removed: bool) -> Result<Vec<Flow>> {
    let filter = if include_removed {
        ""
    } else {
        "WHERE removed_at IS NULL"
    };
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM flows {filter} ORDER BY name"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// One flow, removed or not. `source` is left empty, as in [`list`].
pub fn get(conn: &Connection, name: &str) -> Result<Option<Flow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM flows WHERE name = ?1"),
            params![name],
            from_row,
        )
        .optional()?)
}

/// Says yes to the folder as it was shown, by its hash: given only while that is the folder as
/// last loaded. False when it is not, there is no such flow, or it is gone.
pub fn approve(conn: &Connection, name: &str, hash: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE flows SET approved_hash = hash, next_at = NULL \
         WHERE name = ?1 AND hash = ?2 AND removed_at IS NULL",
        params![name, hash],
    )?;
    Ok(n == 1)
}

/// Takes the yes back, whatever version it was to: as a flow is parked, so that coming back it
/// needs one again even with its folder as it was ([`sync`] gives a yes back otherwise).
pub fn unapprove(conn: &Connection, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE flows SET approved_hash = NULL, next_at = NULL WHERE name = ?1",
        params![name],
    )?;
    Ok(())
}

/// False when there is no such flow.
pub fn set_paused(conn: &Connection, name: &str, paused: bool) -> Result<bool> {
    let n = conn.execute(
        "UPDATE flows SET paused = ?2 WHERE name = ?1",
        params![name, paused as i32],
    )?;
    Ok(n == 1)
}

pub fn set_next(conn: &Connection, name: &str, at: Option<Timestamp>) -> Result<()> {
    conn.execute(
        "UPDATE flows SET next_at = ?2 WHERE name = ?1",
        params![name, at.map(to_millis)],
    )?;
    Ok(())
}

pub fn set_fired(conn: &Connection, name: &str, at: Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE flows SET last_fired_at = ?2 WHERE name = ?1",
        params![name, to_millis(at)],
    )?;
    Ok(())
}

fn from_row(row: &Row) -> rusqlite::Result<Flow> {
    let def: String = row.get(2)?;
    Ok(Flow {
        name: row.get(0)?,
        hash: row.get(1)?,
        def: serde_json::from_str(&def).map_err(|e| conv(2, e))?,
        source: PathBuf::new(),
        folder: None,
        paused: row.get::<_, i32>(3)? != 0,
        approved_hash: row.get(4)?,
        last_fired_at: optional_from_millis(row.get(5)?, 5, conv)?,
        next_at: optional_from_millis(row.get(6)?, 6, conv)?,
        removed_at: optional_from_millis(row.get(7)?, 7, conv)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use rl_proto::flow::FlowStatus;

    fn loaded(name: &str, hash: &str) -> Loaded {
        Loaded {
            name: name.into(),
            hash: hash.into(),
            def_json: format!(r#"{{"name":"{name}","v":"{hash}"}}"#),
        }
    }

    #[test]
    fn sync_adds_changes_and_removes() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-07T10:00:00Z".parse().unwrap();
        let s = ledger
            .call_blocking(move |c| sync(c, &[loaded("a", "h1"), loaded("b", "h1")], now))
            .unwrap();
        assert_eq!(
            (s.added.as_slice(), s.changed.len(), s.removed.len()),
            (&["a".to_string(), "b".to_string()][..], 0, 0)
        );
        let a = ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap();
        assert_eq!(a.status(), FlowStatus::NeedsApproval, "nothing is approved as it loads");
        assert_eq!(a.def["v"], "h1");

        // A yes, a pause and a next time; then the folder changes.
        let next: Timestamp = "2026-10-07T11:00:00Z".parse().unwrap();
        ledger
            .call_blocking(move |c| {
                assert!(approve(c, "a", "h1")?);
                assert!(set_paused(c, "a", true)?);
                set_next(c, "a", Some(next))?;
                set_fired(c, "a", now)
            })
            .unwrap();
        assert_eq!(
            ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap().status(),
            FlowStatus::Paused
        );
        let s = ledger
            .call_blocking(move |c| sync(c, &[loaded("a", "h2")], now))
            .unwrap();
        assert_eq!(s.changed, ["a"]);
        assert_eq!(s.removed, ["b"]);
        let a = ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap();
        assert_eq!(a.status(), FlowStatus::NeedsApproval, "a change needs a yes again");
        assert_eq!(a.approved_hash.as_deref(), Some("h1"), "the old yes is kept");
        assert_eq!(a.def["v"], "h2");
        assert!(a.paused);
        assert_eq!(a.next_at, None);
        assert_eq!(a.last_fired_at, Some(now));
        let all = ledger.call_blocking(|c| list(c, true)).unwrap();
        assert_eq!(
            all.iter().find(|f| f.name == "b").unwrap().status(),
            FlowStatus::Removed
        );
        assert_eq!(ledger.call_blocking(|c| list(c, false)).unwrap().len(), 1);
        assert!(
            !ledger.call_blocking(|c| approve(c, "b", "h1")).unwrap(),
            "a gone flow takes no yes"
        );
        // A yes is to the version shown: one gone by is not given.
        assert!(!ledger.call_blocking(|c| approve(c, "a", "h1")).unwrap());
        assert!(!ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap().approved());

        // The same folder again changes nothing; one back is added.
        let s = ledger
            .call_blocking(move |c| sync(c, &[loaded("a", "h2"), loaded("b", "h1")], now))
            .unwrap();
        assert_eq!(
            s,
            Synced {
                added: vec!["b".into()],
                ..Default::default()
            }
        );
        assert!(
            ledger
                .call_blocking(|c| get(c, "b"))
                .unwrap()
                .unwrap()
                .removed_at
                .is_none()
        );
        assert!(!ledger.call_blocking(|c| approve(c, "nope", "h1")).unwrap());
        assert!(ledger.call_blocking(|c| get(c, "nope")).unwrap().is_none());
    }

    #[test]
    fn unapprove_clears_the_yes() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-07T10:00:00Z".parse().unwrap();
        ledger
            .call_blocking(move |c| {
                sync(c, &[loaded("a", "h1")], now)?;
                assert!(approve(c, "a", "h1")?);
                unapprove(c, "a")
            })
            .unwrap();
        let a = ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap();
        assert_eq!(a.approved_hash, None);
        assert_eq!(a.status(), FlowStatus::NeedsApproval);

        // Gone and back with the very folder said yes to: still no yes.
        let s = ledger
            .call_blocking(move |c| {
                sync(c, &[], now)?;
                sync(c, &[loaded("a", "h1")], now)
            })
            .unwrap();
        assert_eq!(s.added, ["a"]);
        assert!(!ledger.call_blocking(|c| get(c, "a")).unwrap().unwrap().approved());
    }
}
