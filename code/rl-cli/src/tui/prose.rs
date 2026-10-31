//! The report as a short plain-text narrative: the written summary `w` shows, and `rl report`
//! prints.
//!
//! Pure rendering over a `Report`, walking the inbox: what needs you first, then each team in
//! the list's order. Built from the queue and the logs; nothing is generated. No styling, so it
//! is also the substrate for a later spoken version.

use jiff::{SignedDuration, Timestamp};
use rl_core::time::fmt_when;
use rl_proto::lead::Lead;
use rl_proto::methods::Report;
use rl_proto::queue::Hint;

use super::inbox::{Mark, Team, inbox, mark};
use super::report::{commits, first_line, git, last, needs_you};

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

fn hinted(lead: &Lead, hint: Hint) -> bool {
    lead.rest.as_ref().is_some_and(|r| r.item.hints.contains(&hint))
}

/// How an ask asks: a permission by what it wants to run; a lead that is not
/// a session's (a decision, a posted lead) by how to act on it.
fn asks(lead: &Lead) -> String {
    if lead.rest.is_none() {
        return match &lead.action {
            Some(a) => format!("waits for you ({a})"),
            None => "waits for you".into(),
        };
    }
    if hinted(lead, Hint::Permission) {
        return match last(lead).map(first_line) {
            Some(what) => format!("asks permission ({what})"),
            None => "asks permission".into(),
        };
    }
    if hinted(lead, Hint::Question) {
        "asks a question".into()
    } else if hinted(lead, Hint::Unsure) {
        "may be asking".into()
    } else {
        "asks".into()
    }
}

/// One item of a team: its name, how it came to rest, git where there is one, and the first
/// line of what it last said.
fn clause(lead: &Lead) -> String {
    let state = match mark(lead) {
        Mark::Asks => "asks",
        Mark::Failed => "failed",
        Mark::Done if hinted(lead, Hint::Stopped) => "stopped",
        Mark::Done => "done",
        Mark::Working => "working",
    };
    let mut out = format!("{} {state}", lead.title);
    if let Some(g) = git(&[lead]) {
        out.push_str(&format!(" ({g})"));
    }
    if let Some(t) = last(lead) {
        out.push_str(&format!(" — {}", first_line(t)));
    }
    out
}

/// A team's counts, the zeros left out; commits only where there is git.
fn counts(t: &Team) -> String {
    let mut parts: Vec<String> = [
        (t.needs, needs_you(t.needs)),
        (t.failed, format!("{} failed", t.failed)),
        (t.done, format!("{} done", t.done)),
        (t.working, format!("{} working", t.working)),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(_, w)| w)
    .collect();
    parts.extend(t.commits.map(commits));
    parts.join(", ")
}

