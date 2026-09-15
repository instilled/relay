//! A handful of key/value rows for the daemon's own bookkeeping: where the
//! reactor got to in the event stream, and whatever else needs to survive a
//! restart without deserving a table.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// A number kept under `key`, or `None` when it was never set or is not one.
pub fn get_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(get(conn, key)?.and_then(|v| v.parse().ok()))
}

pub fn set_i64(conn: &Connection, key: &str, value: i64) -> Result<()> {
    set(conn, key, &value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn a_value_survives_being_written_twice() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.call_blocking(|c| get(c, "cursor")).unwrap(), None);
        ledger.call_blocking(|c| set_i64(c, "cursor", 7)).unwrap();
        ledger.call_blocking(|c| set_i64(c, "cursor", 9)).unwrap();
        assert_eq!(ledger.call_blocking(|c| get_i64(c, "cursor")).unwrap(), Some(9));
        ledger.call_blocking(|c| set(c, "cursor", "not a number")).unwrap();
        assert_eq!(ledger.call_blocking(|c| get_i64(c, "cursor")).unwrap(), None);
    }
}
