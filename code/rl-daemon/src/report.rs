//! Projecting Relay's own state into leads, and assembling the report. Pure
//! functions over data the handler reads; no ledger, no I/O, so they test flat.

use jiff::Timestamp;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::Firing;
use rl_proto::lead::{Band, Lead, LeadStatus};
use rl_proto::methods::Report;
use rl_proto::types::{Run, RunKind, RunState};
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

/// How long a *failed* run stays in needs-you before it ages out of the
/// report. A failure already happened — it is not something you can still act
/// on — so an old one stops being news and drops (it remains in the overview
/// and ledger as history). A genuinely-*waiting* run (Blocked / NeedsInput) is
/// never aged out: it still needs you, however long you were away. Tunable.
const STALE_FAILED: jiff::SignedDuration = jiff::SignedDuration::from_secs(14 * 24 * 3600);

/// Relay's own state as leads: open decisions, runs by state, flow firings.
/// Computed, never stored, so it tracks the truth and self-clears.
pub fn project_builtins(decisions: &[Decision], runs: &[Run], firings: &[Firing], now: Timestamp) -> Vec<Lead> {
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
        });
    }

    for r in runs {
        // A shell at rest is not news — it is idle whenever you are not typing.
        if r.is_shell() && !r.state.needs_attention() && !r.state.is_terminal() {
            continue;
        }
        let band = if r.state.needs_attention() {
            Band::NeedsYou
        } else if r.state.is_terminal() {
            Band::Happened
        } else {
            Band::Live
        };
        // A failure that happened longer ago than STALE_FAILED is no longer
        // news — drop it rather than let old failures pile up in needs-you.
        // Only Failed ages out: a Blocked/NeedsInput run genuinely waiting on a
        // human is kept whatever its age, so nothing you owe an answer to can
        // silently vanish while you are away.
        if band == Band::NeedsYou && r.state == RunState::Failed && now.duration_since(r.state_at) > STALE_FAILED {
            continue;
        }
        out.push(Lead {
            source: run_source(r),
            key: format!("run:{}", r.id),
            band,
            title: r.title.clone(),
            summary: r.last_line.clone(),
            scope: Some(r.workspace.clone()),
            action: None, // acting on a run lead is step 2
            status: LeadStatus::Open,
            at: r.state_at,
            expires_at: None,
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
        });
    }

    out
}

