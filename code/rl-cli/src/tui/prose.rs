//! The report as a short plain-text narrative: needs first, grouped by co-worker.
//!
//! Pure rendering over a `Report`; no styling, so it is also the substrate for a later
//! spoken version. Grouping and order come from `report::lanes()`, phrasing of a run's
//! headline from `report::headline()`.

use std::collections::HashMap;

use jiff::Timestamp;
use rl_core::time::fmt_duration;
use rl_proto::brief::Brief;
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;
use rl_proto::types::Summary;

use super::report::{headline, lanes, written};

/// One short clause for a lead: what it is, then what is known of it. A happened run is told
/// by its written recap when it has one; else the row's own line, else its headline, else its title.
fn clause(lead: &Lead, briefs: &HashMap<String, Summary>) -> String {
    match lead.band {
        Band::NeedsYou => {
            let detail = match &lead.action {
                Some(a) => format!("{} ({a})", lead.title),
                None => lead.title.clone(),
            };
            with_headline(detail, lead)
        }
        Band::Live => with_headline(lead.title.clone(), lead),
        Band::Happened => {
            if let Some(s) = written(briefs, lead) {
                let one = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
                if s.deep {
                    let t = one(&s.text);
                    if !t.is_empty() {
                        return format!("{}: {t}", lead.title);
                    }
                } else {
                    let b = Brief::parse(&s.text);
                    if let Some(w) = &b.worked_on {
                        return match &b.next {
                            Some(n) => format!("{}: {}, next: {}", lead.title, one(w), one(n)),
                            None => format!("{}: {}", lead.title, one(w)),
                        };
                    }
                }
            }
            match lead.summary.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => format!("{}: {s}", lead.title),
                None => with_headline(lead.title.clone(), lead),
            }
        }
    }
}

fn with_headline(detail: String, lead: &Lead) -> String {
    let head = headline(lead);
    if head.is_empty() {
        detail
    } else {
        format!("{detail} ({head})")
    }
}

/// The whole report as prose. Empty bands are left out; an empty report says so. `since` is
/// when you left: the lead-in says how long ago.
pub fn prose(report: &Report, briefs: &HashMap<String, Summary>, since: Option<Timestamp>, now: Timestamp) -> String {
    let lanes = lanes(report);
    let mut paragraphs = Vec::new();
    for (label, band) in [
        ("Needs you", Band::NeedsYou),
        ("Live", Band::Live),
        ("Happened", Band::Happened),
    ] {
        let groups: Vec<String> = lanes
            .iter()
            .filter_map(|lane| {
                let clauses: Vec<String> = lane
                    .rows
                    .iter()
                    .filter(|r| r.lead.band == band)
                    .map(|r| clause(r.lead, briefs))
                    .collect();
                (!clauses.is_empty()).then(|| format!("{}: {}.", lane.name, clauses.join("; ")))
            })
            .collect();
        if !groups.is_empty() {
            paragraphs.push(format!("{label}. {}", groups.join(" ")));
        }
    }
    if paragraphs.is_empty() {
        return "Nothing to report.\n".to_string();
    }
    let lead_in = match since {
        Some(t) => format!("While you were out ({}):", fmt_duration(now.duration_since(t))),
        None => "While you were out:".to_string(),
    };
    format!("{lead_in}\n\n{}\n", paragraphs.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::facts::RunFacts;
    use rl_proto::lead::LeadStatus;

    fn lead(source: &str, key: &str, band: Band, title: &str) -> Lead {
        Lead {
            source: source.into(),
            key: key.into(),
            band,
            title: title.into(),
            summary: None,
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at: Timestamp::from_second(1).unwrap(),
            expires_at: None,
            facts: None,
        }
    }

    fn report(needs: Vec<Lead>, live: Vec<Lead>, happened: Vec<Lead>) -> Report {
        Report {
            needs_you: needs,
            live,
            happened,
            tags: vec![],
        }
    }

    #[test]
    fn prose_leads_with_needs_you() {
        let mut done = lead("claude", "run:r1", Band::Happened, "Refactor parser");
        done.facts = Some(RunFacts {
            added: 180,
            removed: 40,
            commits: 3,
            ..Default::default()
        });
        let r = report(
            vec![lead("claude", "d1", Band::NeedsYou, "Pick a db")],
            vec![],
            vec![done],
        );
        let p = prose(&r, &HashMap::new(), None, Timestamp::from_second(10).unwrap());
        let (n, h) = (p.find("Pick a db").unwrap(), p.find("Refactor parser").unwrap());
        assert!(n < h, "{p}");
        assert!(p.contains("+180/−40") && p.contains("3 commits"), "{p}");
    }

    #[test]
    fn prose_omits_empty_bands() {
        let r = report(
            vec![],
            vec![],
            vec![lead("claude", "run:r1", Band::Happened, "Did a thing")],
        );
        let p = prose(&r, &HashMap::new(), None, Timestamp::from_second(10).unwrap());
        assert!(!p.contains("Needs you") && !p.contains("Live"), "{p}");
        assert!(p.contains("Happened"), "{p}");
    }

    #[test]
    fn prose_tells_a_run_by_its_brief_and_says_how_long() {
        let r = report(
            vec![],
            vec![],
            vec![lead("claude", "run:r1", Band::Happened, "Refactor")],
        );
        let mut briefs = HashMap::new();
        briefs.insert(
            "r1".to_string(),
            crate::tui::report::tests::summary_of("r1", "Worked on: the parser\nOpen questions: none\nNext: tests"),
        );
        let now = Timestamp::from_second(100_000).unwrap();
        let p = prose(&r, &briefs, Some(now - jiff::SignedDuration::from_hours(3)), now);
        assert!(p.starts_with("While you were out (3h00m):"), "{p}");
        assert!(p.contains("the parser") && p.contains("next: tests"), "{p}");
    }

    #[test]
    fn prose_without_a_brief_falls_back_to_the_rows_line() {
        let mut l = lead("claude", "run:r1", Band::Happened, "Refactor");
        l.summary = Some("waiting on you".into());
        let r = report(vec![], vec![], vec![l]);
        let p = prose(&r, &HashMap::new(), None, Timestamp::from_second(10).unwrap());
        assert!(p.contains("waiting on you"), "{p}");
    }
}
