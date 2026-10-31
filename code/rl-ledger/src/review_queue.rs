//! `review_queue` table: one pointer per session, (source, ref), to where it
//! last came to rest. A reviewed row stays so the next stretch begins where
//! the last was seen.

use anyhow::Result;
use jiff::Timestamp;
use rl_core::time::{from_millis, to_millis};
use rl_proto::queue::{Hint, How, QueueItem, RestKind};
use rusqlite::{Connection, OptionalExtension, Row, params};

const COLS: &str = "source, ref, kind, how, hints, at, rests, from_at, from_head";

/// A session came to rest at `at`. `start` and `start_head` say where its
/// stretch began; they count only for a session not in the queue.
#[allow(clippy::too_many_arguments)]
pub fn rest(
    conn: &Connection,
    source: &str,
    reference: &str,
    kind: RestKind,
    how: How,
    hints: &[Hint],
    at: Timestamp,
    start: Timestamp,
    start_head: Option<&str>,
) -> Result<()> {
    let row: Option<(String, bool)> = conn
        .query_row(
            "SELECT hints, reviewed FROM review_queue WHERE source = ?1 AND ref = ?2",
            params![source, reference],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    match row {
        None => {
            conn.execute(
                &format!("INSERT INTO review_queue ({COLS}, reviewed) VALUES (?1,?2,?3,?4,?5,?6,1,?7,?8,0)"),
                params![
                    source,
                    reference,
                    kind.as_str(),
                    how.as_str(),
                    hints_json(hints)?,
                    to_millis(at),
                    to_millis(start),
                    start_head,
                ],
            )?;
        }
        Some((_, true)) => {
            conn.execute(
                "UPDATE review_queue SET kind = ?3, how = ?4, hints = ?5, at = ?6, rests = 1, reviewed = 0 \
                 WHERE source = ?1 AND ref = ?2",
                params![
                    source,
                    reference,
                    kind.as_str(),
                    how.as_str(),
                    hints_json(hints)?,
                    to_millis(at)
                ],
            )?;
        }
        Some((old, false)) => {
            let mut all = hints.to_vec();
            for h in parse_hints(&old)? {
                if matches!(h, Hint::Stopped | Hint::Failed) && !all.contains(&h) {
                    all.push(h);
                }
            }
            conn.execute(
                "UPDATE review_queue SET kind = ?3, how = ?4, hints = ?5, at = ?6, rests = rests + 1 \
                 WHERE source = ?1 AND ref = ?2",
                params![
                    source,
                    reference,
                    kind.as_str(),
                    how.as_str(),
                    hints_json(&all)?,
                    to_millis(at)
                ],
            )?;
        }
    }
    Ok(())
}

/// A session ended (`Stopped` or `Failed`). A queued one gains the hint; one
/// not queued, or already reviewed, is queued as finished — by the reply it
/// had, else by the screen — at the end.
#[allow(clippy::too_many_arguments)]
pub fn end(
    conn: &Connection,
    source: &str,
    reference: &str,
    hint: Hint,
    at: Timestamp,
    start: Timestamp,
    start_head: Option<&str>,
) -> Result<()> {
    let queued: Option<String> = conn
        .query_row(
            "SELECT hints FROM review_queue WHERE source = ?1 AND ref = ?2 AND reviewed = 0",
            params![source, reference],
            |r| r.get(0),
        )
        .optional()?;
    match queued {
        Some(old) => {
            let mut hints = parse_hints(&old)?;
            if !hints.contains(&hint) {
                hints.push(hint);
            }
            conn.execute(
                "UPDATE review_queue SET hints = ?3 WHERE source = ?1 AND ref = ?2",
                params![source, reference, hints_json(&hints)?],
            )?;
        }
        None => {
            let how = match last_rest(conn, source, reference)? {
                Some((_, How::Reply)) => How::Reply,
                _ => How::Screen,
            };
            rest(
                conn,
                source,
                reference,
                RestKind::Finished,
                how,
                &[hint],
                at,
                start,
                start_head,
            )?;
        }
    }
    Ok(())
}

/// The user looked: the item leaves the queue and the next stretch begins at
/// `at`, from `head`. False when nothing was queued.
pub fn review(conn: &Connection, source: &str, reference: &str, at: Timestamp, head: Option<&str>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE review_queue SET reviewed = 1, from_at = ?3, from_head = ?4, hints = '[]' \
         WHERE source = ?1 AND ref = ?2 AND reviewed = 0",
        params![source, reference, to_millis(at), head],
    )?;
    Ok(n > 0)
}