/// The whole report as prose. An empty report says so. `away` is how long you were away: the
/// opening says so.
pub fn prose(report: &Report, away: Option<SignedDuration>) -> String {
    let i = inbox(report);
    if i.teams.is_empty() {
        return "Nothing to report.\n".to_string();
    }
    let now = Timestamp::now();
    let failed: usize = i.teams.iter().map(|t| t.failed).sum();
    let mut tally: Vec<String> = [
        (i.needs.len(), needs_you(i.needs.len())),
        (failed, format!("{failed} failed")),
        (i.to_review, format!("{} to review", i.to_review)),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(_, w)| w)
    .collect();
    tally.extend(i.commits.map(commits));
    if tally.is_empty() {
        tally.push("nothing to review".into());
    }
    let lead_in = match away {
        Some(d) => format!("While you were out ({}):", coarse(d)),
        None => "While you were out:".to_string(),
    };
    let mut paragraphs = vec![format!("{lead_in} {}.", tally.join(", "))];
    for l in &i.needs {
        let team = i
            .teams
            .iter()
            .find(|t| t.items.iter().any(|x| x.key == l.key))
            .map_or("", |t| t.name.as_str());
        paragraphs.push(format!(
            "Needs you: {} ({team}) {}, waiting since {}.",
            l.title,
            asks(l),
            fmt_when(l.at, now)
        ));
    }
    for t in &i.teams {
        // Its asks were said above, under what needs you.
        let clauses: Vec<String> = t
            .items
            .iter()
            .filter(|l| mark(l) != Mark::Asks)
            .map(|l| clause(l))
            .collect();
        let mut p = format!("{}: {}.", t.name, counts(t));
        if !clauses.is_empty() {
            let said = clauses.join("; ");
            // A last line that ends a sentence already keeps its own stop.
            let stop = if said.ends_with(['.', '?', '!', '…']) {
                ""
            } else {
                "."
            };
            p.push_str(&format!(" {said}{stop}"));
        }
        paragraphs.push(p);
    }
    format!("{}\n", paragraphs.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::lead::{Band, LeadStatus};
    use rl_proto::queue::{Changed, How, LeadRest, QueueItem, RestKind};

    fn lead(key: &str, band: Band, scope: &str, at: i64) -> Lead {
        Lead {
            source: "claude".into(),
            key: key.into(),
            band,
            title: key.into(),
            summary: None,
            scope: Some(scope.into()),
            action: None,
            status: LeadStatus::Open,
            at: Timestamp::from_second(at).unwrap(),
            expires_at: None,
            facts: None,
            rest: None,
        }
    }

    fn queued(mut l: Lead, kind: RestKind, hints: &[Hint], last: &str, changed: Option<Changed>) -> Lead {
        l.rest = Some(LeadRest {
            item: QueueItem {
                source: "run".into(),
                reference: l.key.clone(),
                kind,
                how: How::Reply,
                hints: hints.to_vec(),
                at: l.at,
                rests: 1,
                from: l.at,
                from_head: None,
            },
            last: Some(last.into()),
            changed,
        });
        l
    }

    fn report(needs_you: Vec<Lead>, live: Vec<Lead>, happened: Vec<Lead>) -> Report {
        Report {
            needs_you,
            live,
            happened,
            tags: vec![],
        }
    }

    fn git(added: u64, removed: u64, commits: u32) -> Option<Changed> {
        Some(Changed {
            commits,
            added,
            removed,
            ..Changed::default()
        })
    }

    #[test]
    fn prose_walks_needs_then_teams() {
        let ask = queued(
            lead("pick a db", Band::NeedsYou, "/p/relay", 10),
            RestKind::Asks,
            &[Hint::Question],
            "Postgres or SQLite?",
            None,
        );
        let refactor = queued(
            lead("refactor parser", Band::Happened, "/p/relay", 20),
            RestKind::Finished,
            &[],
            "Split the lexer out.\n\nAll tests pass.",
            git(180, 40, 3),
        );
        let failed = queued(
            lead("docs build", Band::Happened, "/p/docs", 15),
            RestKind::Finished,
            &[Hint::Failed],
            "mdbook exited 1",
            None,
        );
        let r = report(
            vec![ask],
            vec![lead("tidy", Band::Live, "/p/docs", 30)],
            vec![refactor, failed],
        );
        let p = prose(&r, Some(SignedDuration::from_hours(14)));
        let paras: Vec<&str> = p.trim_end().split("\n\n").collect();
        assert_eq!(
            paras[0], "While you were out (14h): 1 needs you, 1 failed, 3 to review, 3 commits.",
            "{p}"
        );
        let since = fmt_when(Timestamp::from_second(10).unwrap(), Timestamp::now());
        assert_eq!(
            paras[1],
            format!("Needs you: pick a db (relay) asks a question, waiting since {since}."),
            "{p}"
        );
        // Teams in the list's order: relay asks, so it leads; its ask is not said twice.
        assert_eq!(
            paras[2],
            "relay: 1 needs you, 1 done, 3 commits. refactor parser done (+180/−40 · 3 commits) — Split the lexer out.",
            "{p}"
        );
        assert_eq!(
            paras[3], "docs: 1 failed, 1 working. docs build failed — mdbook exited 1; tidy working.",
            "{p}"
        );
        assert_eq!(paras.len(), 4, "{p}");
    }

    #[test]
    fn prose_without_git_has_no_commit_words() {
        let r = report(
            vec![queued(
                lead("deploy", Band::NeedsYou, "/p/infra", 1),
                RestKind::Asks,
                &[Hint::Permission],
                "cargo publish",
                None,
            )],
            vec![],
            vec![queued(
                lead("notes", Band::Happened, "/p/infra", 2),
                RestKind::Finished,
                &[Hint::Stopped],
                "Wrote the notes.",
                None,
            )],
        );
        let p = prose(&r, None);
        assert!(!p.contains("commit") && !p.contains("+0/"), "{p}");
        assert!(p.starts_with("While you were out: 1 needs you, 2 to review.\n"), "{p}");
        assert!(p.contains("deploy (infra) asks permission (cargo publish)"), "{p}");
        assert!(
            p.contains("infra: 1 needs you, 1 done. notes stopped — Wrote the notes."),
            "{p}"
        );
    }

    #[test]
    fn an_empty_report_says_so() {
        assert_eq!(prose(&report(vec![], vec![], vec![]), None), "Nothing to report.\n");
    }

    #[test]
    fn the_lead_in_says_how_long_coarsely() {
        let r = report(vec![], vec![], vec![lead("did", Band::Happened, "/p/a", 1)]);
        let said = |d: SignedDuration| {
            let p = prose(&r, Some(d));
            p.split(':').next().unwrap().to_string()
        };
        assert_eq!(said(SignedDuration::from_mins(45)), "While you were out (45m)");
        assert_eq!(said(SignedDuration::from_mins(3 * 60 + 20)), "While you were out (3h)");
        assert_eq!(said(SignedDuration::from_hours(30)), "While you were out (1 day)");
        assert_eq!(said(SignedDuration::from_hours(72)), "While you were out (3 days)");
    }

    #[test]
    fn a_lead_with_no_session_waits_for_you() {
        let mut l = lead("which db", Band::NeedsYou, "/p/a", 1);
        assert!(prose(&report(vec![l.clone()], vec![], vec![]), None).contains("which db (a) waits for you,"));
        l.action = Some("rl decide d1".into());
        let p = prose(&report(vec![l], vec![], vec![]), None);
        assert!(p.contains("which db (a) waits for you (rl decide d1),"), "{p}");
    }
}
