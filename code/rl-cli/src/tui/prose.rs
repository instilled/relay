//! The report as a short plain-text narrative: needs first, grouped by co-worker.
//!
//! Pure rendering over a `Report`; no styling, so it is also the substrate for a later
//! spoken version. Grouping and order come from `report::lanes()`, phrasing of a run's
//! headline from `report::headline()`.

use std::collections::HashMap;

use jiff::SignedDuration;
use rl_proto::brief::Brief;
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;
use rl_proto::types::Summary;

use super::report::{headline, lanes, written};

/// One short clause for a lead: what it is, then what is known of it. A happened run is told
/// by its written recap when it has one; else the row's own line, else its title — with its
/// headline beside whichever, so the facts are shown as facts, never only paraphrased.
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
                        return with_headline(format!("{}: {t}", lead.title), lead);
                    }
                } else {
                    let b = Brief::parse(&s.text);
                    if let Some(w) = &b.worked_on {
                        let told = match &b.next {
                            Some(n) => format!("{}: {}, next: {}", lead.title, one(w), one(n)),
                            None => format!("{}: {}", lead.title, one(w)),
                        };
                        return with_headline(told, lead);
                    }
                }
            }
            match lead.summary.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => with_headline(format!("{}: {s}", lead.title), lead),
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

/// How long, as a person would say it: "45m", "3h", "1 day", "2 days".
fn coarse(d: SignedDuration) -> String {
    let mins = d.as_secs().max(0) / 60;
    let (hours, days) = (mins / 60, mins / (24 * 60));
    match days {
        2.. => format!("{days} days"),
        1 => "1 day".to_string(),
        _ if hours > 0 => format!("{hours}h"),
        _ => format!("{mins}m"),
    }
}

/// The whole report as prose. Empty bands are left out; an empty report says so. `away` is
/// how long you were away: the lead-in says so.
pub fn prose(report: &Report, briefs: &HashMap<String, Summary>, away: Option<SignedDuration>) -> String {
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
    let lead_in = match away {
        Some(d) => format!("While you were out ({}):", coarse(d)),
        None => "While you were out:".to_string(),
    };
    format!("{lead_in}\n\n{}\n", paragraphs.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
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
            rest: None,
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
        let p = prose(&r, &HashMap::new(), None);
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
        let p = prose(&r, &HashMap::new(), None);
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
        let p = prose(&r, &briefs, Some(SignedDuration::from_hours(3)));
        assert!(p.starts_with("While you were out (3h):"), "{p}");
        assert!(p.contains("the parser") && p.contains("next: tests"), "{p}");
    }

    #[test]
    fn the_lead_in_says_how_long_coarsely() {
        let r = report(vec![], vec![], vec![lead("claude", "run:r1", Band::Happened, "Did")]);
        let said = |d: SignedDuration| prose(&r, &HashMap::new(), Some(d)).lines().next().unwrap().to_string();
        assert_eq!(said(SignedDuration::from_mins(45)), "While you were out (45m):");
        assert_eq!(said(SignedDuration::from_mins(3 * 60 + 20)), "While you were out (3h):");
        assert_eq!(said(SignedDuration::from_hours(30)), "While you were out (1 day):");
        assert_eq!(said(SignedDuration::from_hours(72)), "While you were out (3 days):");
    }

    #[test]
    fn a_recap_keeps_the_facts_beside_it() {
        let facts = Some(RunFacts {
            added: 180,
            removed: 40,
            ..Default::default()
        });
        let mut briefed = lead("claude", "run:r1", Band::Happened, "Refactor");
        briefed.facts = facts.clone();
        let mut deep = lead("claude", "run:r2", Band::Happened, "Port");
        deep.facts = facts.clone();
        let mut told = lead("claude", "run:r3", Band::Happened, "Tidy");
        told.facts = facts;
        told.summary = Some("cleaned up".into());
        let r = report(vec![], vec![], vec![briefed, deep, told]);
        let mut briefs = HashMap::new();
        briefs.insert(
            "r1".to_string(),
            crate::tui::report::tests::summary_of("r1", "Worked on: the parser\nOpen questions: none\nNext: tests"),
        );
        let mut d = crate::tui::report::tests::summary_of("r2", "Ported the store.");
        d.deep = true;
        briefs.insert("r2".to_string(), d);
        let p = prose(&r, &briefs, None);
        assert!(p.contains("Refactor: the parser, next: tests (+180/−40"), "{p}");
        assert!(p.contains("Port: Ported the store. (+180/−40"), "{p}");
        assert!(p.contains("Tidy: cleaned up (+180/−40"), "{p}");
    }

    #[test]
    fn prose_without_a_brief_falls_back_to_the_rows_line() {
        let mut l = lead("claude", "run:r1", Band::Happened, "Refactor");
        l.summary = Some("waiting on you".into());
        let r = report(vec![], vec![], vec![l]);
        let p = prose(&r, &HashMap::new(), None);
        assert!(p.contains("waiting on you"), "{p}");
    }
}
