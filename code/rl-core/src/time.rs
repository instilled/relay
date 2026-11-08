//! Timestamps are `jiff::Timestamp` everywhere in memory and on disk
//! (RFC 3339).

use jiff::{SignedDuration, Timestamp};

/// `500ms`, `30m`, `8h`, `2d`, `1h30m`. `None` for anything else.
pub fn parse_duration(s: &str) -> Option<SignedDuration> {
    let mut total_ms = 0i64;
    let mut num = 0i64;
    let mut seen_digit = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(d) = c.to_digit(10) {
            num = num.checked_mul(10)?.checked_add(d as i64)?;
            seen_digit = true;
            continue;
        }
        if !seen_digit {
            return None;
        }
        let mult_ms = match c {
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                1
            }
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            'w' => 7 * 86_400_000,
            _ => return None,
        };
        total_ms = total_ms.checked_add(num.checked_mul(mult_ms)?)?;
        num = 0;
        seen_digit = false;
    }
    if seen_digit || total_ms == 0 {
        return None;
    }
    Some(SignedDuration::from_millis(total_ms))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m"), Some(SignedDuration::from_mins(30)));
        assert_eq!(parse_duration("1h30m"), Some(SignedDuration::from_mins(90)));
        assert_eq!(parse_duration("2d"), Some(SignedDuration::from_hours(48)));
        assert_eq!(parse_duration("500ms"), Some(SignedDuration::from_millis(500)));
        assert_eq!(parse_duration("1m500ms"), Some(SignedDuration::from_millis(60_500)));
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("5"), None);
        assert_eq!(parse_duration("garbage"), None);
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
