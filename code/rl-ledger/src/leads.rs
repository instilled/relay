//! The `leads` store: user-posted leads, one row per (source, key). Relay's own
//! leads are projected at read time and never land here.

use crate::conv;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, optional_from_millis, to_millis};
use rl_proto::lead::{Band, Lead, LeadStatus};
use rusqlite::{Connection, Row, params};
use std::path::PathBuf;

const COLS: &str = "source, key, band, title, summary, scope, action, status, updated_at, expires_at";

/// Upsert a lead by its (source, key). A re-post refreshes everything but the
/// original `created_at`.
pub fn post(conn: &Connection, l: &Lead) -> Result<()> {
    conn.execute(
        "INSERT INTO leads (source, key, band, title, summary, scope, action, status, created_at, updated_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)
         ON CONFLICT(source, key) DO UPDATE SET
            band = excluded.band, title = excluded.title, summary = excluded.summary,
            scope = excluded.scope, action = excluded.action, status = excluded.status,
            updated_at = excluded.updated_at, expires_at = excluded.expires_at",
        params![
            l.source,
            l.key,
            l.band.as_str(),
            l.title,
            l.summary,
            l.scope.as_ref().map(|p| p.to_string_lossy().into_owned()),
            l.action,
            status_str(l.status),
            to_millis(l.at),
            l.expires_at.map(to_millis),
        ],
    )?;
    Ok(())
}

/// Delete a lead. Missing is not an error.
pub fn clear(conn: &Connection, source: &str, key: &str) -> Result<()> {
    conn.execute("DELETE FROM leads WHERE source = ?1 AND key = ?2", params![source, key])?;
    Ok(())
}

/// Every non-expired user lead, newest first.
pub fn list(conn: &Connection, now: Timestamp) -> Result<Vec<Lead>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM leads WHERE expires_at IS NULL OR expires_at > ?1 ORDER BY updated_at DESC"
    ))?;
    let rows = stmt.query_map(params![to_millis(now)], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Delete expired leads; return how many went.
pub fn sweep_expired(conn: &Connection, now: Timestamp) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM leads WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![to_millis(now)],
    )?)
}

fn status_str(s: LeadStatus) -> &'static str {
    match s {
        LeadStatus::Open => "open",
        LeadStatus::Resolved => "resolved",
        LeadStatus::Dismissed => "dismissed",
    }
}

fn status_from(s: &str) -> LeadStatus {
    match s {
        "resolved" => LeadStatus::Resolved,
        "dismissed" => LeadStatus::Dismissed,
        _ => LeadStatus::Open,
    }
}

fn from_row(row: &Row) -> rusqlite::Result<Lead> {
    let band: String = row.get(2)?;
    let status: String = row.get(7)?;
    Ok(Lead {
        source: row.get(0)?,
        key: row.get(1)?,
        band: band.parse().unwrap_or(Band::Happened),
        title: row.get(3)?,
        summary: row.get(4)?,
        scope: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        action: row.get(6)?,
        status: status_from(&status),
        at: from_millis(row.get(8)?).map_err(|e| conv(8, e))?,
        expires_at: optional_from_millis(row.get(9)?, 9, conv)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use jiff::SignedDuration;
    use rl_proto::lead::LeadStatus;

    fn lead(source: &str, key: &str, band: Band, at: Timestamp) -> Lead {
        Lead {
            source: source.into(),
            key: key.into(),
            band,
            title: format!("{source}/{key}"),
            summary: None,
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
        }
    }

    #[test]
    fn list_tolerates_corrupt_band() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        led.call_blocking(move |c| post(c, &lead("feedback", "triage", Band::NeedsYou, now)))
            .unwrap();
        led.call_blocking(|c| Ok(c.execute("UPDATE leads SET band = 'bogus'", [])?))
            .unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].band, Band::Happened);
    }

    #[test]
    fn post_then_list() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        led.call_blocking(move |c| post(c, &lead("feedback", "triage", Band::NeedsYou, now)))
            .unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "feedback");
        assert_eq!(out[0].band, Band::NeedsYou);
    }

    #[test]
    fn re_post_refreshes_in_place() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        led.call_blocking(move |c| post(c, &lead("gh", "pr:412", Band::Live, now)))
            .unwrap();
        let mut again = lead("gh", "pr:412", Band::NeedsYou, now);
        again.title = "review requested on #412".into();
        led.call_blocking(move |c| post(c, &again)).unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert_eq!(out.len(), 1, "same (source,key) is one lead, not two");
        assert_eq!(out[0].band, Band::NeedsYou);
        assert_eq!(out[0].title, "review requested on #412");
    }

    #[test]
    fn clear_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        // Clearing something never posted is fine.
        led.call_blocking(move |c| clear(c, "gh", "pr:999")).unwrap();
        led.call_blocking(move |c| post(c, &lead("gh", "pr:412", Band::Live, now)))
            .unwrap();
        led.call_blocking(move |c| clear(c, "gh", "pr:412")).unwrap();
        led.call_blocking(move |c| clear(c, "gh", "pr:412")).unwrap(); // again, still fine
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn expired_never_listed_and_sweeps() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        let past = now - SignedDuration::from_secs(60);
        let mut l = lead("cron", "nightly", Band::Happened, past);
        l.expires_at = Some(past); // already expired
        led.call_blocking(move |c| post(c, &l)).unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert!(out.is_empty(), "expired lead is never listed even before sweep");
        let n = led.call_blocking(move |c| sweep_expired(c, now)).unwrap();
        assert_eq!(n, 1);
    }
}
