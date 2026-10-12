//! `row_order` table: a project's rows in the order you put them on the
//! panel, by the keys of the project's header and of its rows. A project
//! with nothing here sorts itself.

use anyhow::Result;
use rl_proto::methods::Order;
use rusqlite::{Connection, params};

/// Keeps `rows` as `group`'s order, first on top, in place of what was kept;
/// none lets it sort itself again. A row named twice keeps its first place.
pub fn set(conn: &mut Connection, group: &str, rows: &[String]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM row_order WHERE group_key = ?1", params![group])?;
    {
        let mut ins = tx.prepare("INSERT OR IGNORE INTO row_order (group_key, row_key, pos) VALUES (?1, ?2, ?3)")?;
        for (pos, row) in rows.iter().enumerate() {
            ins.execute(params![group, row, pos as i64])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Gives `to` the place `from` held in every order that kept it — a run
/// picked back up (`run.resume`) stays where the one it carries on stood.
pub fn pass_on(conn: &Connection, from: &str, to: &str) -> Result<()> {
    conn.execute(
        "UPDATE OR IGNORE row_order SET row_key = ?2 WHERE row_key = ?1",
        params![from, to],
    )?;
    Ok(())
}

/// Every project's order, by its header's key.
pub fn list(conn: &Connection) -> Result<Vec<Order>> {
    let mut stmt = conn.prepare("SELECT group_key, row_key FROM row_order ORDER BY group_key, pos")?;
    let mut out: Vec<Order> = Vec::new();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (group, key) = row?;
        match out.last_mut() {
            Some(o) if o.group == group => o.rows.push(key),
            _ => out.push(Order { group, rows: vec![key] }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_order_replaces_the_last_and_none_lets_the_project_sort_itself() {
        let l = Ledger::open_in_memory().unwrap();
        l.call_blocking(|c| set(c, "g:/w/api", &keys(&["r:a", "r:b", "f:nightly"])))
            .unwrap();
        l.call_blocking(|c| set(c, "g:/w/web", &keys(&["r:c"]))).unwrap();
        // The next one for a project replaces it whole: what left drops out,
        // and a row named twice keeps its first place.
        l.call_blocking(|c| set(c, "g:/w/api", &keys(&["r:b", "r:a", "r:b"])))
            .unwrap();
        let got = l.call_blocking(|c| list(c)).unwrap();
        assert_eq!(
            got,
            [
                Order {
                    group: "g:/w/api".into(),
                    rows: keys(&["r:b", "r:a"])
                },
                Order {
                    group: "g:/w/web".into(),
                    rows: keys(&["r:c"])
                },
            ]
        );
        l.call_blocking(|c| set(c, "g:/w/api", &[])).unwrap();
        let got = l.call_blocking(|c| list(c)).unwrap();
        assert_eq!(got.iter().map(|o| o.group.as_str()).collect::<Vec<_>>(), ["g:/w/web"]);
    }

    #[test]
    fn a_row_passes_its_place_on() {
        let l = Ledger::open_in_memory().unwrap();
        l.call_blocking(|c| set(c, "g:/w/api", &keys(&["r:a", "r:b", "r:c"])))
            .unwrap();
        l.call_blocking(|c| pass_on(c, "r:b", "r:d")).unwrap();
        l.call_blocking(|c| pass_on(c, "r:x", "r:y")).unwrap();
        let got = l.call_blocking(|c| list(c)).unwrap();
        assert_eq!(got[0].rows, keys(&["r:a", "r:d", "r:c"]));
    }
}
