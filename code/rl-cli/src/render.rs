//! Plain-text rendering shared by CLI commands (and later the TUI's row model).
//! Monochrome; attention states are the only thing that gets colour, and only
//! in the TUI.

use jiff::{Timestamp, tz::TimeZone};
use rl_core::time::fmt_age;
use rl_proto::types::{Confidence, Event, Run, RunState};

/// `blocked`, `idle?` (guess), `idle~` (likely). Exact signals carry no mark.
pub fn state_label(state: RunState, confidence: Confidence) -> String {
    match confidence {
        Confidence::Exact => state.to_string(),
        Confidence::Likely => format!("{state}~"),
        Confidence::Guess => format!("{state}?"),
    }
}

pub fn run_rows(runs: &[Run], now: Timestamp) -> Vec<Vec<String>> {
    runs.iter()
        .map(|r| {
            vec![
                r.id.clone(),
                state_label(r.state, r.confidence),
                r.kind.to_string(),
                fmt_age(r.state_at, now),
                r.group_key(),
                r.title.clone(),
            ]
        })
        .collect()
}

pub const RUN_HEADERS: [&str; 6] = ["ID", "STATE", "KIND", "AGE", "GROUP", "TITLE"];

pub fn event_rows(events: &[Event], now: Timestamp) -> Vec<Vec<String>> {
    events
        .iter()
        .map(|e| {
            vec![
                fmt_clock(e.at, now),
                e.kind.clone(),
                e.run.clone().unwrap_or_default(),
                e.group_key.clone(),
                e.summary.clone(),
            ]
        })
        .collect()
}

pub const EVENT_HEADERS: [&str; 5] = ["AT", "KIND", "RUN", "GROUP", "SUMMARY"];

pub const LEAD_HEADERS: [&str; 4] = ["SOURCE", "WHEN", "LEAD", "ACTION"];

pub fn lead_rows(leads: &[rl_proto::lead::Lead], now: Timestamp) -> Vec<Vec<String>> {
    leads
        .iter()
        .map(|l| {
            vec![
                l.source.clone(),
                fmt_clock(l.at, now),
                match &l.summary {
                    Some(s) => format!("{} — {s}", l.title),
                    None => l.title.clone(),
                },
                l.action.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

/// `14:03:22` today, `Sep 07 14:03` otherwise. Local time.
pub fn fmt_clock(t: Timestamp, now: Timestamp) -> String {
    let tz = TimeZone::system();
    let z = t.to_zoned(tz.clone());
    let n = now.to_zoned(tz);
    if z.date() == n.date() {
        z.strftime("%H:%M:%S").to_string()
    } else {
        z.strftime("%b %d %H:%M").to_string()
    }
}

/// Left-aligned columns, two spaces apart, last column unpadded.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let ncol = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncol) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let line = |cells: Vec<&str>, out: &mut String| {
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == ncol {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        out.push('\n');
    };
    line(headers.to_vec(), &mut out);
    for row in rows {
        line(row.iter().map(String::as_str).collect(), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_rows_show_source_and_title() {
        use rl_proto::lead::{Band, Lead, LeadStatus};
        let now = Timestamp::now();
        let leads = vec![Lead {
            source: "mobile developer".into(),
            key: "pr:412".into(),
            band: Band::NeedsYou,
            title: "review requested on #412".into(),
            summary: None,
            scope: None,
            action: Some("rl decide d1".into()),
            status: LeadStatus::Open,
            at: now,
            expires_at: None,
            facts: None,
        }];
        let rows = lead_rows(&leads, now);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].iter().any(|c| c.contains("mobile developer")));
        assert!(rows[0].iter().any(|c| c.contains("review requested on #412")));
    }

    #[test]
    fn labels_mark_guesses() {
        assert_eq!(state_label(RunState::Idle, Confidence::Guess), "idle?");
        assert_eq!(state_label(RunState::Idle, Confidence::Likely), "idle~");
        assert_eq!(state_label(RunState::Blocked, Confidence::Exact), "blocked");
    }

    #[test]
    fn table_aligns() {
        let t = table(
            &["A", "BB"],
            &[vec!["xxx".into(), "y".into()], vec!["z".into(), "w".into()]],
        );
        assert_eq!(t, "A    BB\nxxx  y\nz    w\n");
    }
}
