//! `dismissals` table: leads the user dismissed, by (source, key). An overlay
//! the report is filtered against, so built-in leads can be hidden too.

use anyhow::Result;
use rl_core::time::to_millis;
use rl_proto::methods::Dismissal;
use rusqlite::{Connection, params};

/// Records a dismissal at `at`. Dismissing again just refreshes the time.
pub fn add(conn: &Connection, source: &str, key: &str, at: jiff::Timestamp) -> Result<()> {
    conn.execute(
        "INSERT INTO dismissals (source, key, at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(source, key) DO UPDATE SET at = excluded.at",
        params![source, key, to_millis(at)],
    )?;
    Ok(())
}

/// Un-dismisses a lead. Removing one that isn't dismissed does nothing.
pub fn remove(conn: &Connection, source: &str, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM dismissals WHERE source = ?1 AND key = ?2",
        params![source, key],
    )?;
    Ok(())
}

/// Every dismissed lead.
pub fn list(conn: &Connection) -> Result<Vec<Dismissal>> {
    let mut stmt = conn.prepare("SELECT source, key FROM dismissals ORDER BY source, key")?;
    let rows = stmt.query_map([], |r| {
        Ok(Dismissal {
            source: r.get(0)?,
            key: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn add_then_list_roundtrips() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            add(c, "claude", "run:1", jiff::Timestamp::now())?;
            add(c, "feedback", "triage", jiff::Timestamp::now())?;
            Ok(())
        })
        .unwrap();
        let d = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(d.len(), 2);
        assert!(d.iter().any(|d| d.source == "claude" && d.key == "run:1"));
    }

    #[test]
    fn add_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            add(c, "claude", "run:1", jiff::Timestamp::now())?;
            add(c, "claude", "run:1", jiff::Timestamp::now())?;
            Ok(())
        })
        .unwrap();
        assert_eq!(led.call_blocking(|c| list(c)).unwrap().len(), 1);
    }

    #[test]
    fn remove_deletes_and_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            add(c, "claude", "run:1", jiff::Timestamp::now())?;
            remove(c, "claude", "run:1")?;
            remove(c, "claude", "run:1")?;
            Ok(())
        })
        .unwrap();
        assert!(led.call_blocking(|c| list(c)).unwrap().is_empty());
    }
}