/// Merge built-in and stored leads into the three bands. needs-you and live are
/// shown whatever their age here (project_builtins has already aged old *failed*
/// runs out upstream); happened is bounded by `since`. Anything expired or out of
/// scope is dropped. Each band is newest-first.
pub fn assemble(
    builtins: Vec<Lead>,
    stored: Vec<Lead>,
    scope: Option<&Path>,
    since: Timestamp,
    now: Timestamp,
) -> Report {
    let mut report = Report::default();
    for lead in builtins.into_iter().chain(stored) {
        if lead.expires_at.is_some_and(|e| e <= now) {
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
            Band::Happened if lead.at >= since => report.happened.push(lead),
            Band::Happened => {}
        }
    }
    for band in [&mut report.needs_you, &mut report.live, &mut report.happened] {
        band.sort_by_key(|l| std::cmp::Reverse(l.at));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::flow::FiringStatus;
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

    #[test]
    fn stale_failed_run_is_dropped() {
        let now = ts("2026-10-18T00:00:00Z");
        let old = run_in_state("r1", RunState::Failed, now - Duration::from_secs(15 * 24 * 3600));
        let leads = project_builtins(&[], &[old], &[], now);
        assert!(
            leads.is_empty(),
            "a run that failed 15 days ago is stale history, not news"
        );
    }

    #[test]
    fn recent_failed_run_is_kept() {
        let now = ts("2026-10-18T00:00:00Z");
        let fresh = run_in_state("r1", RunState::Failed, now - Duration::from_secs(60));
        let leads = project_builtins(&[], &[fresh], &[], now);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
    }

    #[test]
    fn failed_run_just_under_cutoff_is_kept() {
        // Pins the 14-day cutoff itself: quietly shrinking it back (e.g. to 7
        // days) would drop this and fail the test.
        let now = ts("2026-10-18T00:00:00Z");
        let r = run_in_state("r1", RunState::Failed, now - Duration::from_secs(13 * 24 * 3600));
        let leads = project_builtins(&[], &[r], &[], now);
        assert_eq!(
            leads.len(),
            1,
            "a run that failed 13 days ago is still within the 14-day window"
        );
    }

    #[test]
    fn stale_waiting_runs_are_never_dropped() {
        // The vacation case: something genuinely waiting on a human must not
        // vanish from the report however long you were away. Only Failed ages.
        let now = ts("2026-10-18T00:00:00Z");
        let long_ago = now - Duration::from_secs(60 * 24 * 3600);
        let blocked = run_in_state("r1", RunState::Blocked, long_ago);
        let needs_input = run_in_state("r2", RunState::NeedsInput, long_ago);
        let leads = project_builtins(&[], &[blocked, needs_input], &[], now);
        assert_eq!(leads.len(), 2, "a 60-day-old Blocked/NeedsInput run still needs you");
        assert!(leads.iter().all(|l| l.band == Band::NeedsYou));
    }

    #[test]
    fn aging_never_drops_decisions_or_firings() {
        let now = ts("2026-10-18T00:00:00Z");
        let old = now - Duration::from_secs(60 * 24 * 3600);
        let d = open_decision("d1", old);
        let f = test_firing("f1", Some(old)); // an old, ended firing
        let leads = project_builtins(&[d], &[], &[f], now);
        assert_eq!(
            leads.len(),
            2,
            "an old open decision and an old firing are never aged out"
        );
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
        let leads = project_builtins(&[d], &[], &[], Timestamp::now());
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        assert_eq!(leads[0].source, MY_WORK);
        assert_eq!(leads[0].key, "decision:d1");
        assert_eq!(leads[0].action.as_deref(), Some("rl decide d1"));
    }

    #[test]
    fn run_state_maps_to_band() {
        let cases = [
            (RunState::NeedsInput, Band::NeedsYou),
            (RunState::Blocked, Band::NeedsYou),
            (RunState::Failed, Band::NeedsYou),
            (RunState::Working, Band::Live),
            (RunState::Starting, Band::Live),
            (RunState::Idle, Band::Live),
            (RunState::Finished, Band::Happened),
            (RunState::Stopped, Band::Happened),
        ];
        for (state, want) in cases {
            let r = test_run("r1", state);
            let leads = project_builtins(&[], &[r], &[], Timestamp::now());
            assert_eq!(leads.len(), 1, "{state:?} should project one lead");
            assert_eq!(leads[0].band, want, "{state:?}");
            assert_eq!(leads[0].key, "run:r1");
        }
    }

    #[test]
    fn resting_shell_is_not_projected() {
        let shell = test_shell("sh1", RunState::Idle);
        assert!(
            project_builtins(&[], &[shell], &[], Timestamp::now()).is_empty(),
            "an idle shell is not news"
        );
    }

    #[test]
    fn firing_band_by_whether_it_ended() {
        let live = test_firing("f1", None);
        let done = test_firing("f2", Some(Timestamp::now()));
        assert_eq!(
            project_builtins(&[], &[], &[live], Timestamp::now())[0].band,
            Band::Live
        );
        assert_eq!(
            project_builtins(&[], &[], &[done], Timestamp::now())[0].band,
            Band::Happened
        );
    }

    #[test]
    fn run_lead_source_is_the_cli() {
        let mut r = test_run("r1", RunState::Working);
        r.cli = Some("claude".into());
        let leads = project_builtins(&[], &[r], &[], Timestamp::now());
        assert_eq!(leads[0].source, "claude");
    }

    #[test]
    fn run_without_cli_sources_to_its_kind() {
        let mut r = test_run("r1", RunState::Working);
        r.cli = None;
        r.kind = RunKind::Command;
        let leads = project_builtins(&[], &[r], &[], Timestamp::now());
        assert_eq!(leads[0].source, "command");
        let mut a = test_run("r2", RunState::Working);
        a.cli = None;
        assert_eq!(project_builtins(&[], &[a], &[], Timestamp::now())[0].source, "agent");
        let mut sh = test_shell("r3", RunState::Failed);
        sh.cli = None;
        assert_eq!(project_builtins(&[], &[sh], &[], Timestamp::now())[0].source, "shell");
    }

    #[test]
    fn firing_lead_source_is_its_flow_name() {
        let f = test_firing("f1", None);
        let leads = project_builtins(&[], &[], &[f], Timestamp::now());
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
