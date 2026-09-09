//! Five-field cron, local time. `min hour dom month dow`; lists, ranges,
//! steps, `*`, month and weekday names; `7` is Sunday like `0`. Day-of-month
//! and day-of-week combine the classic way: both restricted means either
//! matches. No seconds, no `@daily`.

use anyhow::{Context, Result, bail};
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: [bool; 60],
    hour: [bool; 24],
    dom: [bool; 32],
    month: [bool; 13],
    dow: [bool; 7],
    dom_any: bool,
    dow_any: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Beyond this the expression matches nothing (Feb 30, Feb 29 on `*/…`).
const HORIZON_DAYS: i64 = 366 * 5;

impl Cron {
    pub fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            bail!("cron needs 5 fields (min hour dom month dow), got {}", fields.len());
        }
        let mut c = Cron {
            minute: [false; 60],
            hour: [false; 24],
            dom: [false; 32],
            month: [false; 13],
            dow: [false; 7],
            dom_any: fields[2] == "*",
            dow_any: fields[4] == "*",
        };
        fill(&mut c.minute, fields[0], 0, 59, &[]).context("minute")?;
        fill(&mut c.hour, fields[1], 0, 23, &[]).context("hour")?;
        fill(&mut c.dom, fields[2], 1, 31, &[]).context("day of month")?;
        fill(&mut c.month, fields[3], 1, 12, &MONTHS).context("month")?;
        let mut dow = [false; 8];
        fill(&mut dow, fields[4], 0, 7, &DAYS).context("day of week")?;
        for (i, on) in dow.iter().enumerate().take(7) {
            c.dow[i] = *on;
        }
        if dow[7] {
            c.dow[0] = true;
        }
        Ok(c)
    }

    fn matches(&self, z: &Zoned) -> bool {
        let day_ok = match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => self.dom[z.day() as usize],
            (true, false) => self.dow[weekday_index(z.weekday())],
            (false, false) => self.dom[z.day() as usize] || self.dow[weekday_index(z.weekday())],
        };
        self.minute[z.minute() as usize] && self.hour[z.hour() as usize] && self.month[z.month() as usize] && day_ok
    }

    /// The first matching minute strictly after `after`, in `tz`.
    pub fn next_after_in(&self, after: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
        let start = after.to_zoned(tz.clone());
        // Next whole minute.
        let mut z = start
            .with()
            .second(0)
            .subsec_nanosecond(0)
            .build()
            .ok()?
            .checked_add(1.minute())
            .ok()?;
        let horizon = start.checked_add(HORIZON_DAYS.days()).ok()?;
        while z <= horizon {
            if !self.month[z.month() as usize] {
                // Skip to the first of next month.
                z = z
                    .with()
                    .day(1)
                    .hour(0)
                    .minute(0)
                    .build()
                    .ok()?
                    .checked_add(1.month())
                    .ok()?;
                continue;
            }
            let day_ok = match (self.dom_any, self.dow_any) {
                (true, true) => true,
                (false, true) => self.dom[z.day() as usize],
                (true, false) => self.dow[weekday_index(z.weekday())],
                (false, false) => self.dom[z.day() as usize] || self.dow[weekday_index(z.weekday())],
            };
            if !day_ok {
                z = z.with().hour(0).minute(0).build().ok()?.checked_add(1.day()).ok()?;
                continue;
            }
            if !self.hour[z.hour() as usize] {
                z = z.with().minute(0).build().ok()?.checked_add(1.hour()).ok()?;
                continue;
            }
            if self.matches(&z) {
                return Some(z.timestamp());
            }
            z = z.checked_add(1.minute()).ok()?;
        }
        None
    }

    pub fn next_after(&self, after: Timestamp) -> Option<Timestamp> {
        self.next_after_in(after, &TimeZone::system())
    }
}

