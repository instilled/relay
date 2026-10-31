//! The report as an inbox: what needs you, the teams with their counts and
//! gists, the order `j`/`k` walk, and where it opens.
//!
//! Pure grouping and ordering, no rendering. Everything borrows from the
//! `Report`.

use std::collections::HashMap;

use jiff::{SignedDuration, Timestamp};
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;
use rl_proto::queue::Hint;

use super::report::lane_name;

/// What is selected in the list pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    All,
    Team(String),
    /// A lead, by key.
    Item(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Asks,
    Failed,
    Done,
    Working,
}

/// An ask whose run then stopped or failed waits on no one: it is done, or
/// failed.
pub fn mark(lead: &Lead) -> Mark {
    let ended = lead.rest.as_ref().is_some_and(|r| r.item.ended());
    if lead.band == Band::NeedsYou && !ended {
        Mark::Asks
    } else if hinted(lead, Hint::Failed) {
        Mark::Failed
    } else if lead.band == Band::Live {
        Mark::Working
    } else {
        Mark::Done
    }
}

fn hinted(lead: &Lead, hint: Hint) -> bool {
    lead.rest.as_ref().is_some_and(|r| r.item.hints.contains(&hint))
}

fn changed(lead: &Lead) -> Option<&rl_proto::queue::Changed> {
    lead.rest.as_ref().and_then(|r| r.changed.as_ref())
}

pub struct Team<'a> {
    pub name: String,
    pub needs: usize,
    pub failed: usize,
    pub done: usize,
    pub working: usize,
    /// `None` when no item has a repo to count in.
    pub commits: Option<u32>,
    pub gist: String,
    /// Asks, then failed, then done, then working; newest first within.
    pub items: Vec<&'a Lead>,
}

pub struct Inbox<'a> {
    /// Every ask, oldest first.
    pub needs: Vec<&'a Lead>,
    pub teams: Vec<Team<'a>>,
    /// Everything waiting for your review: queued items and decisions.
    pub to_review: usize,
    pub working: usize,
    pub commits: Option<u32>,
}

fn sum_commits<'a>(leads: impl Iterator<Item = &'a Lead>) -> Option<u32> {
    leads
        .filter_map(|l| changed(l).map(|c| c.commits))
        .fold(None, |a, c| Some(a.unwrap_or(0) + c))
}

fn rank(m: Mark) -> u8 {
    match m {
        Mark::Asks => 0,
        Mark::Failed => 1,
        Mark::Done => 2,
        Mark::Working => 3,
    }
}

pub fn inbox(report: &Report) -> Inbox<'_> {
    let tags: HashMap<&str, &str> = report
        .tags
        .iter()
        .map(|t| (t.source.as_str(), t.tag.as_str()))
        .collect();
    let mut by_name: HashMap<String, Vec<&Lead>> = HashMap::new();
    let all = report.needs_you.iter().chain(&report.live).chain(&report.happened);
    for lead in all {
        by_name.entry(lane_name(lead, &tags)).or_default().push(lead);
    }
    let mut teams: Vec<Team<'_>> = by_name
        .into_iter()
        .map(|(name, mut items)| {
            items.sort_by(|a, b| {
                rank(mark(a))
                    .cmp(&rank(mark(b)))
                    .then(b.at.cmp(&a.at))
                    .then(a.key.cmp(&b.key))
            });
            let count = |m| items.iter().filter(|l| mark(l) == m).count();
            Team {
                needs: count(Mark::Asks),
                failed: count(Mark::Failed),
                done: count(Mark::Done),
                working: count(Mark::Working),
                commits: sum_commits(items.iter().copied()),
                gist: gist(&items),
                name,
                items,
            }
        })
        .collect();
    let latest = |t: &Team<'_>| t.items.iter().map(|l| l.at).max().unwrap_or(Timestamp::MIN);
    teams.sort_by(|a, b| {
        (b.needs > 0)
            .cmp(&(a.needs > 0))
            .then((b.failed > 0).cmp(&(a.failed > 0)))
            .then(((b.failed + b.done) > 0).cmp(&((a.failed + a.done) > 0)))
            .then(latest(b).cmp(&latest(a)))
            .then(a.name.cmp(&b.name))
    });
    let mut needs: Vec<&Lead> = teams
        .iter()
        .flat_map(|t| &t.items)
        .copied()
        .filter(|l| mark(l) == Mark::Asks)
        .collect();
    needs.sort_by(|a, b| a.at.cmp(&b.at).then(a.key.cmp(&b.key)));
    Inbox {
        needs,
        to_review: teams.iter().map(|t| to_review(&t.items)).sum(),
        working: teams.iter().map(|t| t.working).sum(),
        commits: sum_commits(teams.iter().flat_map(|t| &t.items).copied()),
        teams,
    }
}

