//! Projecting Relay's own state into leads, and assembling the report. Pure
//! functions over data the handler reads; no ledger, no I/O, so they test flat.

use jiff::Timestamp;
use rl_proto::RunFacts;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::Firing;
use rl_proto::lead::{Band, Lead, LeadStatus};
use rl_proto::methods::{Dismissal, Report};
use rl_proto::queue::{LeadRest, QueueItem, RestKind};
use rl_proto::types::{Cause, Run, RunKind, RunState};
use std::collections::HashMap;
use std::path::Path;

/// Built-in leads that belong to no co-worker sit here until tagged.
pub const MY_WORK: &str = "my work";

/// The co-worker a run lead belongs to: its detected CLI, else its kind.
fn run_source(r: &Run) -> String {
    r.cli.clone().unwrap_or_else(|| match r.kind {
        RunKind::Shell => "shell".into(),
        RunKind::Command => "command".into(),
        RunKind::Plugin => "plugin".into(),
        RunKind::Agent => "agent".into(),
    })
}

/// Relay's own state as leads: open decisions, runs by state, flow firings.
/// Computed, never stored, so it tracks the truth and self-clears. `recaps`
/// is each run's recap line (written, else the agent's own): a happened run
/// is told by it before its raw output tail. `queue` is each queued run's
/// item, by its id: it bands the run and rides on its lead.
pub fn project_builtins(
    decisions: &[Decision],
    runs: &[Run],
    firings: &[Firing],
    facts: &HashMap<String, RunFacts>,
    recaps: &HashMap<String, String>,
    queue: &HashMap<String, QueueItem>,
) -> Vec<Lead> {
    let mut out = Vec::new();

    for d in decisions.iter().filter(|d| d.status == DecisionStatus::Open) {
        out.push(Lead {
            source: MY_WORK.into(),
            key: format!("decision:{}", d.id),
            band: Band::NeedsYou,
            title: d.title.clone(),
            summary: d.summary.clone(),
            scope: Some(d.workspace.clone()),
            action: Some(format!("rl decide {}", d.id)),
            status: LeadStatus::Open,
            at: d.created_at,
            expires_at: d.expires_at,
            facts: None,
            rest: None,
        });
    }

    for r in runs {
        // The assistant's own runs (a recap being written) are Relay's work
        // on your runs, not news of their own.
        if matches!(r.cause, Cause::Assistant(_)) {
            continue;
        }
        // A run at work is live and one waiting on you needs you, by its
        // state: the band does not wait for the queue. Past that the queue
        // says what is news; a run at rest or ended with nothing queued has
        // been reviewed. A run that carried on keeps its item until its next
        // rest.
        let item = queue.get(&r.id);
        let band = match (r.state, item.map(|i| i.kind)) {
            (RunState::Working | RunState::Starting, _) => Band::Live,
            (RunState::NeedsInput | RunState::Blocked, _) | (_, Some(RestKind::Asks)) => Band::NeedsYou,
            (_, Some(RestKind::Finished)) => Band::Happened,
            (_, None) => continue,
        };
        // The ladder past a written recap: the agent's own, then the tail.
        let summary = match band {
            Band::Happened => recaps.get(&r.id).cloned().or_else(|| r.last_line.clone()),
            _ => r.last_line.clone(),
        };
        out.push(Lead {
            source: run_source(r),
            key: format!("run:{}", r.id),
            band,
            title: r.title.clone(),
            summary,
            scope: Some(r.workspace.clone()),
            action: None, // acting on a run lead is step 2
            status: LeadStatus::Open,
            at: item.map_or(r.state_at, |i| i.at),
            expires_at: None,
            facts: facts.get(&r.id).cloned(),
            rest: item.map(|i| LeadRest {
                item: i.clone(),
                last: None,
                changed: None,
            }),
        });
    }

    for f in firings {
        let (band, at) = match f.ended_at {
            Some(end) => (Band::Happened, end),
            None => (Band::Live, f.started_at),
        };
        out.push(Lead {
            source: f.flow.clone(),
            key: format!("firing:{}", f.id),
            band,
            title: format!("flow {}", f.flow),
            summary: f.outcome.clone(),
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
            facts: None,
            rest: None,
        });
    }

    out
}

