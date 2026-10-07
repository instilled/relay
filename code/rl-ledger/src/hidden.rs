//! `hidden` table: projects and jobs taken off the panel's list, by the key
//! of their row. A run is hidden on its own row (`runs.hidden_at`).

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::methods::Hidden;
use rusqlite::{Connection, params};

/// Hides `key` (`Some`, from then) or brings it back (`None`). Hiding what is
/// already hidden moves the time on, so what came through since goes back.
pub fn set(conn: &Connection, key: &str, at: Option<Timestamp>) -> Result<()> {
    match at {
        Some(at) => conn.execute(
            "INSERT INTO hidden (key, hidden_at) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET hidden_at = excluded.hidden_at",
            params![key, to_millis(at)],
        )?,
        None => conn.execute("DELETE FROM hidden WHERE key = ?1", params![key])?,
    };
    Ok(())
}

/// Everything hidden, by key.
pub fn list(conn: &Connection) -> Result<Vec<Hidden>> {
    let mut stmt = conn.prepare("SELECT key, hidden_at FROM hidden ORDER BY key")?;
    let rows = stmt.query_map([], |row| {
        Ok(Hidden {
            key: row.get(0)?,
            at: from_millis(row.get(1)?).map_err(|e| conv(1, e))?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn a_key_hides_until_let_go_and_hiding_again_moves_the_time() {
        let l = Ledger::open_in_memory().unwrap();
        let t1: Timestamp = "2026-09-08T10:00:00Z".parse().unwrap();
        let t2: Timestamp = "2026-09-08T11:00:00Z".parse().unwrap();
        l.call_blocking(move |c| set(c, "g:/w/api", Some(t1))).unwrap();
        l.call_blocking(move |c| set(c, "f:nightly", Some(t1))).unwrap();
        l.call_blocking(move |c| set(c, "g:/w/api", Some(t2))).unwrap();
        let got = l.call_blocking(|c| list(c)).unwrap();
        assert_eq!(
            got,
            [
                Hidden {
                    key: "f:nightly".into(),
                    at: t1
                },
                Hidden {
                    key: "g:/w/api".into(),
                    at: t2
                },
            ]
        );
        l.call_blocking(|c| set(c, "g:/w/api", None)).unwrap();
        l.call_blocking(|c| set(c, "g:/w/nothing", None)).unwrap();
        let keys: Vec<String> = l
            .call_blocking(|c| list(c))
            .unwrap()
            .into_iter()
            .map(|h| h.key)
            .collect();
        assert_eq!(keys, ["f:nightly"]);
    }
}
