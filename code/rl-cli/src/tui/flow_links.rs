//! Which flows start which, worked out from every loaded flow's definition and its last runs'
//! signals, with no ledger: what one flow's firings raise, heard by another's `on` and not
//! ruled out by its `where`. Pure.

use rl_flow::def::{Does, FlowDef, kind_matches};
use rl_proto::flow::{Flow, FlowStatus};
use rl_proto::methods::DigestItem;
use rl_proto::types::{Cause, event_kind as k};
use std::collections::BTreeSet;

/// What a step's run raises, with the step's flow as its cause.
const RUN_KINDS: &[&str] = &[
    k::RUN_STARTED,
    k::RUN_STATE,
    k::RUN_NEEDS_INPUT,
    k::RUN_BLOCKED,
    k::RUN_RESUMED,
    k::RUN_FINISHED,
    k::RUN_FAILED,
    k::RUN_STOPPED,
    k::RUN_TROUBLE,
    k::RUN_LOST,
    k::RUN_SUMMARISED,
];

/// What an `ask` step's question raises.
const DECISION_KINDS: &[&str] = &[
    k::DECISION_RAISED,
    k::DECISION_TAKEN,
    k::DECISION_DISMISSED,
    k::DECISION_EXPIRED,
];

/// A loaded flow as its wiring sees it: its definition, and the kinds its last runs raised.
#[derive(Debug, Clone)]
pub struct Wired {
    pub name: String,
    pub def: FlowDef,
    /// The kinds of what its steps' runs on the list raised (`rl emit`, `$RELAY_OUT`).
    pub emitted: BTreeSet<String>,
}

/// Every flow loaded and not removed whose definition reads, each with what its runs among
/// `items` raised.
pub fn wired(flows: &[Flow], items: &[DigestItem]) -> Vec<Wired> {
    flows
        .iter()
        .filter(|f| f.status() != FlowStatus::Removed)
        .filter_map(|f| {
            let def: FlowDef = serde_json::from_value(f.def.clone()).ok()?;
            let emitted = items
                .iter()
                .filter(|i| matches!(&i.run.cause, Cause::Flow { flow, .. } if *flow == f.name))
                .flat_map(|i| i.emitted.iter().map(|s| s.kind.clone()))
                .collect();
            Some(Wired {
                name: f.name.clone(),
                def,
                emitted,
            })
        })
        .collect()
}

/// What `a`'s firings raise, each with whether only a `where` naming `a` makes it a link: a
/// run's kinds would be heard from any flow's runs, so they count only then.
fn raises(a: &Wired) -> Vec<(&str, bool)> {
    let asks = a.def.steps.iter().any(|s| matches!(s.does, Does::Ask(_)));
    let runs = RUN_KINDS.iter().map(|k| (*k, true));
    let decisions = DECISION_KINDS.iter().filter(|_| asks).map(|k| (*k, false));
    let emitted = a.emitted.iter().map(|k| (k.as_str(), false));
    runs.chain(decisions).chain(emitted).collect()
}

/// The patterns of `b`'s `on` that `a`'s firings would start it on, as written. None when they
/// are one flow — a flow does not hear its own doing — or `b`'s `where.cause` rules `a` out.
/// The rest of a `where` cannot be known ahead, so it rules nothing out.
pub fn heard(a: &Wired, b: &Wired) -> Vec<String> {
    if a.name == b.name {
        return Vec::new();
    }
    let cause = b.def.trigger.filter.as_ref().and_then(|w| w.cause.as_deref());
    let step = format!("flow:{}/", a.name);
    let names_a = cause.is_some_and(|c| c == format!("flow:{}", a.name) || c.starts_with(&step));
    if !names_a && cause.is_some_and(|c| !step.starts_with(c)) {
        return Vec::new();
    }
    let raised = raises(a);
    b.def
        .trigger
        .on
        .iter()
        .filter(|p| {
            raised
                .iter()
                .any(|(kind, needs_naming)| kind_matches(p, kind) && (names_a || !needs_naming))
        })
        .cloned()
        .collect()
}

/// A flow's wiring among the others: the flows that start it, and those it starts with the
/// patterns they start on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    pub by: Vec<String>,
    pub starts: Vec<(String, Vec<String>)>,
}

/// `name`'s links among `all`, each side in the order the flows came.
pub fn links(name: &str, all: &[Wired]) -> Links {
    let Some(me) = all.iter().find(|w| w.name == name) else {
        return Links::default();
    };
    Links {
        by: all
            .iter()
            .filter(|a| !heard(a, me).is_empty())
            .map(|a| a.name.clone())
            .collect(),
        starts: all
            .iter()
            .filter_map(|b| {
                let on = heard(me, b);
                (!on.is_empty()).then(|| (b.name.clone(), on))
            })
            .collect(),
    }
}