/// Everything waiting, latest rest first.
pub fn list(conn: &Connection) -> Result<Vec<QueueItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM review_queue WHERE reviewed = 0 ORDER BY at DESC, source, ref"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One waiting item; `None` when the session is not queued.
pub fn get(conn: &Connection, source: &str, reference: &str) -> Result<Option<QueueItem>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM review_queue WHERE source = ?1 AND ref = ?2 AND reviewed = 0"),
            params![source, reference],
            from_row,
        )
        .optional()?)
}

/// Where the session's unreviewed stretch begins — queued or not: its time,
/// and the HEAD then. `None` when it never came to rest.
pub fn from(conn: &Connection, source: &str, reference: &str) -> Result<Option<(Timestamp, Option<String>)>> {
    let row: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT from_at, from_head FROM review_queue WHERE source = ?1 AND ref = ?2",
            params![source, reference],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(at, head)| Ok((from_millis(at)?, head))).transpose()
}

/// When, and how, the session last came to rest — reviewed or not. The time
/// is that of the last rest, never of the review: a review moves `from`, not
/// `at`.
pub fn last_rest(conn: &Connection, source: &str, reference: &str) -> Result<Option<(Timestamp, How)>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT at, how FROM review_queue WHERE source = ?1 AND ref = ?2",
            params![source, reference],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(at, how)| Ok((from_millis(at)?, how.parse::<How>().map_err(anyhow::Error::msg)?)))
        .transpose()
}

fn hints_json(hints: &[Hint]) -> Result<String> {
    Ok(serde_json::to_string(hints)?)
}

