//! The report as a short plain-text narrative: needs first, grouped by co-worker.
//!
//! Pure rendering over a `Report`; no styling, so it is also the substrate for a later
//! spoken version. Grouping and order come from `report::lanes()`, phrasing of a run's
//! headline from `report::headline()`.

use jiff::Timestamp;
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;

use super::report::{headline, lanes};

/// One short clause for a lead: what it is, then what is known of it.
fn clause(lead: &Lead) -> String {
    let head = headline(lead);
    let detail = match lead.band {
        Band::NeedsYou => match &lead.action {
            Some(a) => format!("{} ({a})", lead.title),
            None => lead.title.clone(),
        },
        Band::Live | Band::Happened => lead.title.clone(),
    };
    if head.is_empty() {
        detail
    } else {
        format!("{detail} ({head})")
    }
}

/// The whole report as prose. Empty bands are left out; an empty report says so.
pub fn prose(report: &Report, _now: Timestamp) -> String {
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
                    .map(|r| clause(r.lead))
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
    format!("While you were out:\n\n{}\n", paragraphs.join("\n\n"))
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
        let p = prose(&r, Timestamp::from_second(10).unwrap());
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
        let p = prose(&r, Timestamp::from_second(10).unwrap());
        assert!(!p.contains("Needs you") && !p.contains("Live"), "{p}");
        assert!(p.contains("Happened"), "{p}");
    }
}
