//! Workspaces: folders the user registered with `rl up`. A workspace is the
//! trust boundary for everything under its `.relay/`.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::schedule::Workspace;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::{Path, PathBuf};

/// Registers `path` (idempotent). Returns the row.
pub fn register(conn: &Connection, path: &Path, name: &str, now: Timestamp) -> Result<Workspace> {
    conn.execute(
        "INSERT INTO workspaces (path, name, registered_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(path) DO UPDATE SET name = excluded.name",
        params![path.to_string_lossy(), name, to_millis(now)],
    )?;
    Ok(get(conn, path)?.expect("just registered"))
}

/// Renames a registered folder in place, keeping its `registered_at`. Returns
/// the row, or `None` when the path was never registered (nothing to rename).
pub fn rename(conn: &Connection, path: &Path, name: &str) -> Result<Option<Workspace>> {
    let n = conn.execute(
        "UPDATE workspaces SET name = ?2 WHERE path = ?1",
        params![path.to_string_lossy(), name],
    )?;
    if n == 0 {
        return Ok(None);
    }
    get(conn, path)
}

pub fn get(conn: &Connection, path: &Path) -> Result<Option<Workspace>> {
    Ok(conn
        .query_row(
            "SELECT path, name, registered_at FROM workspaces WHERE path = ?1",
            params![path.to_string_lossy()],
            from_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Workspace>> {
    let mut stmt = conn.prepare("SELECT path, name, registered_at FROM workspaces ORDER BY path")?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn remove(conn: &Connection, path: &Path) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM workspaces WHERE path = ?1",
        params![path.to_string_lossy()],
    )?;
    Ok(n == 1)
}

/// The registered workspace `path` is inside, the deepest one when nested.
pub fn containing(conn: &Connection, path: &Path) -> Result<Option<Workspace>> {
    let mut best: Option<Workspace> = None;
    for w in list(conn)? {
        if path.starts_with(&w.path)
            && best
                .as_ref()
                .is_none_or(|b| w.path.as_os_str().len() > b.path.as_os_str().len())
        {
            best = Some(w);
        }
    }
    Ok(best)
}

fn from_row(row: &Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        path: PathBuf::from(row.get::<_, String>(0)?),
        name: row.get(1)?,
        registered_at: from_millis(row.get(2)?).map_err(|e| conv(2, e))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn containing_picks_the_deepest_registered_folder() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now = Timestamp::UNIX_EPOCH;
        ledger
            .call_blocking(move |c| {
                register(c, Path::new("/w/outer"), "outer", now)?;
                register(c, Path::new("/w/outer/inner"), "inner", now)?;
                register(c, Path::new("/w/outer"), "outer2", now)
            })
            .unwrap();
        let name = |p: &'static str| {
            ledger
                .call_blocking(move |c| containing(c, Path::new(p)))
                .unwrap()
                .map(|w| w.name)
        };
        assert_eq!(name("/w/outer/inner/src"), Some("inner".into()));
        assert_eq!(name("/w/outer/other"), Some("outer2".into()));
        assert_eq!(name("/w/outerx"), None);
        assert_eq!(ledger.call_blocking(|c| list(c)).unwrap().len(), 2);
        assert!(ledger.call_blocking(|c| remove(c, Path::new("/w/outer"))).unwrap());
        assert_eq!(name("/w/outer/other"), None);
    }

    #[test]
    fn rename_changes_the_name_and_keeps_the_registration() {
        let ledger = Ledger::open_in_memory().unwrap();
        let at = Timestamp::UNIX_EPOCH;
        let was = ledger
            .call_blocking(move |c| register(c, Path::new("/w/proj"), "proj", at))
            .unwrap();
        let now = ledger
            .call_blocking(|c| rename(c, Path::new("/w/proj"), "shiny"))
            .unwrap()
            .expect("registered, so it renames");
        assert_eq!(now.name, "shiny");
        // The registration itself is untouched — same path, same moment.
        assert_eq!(now.registered_at, was.registered_at);
        // A folder nobody registered has nothing to rename.
        assert!(
            ledger
                .call_blocking(|c| rename(c, Path::new("/w/ghost"), "x"))
                .unwrap()
                .is_none()
        );
    }
}