/// How many of `items` wait for your review: queued items and decisions.
pub fn to_review(items: &[&Lead]) -> usize {
    items
        .iter()
        .filter(|l| mark(l) != Mark::Working && (l.rest.is_some() || mark(l) == Mark::Asks))
        .count()
}

/// A team in a line: who asks, what failed or stopped, what finished.
/// Never the message text.
pub fn gist(team_items: &[&Lead]) -> String {
    let of = |m: Mark| team_items.iter().filter(move |l| mark(l) == m);
    let mut clauses: Vec<String> = of(Mark::Asks).map(|l| format!("{} asks", l.title)).collect();
    clauses.extend(of(Mark::Failed).map(|l| format!("{} failed", l.title)));
    let stopped = team_items
        .iter()
        .filter(|l| matches!(mark(l), Mark::Done) && hinted(l, Hint::Stopped));
    clauses.extend(stopped.map(|l| format!("{} stopped", l.title)));
    let done = of(Mark::Done)
        .filter(|l| !hinted(l, Hint::Stopped))
        .max_by_key(|l| l.at);
    if let Some(l) = done {
        let unpushed = changed(l).is_some_and(|c| !c.pushed);
        clauses.push(format!(
            "{} done{}",
            l.title,
            if unpushed { ", not pushed" } else { "" }
        ));
    }
    clauses.truncate(3);
    clauses.join("; ")
}

/// How far back the report's happened band reaches: to when you left, and
/// never less than a day. The panel and `rl report` both ask for this.
pub fn window(left: Option<Timestamp>, now: Timestamp) -> Timestamp {
    let day = now - SignedDuration::from_hours(24);
    left.map_or(day, |l| l.min(day))
}

/// Where the inbox opens: on the first ask when you were away less than
/// `brief_after`, else on all the teams.
pub fn opening(inbox: &Inbox, away: Option<SignedDuration>, brief_after: SignedDuration) -> Sel {
    match (away, inbox.needs.first()) {
        (Some(a), Some(lead)) if a < brief_after => Sel::Item(lead.key.clone()),
        _ => Sel::All,
    }
}

