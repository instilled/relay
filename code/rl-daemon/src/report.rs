//! Projecting Relay's own state into leads, and assembling the report. Pure
//! functions over data the handler reads; no ledger, no I/O, so they test flat.

use jiff::Timestamp;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::Firing;
use rl_proto::lead::{Band, Lead, LeadStatus};
use rl_proto::methods::Report;
use rl_proto::types::Run;
use std::path::Path;

/// The source every built-in lead carries. Reserved — user leads may not use it.
pub const BUILTIN_SOURCE: &str = "relay";

/// Relay's own state as leads: open decisions, runs by state, flow firings.
/// Computed, never stored, so it tracks the truth and self-clears.
pub fn project_builtins(decisions: &[Decision], runs: &[Run], firings: &[Firing]) -> Vec<Lead> {
    let mut out = Vec::new();

    for d in decisions.iter().filter(|d| d.status == DecisionStatus::Open) {
        out.push(Lead {
            source: BUILTIN_SOURCE.into(),
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
        out.push(Lead {
            source: BUILTIN_SOURCE.into(),
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
            source: BUILTIN_SOURCE.into(),
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
/// shown whatever their age; happened is bounded by `since`. Anything expired or
/// out of scope is dropped. Each band is newest-first.
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
        let leads = project_builtins(&[d], &[], &[]);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        assert_eq!(leads[0].source, "relay");
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
            let leads = project_builtins(&[], &[r], &[]);
            assert_eq!(leads.len(), 1, "{state:?} should project one lead");
            assert_eq!(leads[0].band, want, "{state:?}");
            assert_eq!(leads[0].key, "run:r1");
        }
    }

    #[test]
    fn resting_shell_is_not_projected() {
        let shell = test_shell("sh1", RunState::Idle);
        assert!(
            project_builtins(&[], &[shell], &[]).is_empty(),
            "an idle shell is not news"
        );
    }

    #[test]
    fn firing_band_by_whether_it_ended() {
        let live = test_firing("f1", None);
        let done = test_firing("f2", Some(Timestamp::now()));
        assert_eq!(project_builtins(&[], &[], &[live])[0].band, Band::Live);
        assert_eq!(project_builtins(&[], &[], &[done])[0].band, Band::Happened);
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
