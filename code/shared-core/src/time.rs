//! Timestamps are `jiff::Timestamp` everywhere in memory and on the wire
//! (RFC 3339), and epoch milliseconds in the ledger.

use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};

pub fn now() -> Timestamp {
    Timestamp::now()
}

/// Accepts a relative duration (`30m`, `8h`, `2d`, `1h30m`), a date
/// (`2026-09-08`, local midnight) or an RFC 3339 instant.
pub fn parse_since(s: &str, now: Timestamp) -> Result<Timestamp> {
    let s = s.trim();
    if let Some(d) = parse_duration(s) {
        return Ok(now - d);
    }
    if let Ok(date) = s.parse::<Date>() {
        let zoned = date.to_zoned(TimeZone::system()).context("local midnight")?;
        return Ok(zoned.timestamp());
    }
    s.parse::<Timestamp>()
        .with_context(|| format!("`{s}` is not a duration (8h), date (2026-09-08) or RFC 3339 instant"))
}

fn parse_duration(s: &str) -> Option<SignedDuration> {
    let mut total = 0i64;
    let mut num = 0i64;
    let mut seen_digit = false;
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            num = num.checked_mul(10)?.checked_add(d as i64)?;
            seen_digit = true;
            continue;
        }
        if !seen_digit {
            return None;
        }
        let mult = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => return None,
        };
        total = total.checked_add(num.checked_mul(mult)?)?;
        num = 0;
        seen_digit = false;
    }
    if seen_digit || total == 0 {
        return None;
    }
    Some(SignedDuration::from_secs(total))
}

/// Compact age for tables: `12s`, `4m`, `3h`, `2d`.
pub fn fmt_age(from: Timestamp, now: Timestamp) -> String {
    let secs = now.duration_since(from).as_secs().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `41m03s`, `1h12m`, `3d4h` — for durations shown as facts.
pub fn fmt_duration(d: SignedDuration) -> String {
    let secs = d.as_secs().max(0);
    let (dd, hh, mm, ss) = (secs / 86_400, (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    match (dd, hh, mm) {
        (0, 0, 0) => format!("{ss}s"),
        (0, 0, _) => format!("{mm}m{ss:02}s"),
        (0, _, _) => format!("{hh}h{mm:02}m"),
        _ => format!("{dd}d{hh}h"),
    }
}

pub fn to_millis(t: Timestamp) -> i64 {
    t.as_millisecond()
}

pub fn from_millis(ms: i64) -> Result<Timestamp> {
    Timestamp::from_millisecond(ms).with_context(|| format!("timestamp {ms}ms out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let now = "2026-09-08T12:00:00Z".parse::<Timestamp>().unwrap();
        assert_eq!(
            parse_since("30m", now).unwrap(),
            "2026-09-08T11:30:00Z".parse().unwrap()
        );
        assert_eq!(
            parse_since("1h30m", now).unwrap(),
            "2026-09-08T10:30:00Z".parse().unwrap()
        );
        assert_eq!(parse_since("2d", now).unwrap(), "2026-09-06T12:00:00Z".parse().unwrap());
        assert!(parse_since("m", now).is_err());
        assert!(parse_since("5x", now).is_err());
        assert!(parse_since("garbage", now).is_err());
    }

    #[test]
    fn instants_and_dates() {
        let now = now();
        assert_eq!(
            parse_since("2026-09-08T10:00:00Z", now).unwrap(),
            "2026-09-08T10:00:00Z".parse().unwrap()
        );
        let d = parse_since("2026-09-08", now).unwrap();
        // Local midnight is within a day either side of UTC midnight.
        let utc_midnight = "2026-09-08T00:00:00Z".parse::<Timestamp>().unwrap();
        assert!(d.duration_since(utc_midnight).abs() <= SignedDuration::from_hours(14));
    }

    #[test]
    fn ages() {
        let now = "2026-09-08T12:00:00Z".parse::<Timestamp>().unwrap();
        assert_eq!(fmt_age(now - SignedDuration::from_secs(5), now), "5s");
        assert_eq!(fmt_age(now - SignedDuration::from_secs(300), now), "5m");
        assert_eq!(fmt_age(now - SignedDuration::from_hours(30), now), "1d");
        assert_eq!(fmt_duration(SignedDuration::from_secs(41 * 60 + 3)), "41m03s");
        assert_eq!(fmt_duration(SignedDuration::from_secs(3600 + 12 * 60)), "1h12m");
    }
}
