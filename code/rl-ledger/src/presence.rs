//! `presence` table: `last_seen_at` per client id.

use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rusqlite::{Connection, OptionalExtension, params};

/// Records `at` for `client` and returns the previous value, if any.
pub fn touch(conn: &Connection, client: &str, at: Timestamp) -> Result<Option<Timestamp>> {
    let previous = get(conn, client)?;
    conn.execute(
        "INSERT INTO presence (client, last_seen_at) VALUES (?1, ?2) ON CONFLICT(client) DO UPDATE SET last_seen_at = excluded.last_seen_at",
        params![client, to_millis(at)],
    )?;
    Ok(previous)
}

pub fn get(conn: &Connection, client: &str) -> Result<Option<Timestamp>> {
    let ms: Option<i64> = conn
        .query_row(
            "SELECT last_seen_at FROM presence WHERE client = ?1",
            params![client],
            |r| r.get(0),
        )
        .optional()?;
    ms.map(from_millis).transpose()
}

/// The most recent `last_seen_at` across every client — "when was I last here".
pub fn latest(conn: &Connection) -> Result<Option<Timestamp>> {
    let ms: Option<i64> = conn.query_row("SELECT MAX(last_seen_at) FROM presence", [], |r| r.get(0))?;
    ms.map(from_millis).transpose()
}
