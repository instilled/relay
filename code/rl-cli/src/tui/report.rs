//! The report as lanes: leads grouped by the work or team they belong to.
//!
//! Pure grouping and ordering, no rendering. `Lane`/`Row` borrow from the
//! `Report` (cheap; the renderer holds the report for the frame anyway).

use std::collections::{HashMap, HashSet};

use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;

pub struct Row<'a> {
    pub lead: &'a Lead,
}

pub struct Lane<'a> {
    /// The tag, or the project basename, or "my work".
    pub name: String,
    /// More than one distinct source lands in this lane.
    pub is_team: bool,
    pub needs: usize,
    pub live: usize,
    pub done: usize,
    /// Ordered needs -> live -> happened, newest first within a band.
    pub rows: Vec<Row<'a>>,
}

fn lane_name(lead: &Lead, tags: &HashMap<&str, &str>) -> String {
    if let Some(tag) = tags.get(lead.source.as_str()) {
        return (*tag).to_string();
    }
    lead.scope
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "my work".to_string())
}

pub fn lanes(report: &Report) -> Vec<Lane<'_>> {
    let tags: HashMap<&str, &str> = report
        .tags
        .iter()
        .map(|t| (t.source.as_str(), t.tag.as_str()))
        .collect();
    let mut by_name: HashMap<String, (Lane<'_>, HashSet<&str>)> = HashMap::new();
    let all = report.needs_you.iter().chain(&report.live).chain(&report.happened);
    for lead in all {
        let name = lane_name(lead, &tags);
        let (lane, sources) = by_name.entry(name.clone()).or_insert_with(|| {
            let lane = Lane {
                name,
                is_team: false,
                needs: 0,
                live: 0,
                done: 0,
                rows: Vec::new(),
            };
            (lane, HashSet::new())
        });
        match lead.band {
            Band::NeedsYou => lane.needs += 1,
            Band::Live => lane.live += 1,
            Band::Happened => lane.done += 1,
        }
        sources.insert(lead.source.as_str());
        lane.rows.push(Row { lead });
    }
    let mut out: Vec<Lane<'_>> = by_name
        .into_values()
        .map(|(mut lane, sources)| {
            lane.is_team = sources.len() > 1;
            lane.rows
                .sort_by(|a, b| (a.lead.band.rank(), b.lead.at).cmp(&(b.lead.band.rank(), a.lead.at)));
            lane
        })
        .collect();
    let newest = |l: &Lane<'_>| l.rows.iter().map(|r| r.lead.at).max();
    // Name is the final tiebreak so the order is deterministic.
    out.sort_by(|a, b| {
        (b.needs > 0)
            .cmp(&(a.needs > 0))
            .then_with(|| newest(b).cmp(&newest(a)))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::lead::LeadStatus;
    use rl_proto::methods::SourceTag;

    fn ts(n: i64) -> Timestamp {
        Timestamp::from_second(n).unwrap()
    }

    fn mk_lead(source: &str, key: &str, band: Band, scope: Option<&str>, at: Timestamp) -> Lead {
        Lead {
            source: source.into(),
            key: key.into(),
            band,
            title: key.into(),
            summary: None,
            scope: scope.map(Into::into),
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
        }
    }

    #[test]
    fn untagged_lead_lands_under_its_project_basename() {
        let r = Report {
            happened: vec![],
            needs_you: vec![],
            live: vec![mk_lead(
                "shell",
                "r1",
                Band::Live,
                Some("/Users/x/Projects/relay"),
                ts(1),
            )],
            tags: vec![],
        };
        let ls = lanes(&r);
        assert_eq!(ls[0].name, "relay");
        assert!(!ls[0].is_team);
    }

    #[test]
    fn tag_moves_every_band_of_a_source_into_one_lane() {
        let r = Report {
            needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(3))],
            live: vec![mk_lead("claude", "b", Band::Live, Some("/p/relay"), ts(2))],
            happened: vec![mk_lead("codex", "c", Band::Happened, Some("/p/relay"), ts(1))],
            tags: vec![
                SourceTag {
                    source: "claude".into(),
                    tag: "dev".into(),
                },
                SourceTag {
                    source: "codex".into(),
                    tag: "dev".into(),
                },
            ],
        };
        let ls = lanes(&r);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].name, "dev");
        assert!(ls[0].is_team);
        assert_eq!(ls[0].rows.len(), 3);
        assert_eq!(ls[0].rows[0].lead.band, Band::NeedsYou);
        assert_eq!((ls[0].needs, ls[0].live, ls[0].done), (1, 1, 1));
    }

    #[test]
    fn needs_bearing_lanes_sort_first() {
        let r = Report {
            needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(1))],
            live: vec![mk_lead("shell", "b", Band::Live, Some("/p/ops"), ts(9))],
            happened: vec![],
            tags: vec![],
        };
        let ls = lanes(&r);
        assert_eq!(ls[0].name, "relay");
    }

    #[test]
    fn scopeless_lead_falls_to_my_work() {
        let r = Report {
            needs_you: vec![],
            live: vec![],
            tags: vec![],
            happened: vec![mk_lead("agent", "x", Band::Happened, None, ts(1))],
        };
        assert_eq!(lanes(&r)[0].name, "my work");
    }
}
