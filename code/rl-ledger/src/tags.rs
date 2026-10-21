//! `source_tags` table: a lead source tagged with a co-worker or team name.
//! Set once per source; every lead from that source inherits the tag.

use anyhow::Result;
use rl_proto::methods::SourceTag;
use rusqlite::{Connection, params};

/// Tags `source` with `tag`, in place of the tag it had.
pub fn set(conn: &Connection, source: &str, tag: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO source_tags (source, tag) VALUES (?1, ?2) \
         ON CONFLICT(source) DO UPDATE SET tag = excluded.tag",
        params![source, tag],
    )?;
    Ok(())
}

/// Takes the tag off `source`. Clearing an untagged source does nothing.
pub fn clear(conn: &Connection, source: &str) -> Result<()> {
    conn.execute("DELETE FROM source_tags WHERE source = ?1", params![source])?;
    Ok(())
}

/// Every tagged source, by source.
pub fn list(conn: &Connection) -> Result<Vec<SourceTag>> {
    let mut stmt = conn.prepare("SELECT source, tag FROM source_tags ORDER BY source")?;
    let rows = stmt.query_map([], |r| {
        Ok(SourceTag {
            source: r.get(0)?,
            tag: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn set_then_list_roundtrips() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            set(c, "claude", "dev")?;
            set(c, "codex", "dev")?;
            Ok(())
        })
        .unwrap();
        let tags = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags.iter().any(|t| t.source == "claude" && t.tag == "dev"));
    }

    #[test]
    fn set_is_an_upsert() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            set(c, "claude", "dev")?;
            set(c, "claude", "backend")?;
            Ok(())
        })
        .unwrap();
        let tags = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag, "backend");
    }

    #[test]
    fn clear_removes_and_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            set(c, "claude", "dev")?;
            clear(c, "claude")?;
            clear(c, "claude")?;
            Ok(())
        })
        .unwrap();
        assert!(led.call_blocking(|c| list(c)).unwrap().is_empty());
    }
}