/// The flows started, in words: `triage (on decision.taken) · page (on run.failed)`.
pub fn starts_words(starts: &[(String, Vec<String>)]) -> String {
    starts
        .iter()
        .map(|(name, on)| format!("{name} (on {})", on.join(", ")))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::Filter;
    use rl_proto::types::Signal;

    fn def(name: &str, on: &[&str], cause: Option<&str>, ask: bool) -> FlowDef {
        let does = if ask {
            serde_json::json!({ "Ask": { "text": "ok?", "options": [], "default": null,
                                         "expires": null, "group": null, "multi": false } })
        } else {
            serde_json::json!({ "Run": { "run": "true" } })
        };
        let mut d: FlowDef = serde_json::from_value(serde_json::json!({
            "name": name,
            "in_": null,
            "trigger": { "when": null, "on": on, "filter": null },
            "missed": "skip",
            "overlap": "skip",
            "throttle_secs": 0,
            "steps": [{ "name": "a", "after": [], "if_": null, "in_": null, "does": does }]
        }))
        .unwrap();
        d.trigger.filter = cause.map(|c| Filter {
            cause: Some(c.into()),
            ..Default::default()
        });
        d
    }

    fn w(name: &str, on: &[&str], cause: Option<&str>) -> Wired {
        Wired {
            name: name.into(),
            def: def(name, on, cause, false),
            emitted: BTreeSet::new(),
        }
    }

    #[test]
    fn a_flow_whose_where_names_another_is_started_by_its_runs() {
        let all = [w("a", &[], None), w("b", &["run.failed"], Some("flow:a"))];
        assert_eq!(links("b", &all).by, ["a"]);
        let a = links("a", &all);
        assert_eq!(a.starts, [("b".to_string(), vec!["run.failed".to_string()])]);
        assert_eq!(starts_words(&a.starts), "b (on run.failed)");
        // A step of it, named, is it too; `flow:` alone names no flow.
        let all = [w("a", &[], None), w("b", &["run.*"], Some("flow:a/fetch"))];
        assert_eq!(links("b", &all).by, ["a"]);
        let all = [w("a", &[], None), w("b", &["run.failed"], Some("flow:"))];
        assert!(links("b", &all).by.is_empty());
        // Another flow's cause rules it out, a longer name too.
        let all = [w("a", &["deploy.done"], None), w("b", &["run.failed"], Some("flow:ab"))];
        assert!(links("a", &all).starts.is_empty());
    }

    #[test]
    fn an_unfiltered_run_pattern_is_not_listed_under_every_flow() {
        let all = [w("a", &[], None), w("b", &["run.failed"], None)];
        assert!(links("a", &all).starts.is_empty());
        assert!(links("b", &all).by.is_empty());
    }

    #[test]
    fn what_a_flows_runs_emitted_and_its_questions_start_others() {
        let mut a = w("a", &[], None);
        a.emitted.insert("deploy.done".into());
        let all = [a, w("b", &["deploy.done"], None), w("c", &["decision.taken"], None)];
        assert_eq!(
            links("a", &all).starts,
            [("b".to_string(), vec!["deploy.done".to_string()])]
        );
        let mut asks = w("a", &[], None);
        asks.def = def("a", &[], None, true);
        let all = [asks, w("c", &["decision.*", "run.failed"], None)];
        assert_eq!(starts_words(&links("a", &all).starts), "c (on decision.*)");
        // A `where` on another flow's cause still rules it out.
        let mut a = w("a", &[], None);
        a.emitted.insert("deploy.done".into());
        let all = [a, w("b", &["deploy.done"], Some("flow:other"))];
        assert!(links("a", &all).starts.is_empty());
    }

    #[test]
    fn a_flow_never_starts_itself() {
        let mut a = w("a", &["deploy.done", "run.failed"], Some("flow:a"));
        a.emitted.insert("deploy.done".into());
        let all = [a];
        assert_eq!(links("a", &all), Links::default());
    }

    #[test]
    fn what_a_flow_emitted_is_read_from_its_own_runs() {
        use jiff::Timestamp;
        use rl_proto::types::{Run, RunKind};
        let at: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let item = |cause: Cause, kind: &str| {
            let mut run = Run::new("r1", RunKind::Command, "x", vec![], "/w", "/w", at);
            run.cause = cause;
            DigestItem {
                run,
                summary: String::new(),
                at,
                unseen: false,
                joined_by: None,
                emitted: vec![Signal {
                    kind: kind.into(),
                    count: 1,
                    said: String::new(),
                    at,
                }],
                cwd: None,
                recap: None,
            }
        };
        let flow = |name: &str| Flow {
            name: name.into(),
            hash: "h".into(),
            folder: None,
            def: serde_json::to_value(def(name, &[], None, false)).unwrap(),
            source: "/h/flows/x".into(),
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        };
        let items = [
            item(
                Cause::Flow {
                    flow: "a".into(),
                    step: "s".into(),
                },
                "deploy.done",
            ),
            item(Cause::Manual, "other.thing"),
        ];
        let all = wired(&[flow("a"), flow("b")], &items);
        assert_eq!(all[0].emitted, BTreeSet::from(["deploy.done".to_string()]));
        assert!(all[1].emitted.is_empty());
    }
}