/// What `j`/`k` walk: outside a team, all then each ask then each team;
/// inside one, its items.
pub fn order(inbox: &Inbox, inside: Option<&str>) -> Vec<Sel> {
    if let Some(name) = inside {
        return inbox
            .teams
            .iter()
            .filter(|t| t.name == name)
            .flat_map(|t| &t.items)
            .map(|l| Sel::Item(l.key.clone()))
            .collect();
    }
    let mut out = vec![Sel::All];
    out.extend(inbox.needs.iter().map(|l| Sel::Item(l.key.clone())));
    out.extend(inbox.teams.iter().map(|t| Sel::Team(t.name.clone())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::lead::LeadStatus;
    use rl_proto::queue::{Changed, How, LeadRest, QueueItem, RestKind};

    fn ts(n: i64) -> Timestamp {
        Timestamp::from_second(n).unwrap()
    }

    fn lead(key: &str, band: Band, scope: &str, at: i64) -> Lead {
        Lead {
            source: "run".into(),
            key: key.into(),
            band,
            title: key.into(),
            summary: None,
            scope: Some(scope.into()),
            action: None,
            status: LeadStatus::Open,
            at: ts(at),
            expires_at: None,
            facts: None,
            rest: None,
        }
    }

    fn queued(mut l: Lead, kind: RestKind, hints: &[Hint], changed: Option<Changed>) -> Lead {
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
            last: Some("SECRET WORDS".into()),
            changed,
        });
        l
    }

    fn ask(key: &str, scope: &str, at: i64) -> Lead {
        queued(
            lead(key, Band::NeedsYou, scope, at),
            RestKind::Asks,
            &[Hint::Question],
            None,
        )
    }

    fn done(key: &str, scope: &str, at: i64) -> Lead {
        queued(lead(key, Band::Happened, scope, at), RestKind::Finished, &[], None)
    }

    fn report(needs_you: Vec<Lead>, live: Vec<Lead>, happened: Vec<Lead>) -> Report {
        Report {
            needs_you,
            live,
            happened,
            tags: vec![],
        }
    }

    fn names<'a>(i: &'a Inbox<'a>) -> Vec<&'a str> {
        i.teams.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn teams_sort_by_urgency_then_name() {
        let r = report(
            vec![ask("q", "/p/zed", 10)],
            vec![lead("w", Band::Live, "/p/live", 99)],
            vec![
                queued(
                    lead("f", Band::Happened, "/p/broke", 5),
                    RestKind::Finished,
                    &[Hint::Failed],
                    None,
                ),
                done("d1", "/p/beta", 7),
                done("d2", "/p/alpha", 7),
                done("d3", "/p/newest", 50),
            ],
        );
        let i = inbox(&r);
        assert_eq!(names(&i), ["zed", "broke", "newest", "alpha", "beta", "live"]);
        assert_eq!((i.to_review, i.working), (5, 1));
    }

    #[test]
    fn needs_strip_lists_asks_across_teams_oldest_first() {
        let r = report(
            vec![ask("new", "/p/a", 30), ask("old", "/p/b", 10), ask("mid", "/p/a", 20)],
            vec![],
            vec![],
        );
        let i = inbox(&r);
        let keys: Vec<&str> = i.needs.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["old", "mid", "new"]);
        let a = i.teams.iter().find(|t| t.name == "a").unwrap();
        assert_eq!(a.items[0].key, "new");
    }

    #[test]
    fn a_decision_without_a_rest_is_an_ask() {
        let r = report(vec![lead("decision:1", Band::NeedsYou, "/p/a", 1)], vec![], vec![]);
        let i = inbox(&r);
        assert_eq!(i.needs.len(), 1);
        assert_eq!(i.teams[0].needs, 1);
        assert_eq!(i.to_review, 1);
    }

    #[test]
    fn a_firing_without_a_rest_stays_as_a_done_item() {
        let r = report(
            vec![],
            vec![],
            vec![lead("flow", Band::Happened, "/p/a", 1), done("d", "/p/b", 2)],
        );
        let i = inbox(&r);
        assert_eq!(names(&i), ["b", "a"]);
        assert_eq!((i.teams[1].done, i.teams[1].gist.as_str()), (1, "flow done"));
        // only the queued item waits for review
        assert_eq!(i.to_review, 1);
    }

    #[test]
    fn gist_never_uses_the_message() {
        let r = report(vec![ask("q", "/p/a", 1)], vec![], vec![done("d", "/p/a", 2)]);
        let i = inbox(&r);
        assert_eq!(i.teams[0].gist, "q asks; d done");
        assert!(!i.teams[0].gist.contains("SECRET"));
    }

    #[test]
    fn gist_says_not_pushed_only_with_git() {
        let c = Changed {
            commits: 2,
            pushed: false,
            ..Changed::default()
        };
        let with = queued(
            lead("migrate auth", Band::Happened, "/p/a", 1),
            RestKind::Finished,
            &[],
            Some(c),
        );
        assert_eq!(gist(&[&with]), "migrate auth done, not pushed");
        let without = done("migrate auth", "/p/a", 1);
        assert_eq!(gist(&[&without]), "migrate auth done");
    }

    #[test]
    fn gist_keeps_three_clauses_and_names_failed_and_stopped() {
        let failed = queued(
            lead("f", Band::Happened, "/p/a", 1),
            RestKind::Finished,
            &[Hint::Failed],
            None,
        );
        let stopped = queued(
            lead("s", Band::Happened, "/p/a", 2),
            RestKind::Finished,
            &[Hint::Stopped],
            None,
        );
        let (a1, a2) = (ask("a1", "/p/a", 3), ask("a2", "/p/a", 4));
        let d = done("d", "/p/a", 5);
        assert_eq!(gist(&[&a1, &a2, &failed, &stopped, &d]), "a1 asks; a2 asks; f failed");
        assert_eq!(gist(&[&stopped, &d]), "s stopped; d done");
    }

    #[test]
    fn commits_are_none_without_any_repo() {
        let r = report(vec![ask("q", "/p/a", 1)], vec![], vec![done("d", "/p/a", 2)]);
        let i = inbox(&r);
        assert_eq!(i.commits, None);
        assert_eq!(i.teams[0].commits, None);
        let c = |n| {
            Some(Changed {
                commits: n,
                ..Changed::default()
            })
        };
        let r = report(
            vec![],
            vec![],
            vec![
                queued(lead("x", Band::Happened, "/p/a", 1), RestKind::Finished, &[], c(2)),
                queued(lead("y", Band::Happened, "/p/b", 1), RestKind::Finished, &[], c(3)),
                done("z", "/p/b", 1),
            ],
        );
        let i = inbox(&r);
        assert_eq!(i.commits, Some(5));
    }

    #[test]
    fn the_window_reaches_back_to_when_you_left_and_never_less_than_a_day() {
        let now = ts(10 * 86_400);
        let day = ts(9 * 86_400);
        assert_eq!(window(None, now), day);
        assert_eq!(window(Some(ts(10 * 86_400 - 600)), now), day, "back soon: a day");
        assert_eq!(
            window(Some(ts(7 * 86_400)), now),
            ts(7 * 86_400),
            "three days: all of them"
        );
    }

    #[test]
    fn opening_picks_the_first_ask_when_back_soon() {
        let r = report(vec![ask("old", "/p/a", 1), ask("new", "/p/a", 2)], vec![], vec![]);
        let i = inbox(&r);
        let after = SignedDuration::from_hours(2);
        assert_eq!(
            opening(&i, Some(SignedDuration::from_mins(10)), after),
            Sel::Item("old".into())
        );
        let only = report(vec![], vec![], vec![done("d", "/p/a", 1)]);
        let none = inbox(&only);
        assert_eq!(opening(&none, Some(SignedDuration::from_mins(10)), after), Sel::All);
    }

    #[test]
    fn opening_is_all_teams_after_long_away() {
        let r = report(vec![ask("q", "/p/a", 1)], vec![], vec![]);
        let i = inbox(&r);
        let after = SignedDuration::from_hours(2);
        assert_eq!(opening(&i, Some(SignedDuration::from_hours(5)), after), Sel::All);
        assert_eq!(opening(&i, None, after), Sel::All);
    }

    #[test]
    fn order_walks_all_asks_teams_and_inside_a_team_its_items() {
        let r = report(
            vec![ask("q", "/p/a", 1)],
            vec![],
            vec![done("d", "/p/a", 2), done("e", "/p/b", 3)],
        );
        let i = inbox(&r);
        assert_eq!(
            order(&i, None),
            [
                Sel::All,
                Sel::Item("q".into()),
                Sel::Team("a".into()),
                Sel::Team("b".into())
            ]
        );
        assert_eq!(order(&i, Some("a")), [Sel::Item("q".into()), Sel::Item("d".into())]);
    }
}