/// Merge built-in and stored leads into the three bands. needs-you and live are
/// shown whatever their age; happened is bounded by `since`, but for a queued
/// run, which waits until it is reviewed. Anything expired or out of scope is
/// dropped. Each band is newest-first.
pub fn assemble(
    builtins: Vec<Lead>,
    stored: Vec<Lead>,
    scope: Option<&Path>,
    since: Timestamp,
    now: Timestamp,
) -> Report {
    let mut report = Report::default();
    for lead in builtins.into_iter().chain(stored) {
        if lead.expires_at.is_some_and(|e| e <= now) || lead.status == LeadStatus::Dismissed {
            continue;
        }
        if let Some(s) = scope {
            match &lead.scope {
                Some(p) if p.starts_with(s) => {}
                _ => continue,
            }
        }
        match lead.band {
            Band::NeedsYou => report.needs_you.push(lead),
            Band::Live => report.live.push(lead),
            Band::Happened if lead.at >= since || lead.rest.is_some() => report.happened.push(lead),
            Band::Happened => {}
        }
    }
    for band in [&mut report.needs_you, &mut report.live, &mut report.happened] {
        band.sort_by_key(|l| std::cmp::Reverse(l.at));
    }
    report
}

/// Drops every lead the user dismissed, by (source, key), from all three bands.
/// Works on built-in leads too, which are never stored.
pub fn drop_dismissed(report: &mut Report, dismissed: &[Dismissal]) {
    let hidden: std::collections::HashSet<(&str, &str)> =
        dismissed.iter().map(|d| (d.source.as_str(), d.key.as_str())).collect();
    let keep = |l: &Lead| !hidden.contains(&(l.source.as_str(), l.key.as_str()));
    report.needs_you.retain(keep);
    report.live.retain(keep);
    report.happened.retain(keep);
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::flow::FiringStatus;
    use rl_proto::queue::How;
    use rl_proto::types::{RunKind, RunState};

    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn run_in_state(id: &str, state: RunState, state_at: Timestamp) -> Run {
        let mut r = test_run(id, state);
        r.state_at = state_at;
        r
    }

    fn open_decision(id: &str, created_at: Timestamp) -> Decision {
        let mut d = test_decision(id, "old question");
        d.created_at = created_at;
        d
    }

    /// The queue as the handler reads it: one item per run, by its id.
    fn queued(items: &[(&str, RestKind)]) -> HashMap<String, QueueItem> {
        items
            .iter()
            .map(|(id, kind)| {
                let item = QueueItem {
                    source: "run".into(),
                    reference: id.to_string(),
                    kind: *kind,
                    how: How::Reply,
                    hints: Vec::new(),
                    at: ts("2026-10-18T00:00:00Z"),
                    rests: 1,
                    from: ts("2026-10-17T00:00:00Z"),
                    from_head: None,
                };
                (id.to_string(), item)
            })
            .collect()
    }

    #[test]
    fn a_happened_run_lead_carries_its_facts() {
        let with = test_run("r1", RunState::Finished);
        let without = test_run("r2", RunState::Finished);
        let mut facts = HashMap::new();
        facts.insert(
            "r1".to_string(),
            RunFacts {
                run: "r1".into(),
                commits: 3,
                ..Default::default()
            },
        );
        let leads = project_builtins(
            &[],
            &[with, without],
            &[],
            &facts,
            &Default::default(),
            &queued(&[("r1", RestKind::Finished), ("r2", RestKind::Finished)]),
        );
        let find = |k: &str| leads.iter().find(|l| l.key == k).unwrap();
        assert_eq!(find("run:r1").facts.as_ref().unwrap().commits, 3);
        assert!(find("run:r2").facts.is_none());
    }

    #[test]
    fn the_assistants_own_runs_are_not_leads() {
        let mut summariser = test_run("a1", RunState::Finished);
        summariser.cause = Cause::Assistant("summary".into());
        summariser.title = "summary: fix".into();
        let mine = test_run("r1", RunState::Finished);
        let leads = project_builtins(
            &[],
            &[summariser, mine],
            &[],
            &Default::default(),
            &Default::default(),
            &queued(&[("a1", RestKind::Finished), ("r1", RestKind::Finished)]),
        );
        let keys: Vec<&str> = leads.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["run:r1"]);
    }

    #[test]
    fn a_happened_run_is_told_by_its_recap_before_its_tail() {
        let mut seeded = test_run("r1", RunState::Finished);
        seeded.last_line = Some("$ cargo test".into());
        let mut bare = test_run("r2", RunState::Finished);
        bare.last_line = Some("done.".into());
        let mut live = test_run("r3", RunState::Working);
        live.last_line = Some("compiling".into());
        let recaps: HashMap<String, String> = [
            ("r1".to_string(), "Fixed the search.".to_string()),
            ("r3".to_string(), "Halfway.".to_string()),
        ]
        .into();
        let leads = project_builtins(
            &[],
            &[seeded, bare, live],
            &[],
            &Default::default(),
            &recaps,
            &queued(&[("r1", RestKind::Finished), ("r2", RestKind::Finished)]),
        );
        let find = |k: &str| leads.iter().find(|l| l.key == k).unwrap();
        assert_eq!(find("run:r1").summary.as_deref(), Some("Fixed the search."));
        assert_eq!(find("run:r2").summary.as_deref(), Some("done."), "no recap: the tail");
        assert_eq!(
            find("run:r3").summary.as_deref(),
            Some("compiling"),
            "a live run: its tail"
        );
    }

    #[test]
    fn an_asks_item_puts_the_run_in_needs_you() {
        let idle = test_run("r1", RunState::Idle);
        let queue = queued(&[("r1", RestKind::Asks)]);
        let leads = project_builtins(&[], &[idle], &[], &Default::default(), &Default::default(), &queue);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        let rest = leads[0].rest.as_ref().unwrap();
        assert_eq!(rest.item, queue["r1"]);
        assert_eq!(leads[0].at, queue["r1"].at, "at its rest");
    }

    #[test]
    fn a_finished_item_is_happened_with_its_rest() {
        let ended = test_run("r1", RunState::Finished);
        let failed = test_run("r2", RunState::Failed);
        // A shell at rest is news once it is queued: it finished something.
        let shell = test_shell("sh1", RunState::Idle);
        let queue = queued(&[
            ("r1", RestKind::Finished),
            ("r2", RestKind::Finished),
            ("sh1", RestKind::Finished),
        ]);
        let leads = project_builtins(
            &[],
            &[ended, failed, shell],
            &[],
            &Default::default(),
            &Default::default(),
            &queue,
        );
        assert_eq!(leads.len(), 3);
        for l in &leads {
            assert_eq!(l.band, Band::Happened, "{}", l.key);
            let id = l.key.strip_prefix("run:").unwrap();
            assert_eq!(l.rest.as_ref().unwrap().item, queue[id]);
        }
    }

    #[test]
    fn a_reviewed_finished_run_leaves_the_report() {
        // Nothing queued for them: each was reviewed, or never came to rest.
        let runs = [
            test_run("r1", RunState::Finished),
            test_run("r2", RunState::Failed),
            test_run("r3", RunState::Stopped),
            test_run("r4", RunState::Idle),
            test_run("r5", RunState::Stale),
        ];
        let leads = project_builtins(
            &[],
            &runs,
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert!(leads.is_empty(), "{leads:?}");
    }

    #[test]
    fn a_waiting_run_needs_you_before_it_is_queued() {
        // Inside the screen's grace there is no item yet: its state says.
        for state in [RunState::NeedsInput, RunState::Blocked] {
            let leads = project_builtins(
                &[],
                &[test_run("r1", state)],
                &[],
                &Default::default(),
                &Default::default(),
                &Default::default(),
            );
            assert_eq!(leads.len(), 1, "{state:?}");
            assert_eq!(leads[0].band, Band::NeedsYou, "{state:?}");
            assert!(leads[0].rest.is_none());
        }
    }

    #[test]
    fn a_queued_happened_lead_is_not_bounded_by_since() {
        // An item waits until it is reviewed, however long ago it rested.
        let now = ts("2026-10-18T00:00:00Z");
        let mut queue = queued(&[("r1", RestKind::Finished)]);
        queue.get_mut("r1").unwrap().at = now - Duration::from_secs(3 * 24 * 3600);
        let leads = project_builtins(
            &[],
            &[test_run("r1", RunState::Finished)],
            &[],
            &Default::default(),
            &Default::default(),
            &queue,
        );
        let r = assemble(leads, vec![], None, now - Duration::from_secs(3600), now);
        assert_eq!(r.happened.len(), 1);
    }

    #[test]
    fn an_old_asks_item_is_never_dropped() {
        // The vacation case: something waiting on a human must not vanish
        // from the report however long you were away.
        let now = ts("2026-10-18T00:00:00Z");
        let long_ago = now - Duration::from_secs(60 * 24 * 3600);
        let blocked = run_in_state("r1", RunState::Blocked, long_ago);
        let mut queue = queued(&[("r1", RestKind::Asks)]);
        queue.get_mut("r1").unwrap().at = long_ago;
        let leads = project_builtins(&[], &[blocked], &[], &Default::default(), &Default::default(), &queue);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        let r = assemble(leads, vec![], None, now - Duration::from_secs(3600), now);
        assert_eq!(r.needs_you.len(), 1);
    }

    #[test]
    fn aging_never_drops_decisions_or_firings() {
        let now = ts("2026-10-18T00:00:00Z");
        let old = now - Duration::from_secs(60 * 24 * 3600);
        let d = open_decision("d1", old);
        let f = test_firing("f1", Some(old)); // an old, ended firing
        let leads = project_builtins(
            &[d],
            &[],
            &[f],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(
            leads.len(),
            2,
            "an old open decision and an old firing are never aged out"
        );
    }

    #[test]
    fn assemble_drops_dismissed_leads() {
        let now = Timestamp::now();
        let mut l = mk(Band::NeedsYou, "triage", now, None);
        l.status = LeadStatus::Dismissed;
        let report = assemble(vec![], vec![l], None, now - std::time::Duration::from_secs(3600), now);
        assert!(report.needs_you.is_empty());
    }

    #[test]
    fn drop_dismissed_removes_matching_leads_from_all_bands() {
        let now = Timestamp::now();
        let mut builtin = mk(Band::Live, "run:1", now, None);
        builtin.source = "claude".into();
        let mut stored = mk(Band::NeedsYou, "triage", now, None);
        stored.source = "feedback".into();
        let mut happened = mk(Band::Happened, "run:2", now, None);
        happened.source = "claude".into();
        let keeper = mk(Band::NeedsYou, "keep", now, None);
        let mut report = assemble(
            vec![builtin, happened],
            vec![stored, keeper],
            None,
            now - std::time::Duration::from_secs(3600),
            now,
        );
        let dismissed = |s: &str, k: &str| Dismissal {
            source: s.into(),
            key: k.into(),
        };
        drop_dismissed(
            &mut report,
            &[
                dismissed("claude", "run:1"),
                dismissed("feedback", "triage"),
                dismissed("claude", "run:2"),
            ],
        );
        let all: Vec<_> = report
            .needs_you
            .iter()
            .chain(&report.live)
            .chain(&report.happened)
            .map(|l| (l.source.as_str(), l.key.as_str()))
            .collect();
        assert_eq!(all, vec![("relay", "keep")]);
    }

    fn mk(band: Band, key: &str, at: Timestamp, scope: Option<&str>) -> Lead {
        Lead {
            source: "relay".into(),
            key: key.into(),
            band,
            title: key.into(),
            summary: None,
            scope: scope.map(PathBuf::from),
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
            facts: None,
            rest: None,
        }
    }

    #[test]
    fn since_bounds_only_happened() {
        let now = Timestamp::now();
        let old = now - std::time::Duration::from_secs(3600);
        let builtins = vec![
            mk(Band::NeedsYou, "decision:old", old, None),
            mk(Band::Live, "run:old", old, None),
            mk(Band::Happened, "run:recent", now, None),
            mk(Band::Happened, "run:stale", old, None),
        ];
        let since = now - std::time::Duration::from_secs(600);
        let r = assemble(builtins, vec![], None, since, now);
        assert_eq!(r.needs_you.len(), 1);
        assert_eq!(r.live.len(), 1);
        assert_eq!(
            r.happened.len(),
            1,
            "old happened is bounded out, needs-you/live are not"
        );
        assert_eq!(r.happened[0].key, "run:recent");
    }

    #[test]
    fn expired_dropped_defensively() {
        let now = Timestamp::now();
        let mut l = mk(Band::NeedsYou, "x", now, None);
        l.expires_at = Some(now - std::time::Duration::from_secs(1));
        let r = assemble(vec![], vec![l], None, now - std::time::Duration::from_secs(10), now);
        assert!(r.needs_you.is_empty());
    }

    #[test]
    fn scope_filters_by_path_prefix() {
        let now = Timestamp::now();
        let a = mk(Band::Live, "run:a", now, Some("/home/me/proj-a"));
        let b = mk(Band::Live, "run:b", now, Some("/home/me/proj-b"));
        let r = assemble(vec![a, b], vec![], Some(Path::new("/home/me/proj-a")), now, now);
        assert_eq!(r.live.len(), 1);
        assert_eq!(r.live[0].key, "run:a");
    }

    #[test]
    fn bands_sorted_newest_first_within() {
        let now = Timestamp::now();
        let older = now - std::time::Duration::from_secs(100);
        let r = assemble(
            vec![
                mk(Band::Happened, "old", older, None),
                mk(Band::Happened, "new", now, None),
            ],
            vec![],
            None,
            older - std::time::Duration::from_secs(10),
            now,
        );
        assert_eq!(r.happened[0].key, "new", "newest first");
    }

    #[test]
    fn open_decision_is_needs_you_with_decide_action() {
        let d = test_decision("d1", "approve the task?");
        let leads = project_builtins(
            &[d],
            &[],
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        assert_eq!(leads[0].source, MY_WORK);
        assert_eq!(leads[0].key, "decision:d1");
        assert_eq!(leads[0].action.as_deref(), Some("rl decide d1"));
    }

    #[test]
    fn a_working_run_is_live_whatever_its_item() {
        let cases = [
            (RunState::Working, None, Band::Live),
            (RunState::Starting, None, Band::Live),
            (RunState::Working, Some(RestKind::Finished), Band::Live),
            (RunState::Working, Some(RestKind::Asks), Band::Live),
        ];
        for (state, kind, want) in cases {
            let queue = kind.map(|k| queued(&[("r1", k)])).unwrap_or_default();
            let leads = project_builtins(
                &[],
                &[test_run("r1", state)],
                &[],
                &Default::default(),
                &Default::default(),
                &queue,
            );
            assert_eq!(leads.len(), 1, "{state:?} {kind:?} should project one lead");
            assert_eq!(leads[0].band, want, "{state:?} {kind:?}");
            assert_eq!(leads[0].rest.is_some(), kind.is_some());
        }
    }

    #[test]
    fn resting_shell_is_not_projected() {
        let shell = test_shell("sh1", RunState::Idle);
        assert!(
            project_builtins(
                &[],
                &[shell],
                &[],
                &Default::default(),
                &Default::default(),
                &Default::default()
            )
            .is_empty(),
            "an idle shell is not news"
        );
    }

    #[test]
    fn firing_band_by_whether_it_ended() {
        let live = test_firing("f1", None);
        let done = test_firing("f2", Some(Timestamp::now()));
        assert_eq!(
            project_builtins(
                &[],
                &[],
                &[live],
                &Default::default(),
                &Default::default(),
                &Default::default()
            )[0]
            .band,
            Band::Live
        );
        assert_eq!(
            project_builtins(
                &[],
                &[],
                &[done],
                &Default::default(),
                &Default::default(),
                &Default::default()
            )[0]
            .band,
            Band::Happened
        );
    }

    #[test]
    fn run_lead_source_is_the_cli() {
        let mut r = test_run("r1", RunState::Working);
        r.cli = Some("claude".into());
        let leads = project_builtins(
            &[],
            &[r],
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(leads[0].source, "claude");
    }

    #[test]
    fn run_without_cli_sources_to_its_kind() {
        let mut r = test_run("r1", RunState::Working);
        r.cli = None;
        r.kind = RunKind::Command;
        let leads = project_builtins(
            &[],
            &[r],
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(leads[0].source, "command");
        let mut a = test_run("r2", RunState::Working);
        a.cli = None;
        assert_eq!(
            project_builtins(
                &[],
                &[a],
                &[],
                &Default::default(),
                &Default::default(),
                &Default::default()
            )[0]
            .source,
            "agent"
        );
        let mut sh = test_shell("r3", RunState::Working);
        sh.cli = None;
        assert_eq!(
            project_builtins(
                &[],
                &[sh],
                &[],
                &Default::default(),
                &Default::default(),
                &Default::default()
            )[0]
            .source,
            "shell"
        );
    }

    #[test]
    fn firing_lead_source_is_its_flow_name() {
        let f = test_firing("f1", None);
        let leads = project_builtins(
            &[],
            &[],
            &[f],
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(leads[0].source, "triage");
    }

    fn test_decision(id: &str, title: &str) -> Decision {
        let now = Timestamp::now();
        Decision {
            id: id.into(),
            workspace: "/w".into(),
            run: None,
            chain: id.into(),
            group: None,
            title: title.into(),
            summary: None,
            options: Vec::new(),
            multi: false,
            status: DecisionStatus::Open,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            chosen: Vec::new(),
            outcome_runs: Vec::new(),
            note: None,
        }
    }

    fn test_run(id: &str, state: RunState) -> Run {
        let mut r = Run::new(
            id,
            RunKind::Agent,
            "a run",
            vec!["claude".into()],
            "/w",
            "/w",
            Timestamp::now(),
        );
        r.state = state;
        r
    }

    fn test_shell(id: &str, state: RunState) -> Run {
        let mut r = test_run(id, state);
        r.kind = RunKind::Shell;
        r
    }

    fn test_firing(id: &str, ended: Option<Timestamp>) -> Firing {
        Firing {
            id: id.into(),
            flow: "triage".into(),
            trigger: "hand".into(),
            event: None,
            status: if ended.is_some() {
                FiringStatus::Done
            } else {
                FiringStatus::Running
            },
            started_at: Timestamp::now(),
            ended_at: ended,
            steps: Vec::new(),
            outcome: None,
        }
    }
}