fn parse_hints(s: &str) -> Result<Vec<Hint>> {
    Ok(serde_json::from_str(s)?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<QueueItem> {
    let kind: String = row.get(2)?;
    let how: String = row.get(3)?;
    let hints: String = row.get(4)?;
    Ok(QueueItem {
        source: row.get(0)?,
        reference: row.get(1)?,
        kind: kind.parse().map_err(|e| crate::conv_str(2, e))?,
        how: how.parse().map_err(|e| crate::conv_str(3, e))?,
        hints: serde_json::from_str(&hints).map_err(|e| crate::conv(4, e))?,
        at: from_millis(row.get(5)?).map_err(|e| crate::conv(5, e))?,
        rests: row.get(6)?,
        from: from_millis(row.get(7)?).map_err(|e| crate::conv(7, e))?,
        from_head: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    fn t(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    #[test]
    fn a_first_rest_queues_from_the_start() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            rest(
                c,
                "run",
                "r1",
                RestKind::Asks,
                How::Reply,
                &[Hint::Question],
                t(2),
                t(0),
                Some("abc"),
            )
        })
        .unwrap();
        let items = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!((i.source.as_str(), i.reference.as_str()), ("run", "r1"));
        assert_eq!((i.kind, i.how, i.at), (RestKind::Asks, How::Reply, t(2)));
        assert_eq!(i.hints, [Hint::Question]);
        assert_eq!(i.rests, 1);
        assert_eq!(i.from, t(0));
        assert_eq!(i.from_head.as_deref(), Some("abc"));
    }

    #[test]
    fn resting_again_updates_the_one_item() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            rest(
                c,
                "run",
                "r1",
                RestKind::Asks,
                How::Reply,
                &[Hint::Question],
                t(2),
                t(0),
                Some("abc"),
            )?;
            rest(
                c,
                "run",
                "r1",
                RestKind::Finished,
                How::Screen,
                &[],
                t(4),
                t(3),
                Some("zzz"),
            )
        })
        .unwrap();
        let items = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!(i.rests, 2);
        assert_eq!((i.kind, i.how, i.at), (RestKind::Finished, How::Screen, t(4)));
        assert!(i.hints.is_empty());
        assert_eq!((i.from, i.from_head.as_deref()), (t(0), Some("abc")));
    }

    #[test]
    fn stopped_survives_a_later_rest() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            rest(
                c,
                "run",
                "r1",
                RestKind::Asks,
                How::Reply,
                &[Hint::Question],
                t(2),
                t(0),
                None,
            )?;
            end(c, "run", "r1", Hint::Stopped, t(3), t(0), None)?;
            rest(c, "run", "r1", RestKind::Finished, How::Reply, &[], t(4), t(0), None)
        })
        .unwrap();
        let i = led.call_blocking(|c| get(c, "run", "r1")).unwrap().unwrap();
        assert_eq!(i.hints, [Hint::Stopped]);
    }

    #[test]
    fn review_clears_and_keeps_from() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            rest(
                c,
                "run",
                "r1",
                RestKind::Asks,
                How::Reply,
                &[Hint::Question],
                t(2),
                t(0),
                Some("abc"),
            )?;
            end(c, "run", "r1", Hint::Failed, t(3), t(0), None)
        })
        .unwrap();
        assert!(
            led.call_blocking(|c| review(c, "run", "r1", t(5), Some("def")))
                .unwrap()
        );
        assert!(led.call_blocking(|c| list(c)).unwrap().is_empty());
        assert!(led.call_blocking(|c| get(c, "run", "r1")).unwrap().is_none());
        assert_eq!(
            led.call_blocking(|c| from(c, "run", "r1")).unwrap(),
            Some((t(5), Some("def".to_string()))),
            "reviewed, it still says where the next stretch begins"
        );
        assert_eq!(led.call_blocking(|c| from(c, "run", "r2")).unwrap(), None);
        led.call_blocking(|c| {
            rest(
                c,
                "run",
                "r1",
                RestKind::Finished,
                How::Screen,
                &[Hint::Unsure],
                t(8),
                t(1),
                None,
            )
        })
        .unwrap();
        let i = led.call_blocking(|c| get(c, "run", "r1")).unwrap().unwrap();
        assert_eq!(i.rests, 1);
        assert_eq!((i.from, i.from_head.as_deref()), (t(5), Some("def")));
        assert_eq!(i.hints, [Hint::Unsure]);
    }

    #[test]
    fn end_queues_an_unqueued_run_as_finished() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| end(c, "run", "r1", Hint::Failed, t(3), t(1), Some("abc")))
            .unwrap();
        let i = led.call_blocking(|c| get(c, "run", "r1")).unwrap().unwrap();
        assert_eq!((i.kind, i.how, i.at), (RestKind::Finished, How::Screen, t(3)));
        assert_eq!(i.hints, [Hint::Failed]);
        assert_eq!((i.from, i.from_head.as_deref()), (t(1), Some("abc")));
    }

    #[test]
    fn end_after_a_review_keeps_the_reply_and_the_new_start() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| {
            rest(c, "run", "r1", RestKind::Asks, How::Reply, &[], t(2), t(0), None)?;
            review(c, "run", "r1", t(3), None)?;
            end(c, "run", "r1", Hint::Stopped, t(6), t(0), None)
        })
        .unwrap();
        let i = led.call_blocking(|c| get(c, "run", "r1")).unwrap().unwrap();
        assert_eq!((i.kind, i.how, i.at), (RestKind::Finished, How::Reply, t(6)));
        assert_eq!((i.rests, i.from), (1, t(3)));
        let last = led.call_blocking(|c| last_rest(c, "run", "r1")).unwrap();
        assert_eq!(last, Some((t(6), How::Reply)));
    }

    #[test]
    fn review_of_nothing_is_false() {
        let led = Ledger::open_in_memory().unwrap();
        assert!(!led.call_blocking(|c| review(c, "run", "nope", t(1), None)).unwrap());
        assert!(led.call_blocking(|c| last_rest(c, "run", "nope")).unwrap().is_none());
    }
}