fn weekday_index(w: Weekday) -> usize {
    match w {
        Weekday::Sunday => 0,
        Weekday::Monday => 1,
        Weekday::Tuesday => 2,
        Weekday::Wednesday => 3,
        Weekday::Thursday => 4,
        Weekday::Friday => 5,
        Weekday::Saturday => 6,
    }
}

/// Marks the slots a field selects: `*`, `a`, `a-b`, `*/s`, `a-b/s`, lists.
fn fill(slots: &mut [bool], field: &str, lo: u32, hi: u32, names: &[&str]) -> Result<()> {
    let number = |s: &str| -> Result<u32> {
        if let Ok(n) = s.parse::<u32>() {
            return Ok(n);
        }
        let l = s.to_ascii_lowercase();
        match names.iter().position(|n| *n == l) {
            Some(i) => Ok(i as u32 + lo),
            None => bail!(
                "`{s}` is not a number{}",
                if names.is_empty() { "" } else { " or a name" }
            ),
        }
    };
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .ok()
                    .filter(|s| *s > 0)
                    .context("step must be a positive number")?,
            ),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (number(a)?, number(b)?)
        } else {
            let a = number(range)?;
            // `5/10` means from 5 on; `5` alone means just 5.
            if step > 1 { (a, hi) } else { (a, a) }
        };
        if a < lo || b > hi || a > b {
            bail!("`{part}` is outside {lo}-{hi}");
        }
        let mut v = a;
        while v <= b {
            slots[v as usize] = true;
            v += step;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }
    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }
    fn next(expr: &str, after: &str) -> String {
        Cron::parse(expr)
            .unwrap()
            .next_after_in(at(after), &utc())
            .map(|t| t.to_string())
            .unwrap_or_else(|| "never".into())
    }

    #[test]
    fn every_minute_and_fixed_times() {
        assert_eq!(next("* * * * *", "2026-09-09T10:00:30Z"), "2026-09-09T10:01:00Z");
        assert_eq!(next("0 2 * * *", "2026-09-09T10:00:00Z"), "2026-09-10T02:00:00Z");
        assert_eq!(next("0 2 * * *", "2026-09-09T01:59:00Z"), "2026-09-09T02:00:00Z");
        assert_eq!(next("30 9 * * mon", "2026-09-09T10:00:00Z"), "2026-09-14T09:30:00Z");
        assert_eq!(next("0 0 1 jan *", "2026-09-09T10:00:00Z"), "2027-01-01T00:00:00Z");
    }

    #[test]
    fn lists_ranges_and_steps() {
        assert_eq!(next("*/15 * * * *", "2026-09-09T10:16:00Z"), "2026-09-09T10:30:00Z");
        assert_eq!(next("0 9-17/4 * * *", "2026-09-09T13:00:00Z"), "2026-09-09T17:00:00Z");
        assert_eq!(next("0 9-17/4 * * *", "2026-09-09T17:00:00Z"), "2026-09-10T09:00:00Z");
        assert_eq!(next("0 12 * * 1,3,5", "2026-09-09T13:00:00Z"), "2026-09-11T12:00:00Z");
        assert_eq!(next("0 12 * * 7", "2026-09-09T13:00:00Z"), "2026-09-13T12:00:00Z");
    }

    #[test]
    fn dom_and_dow_together_mean_either() {
        // 2026-09-15 is a Tuesday; the 13th is a Sunday.
        assert_eq!(next("0 0 15 * sun", "2026-09-09T10:00:00Z"), "2026-09-13T00:00:00Z");
        assert_eq!(next("0 0 15 * sun", "2026-09-13T10:00:00Z"), "2026-09-15T00:00:00Z");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(Cron::parse("* * * *").is_err());
        assert!(Cron::parse("60 * * * *").is_err());
        assert!(Cron::parse("* * 0 * *").is_err());
        assert!(Cron::parse("* * * * fun").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert_eq!(next("0 0 30 feb *", "2026-09-09T10:00:00Z"), "never");
    }
}
