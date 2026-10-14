//! Which flows start which, worked out from every loaded flow's definition and its last runs'
//! signals, with no ledger: each event one flow's firings would raise, put to another's `on` and
//! `where` the way the daemon puts an event that came (`rl_flow::events`). Pure.

use rl_flow::def::{Does, FlowDef};
use rl_flow::events::{self, Offered, OfferedRun};
use rl_proto::flow::{Flow, FlowStatus};
use rl_proto::methods::DigestItem;
use rl_proto::types::{Cause, event_kind};
use std::collections::BTreeSet;

/// A loaded flow as its wiring sees it: its definition, and what its last runs raised.
#[derive(Debug, Clone)]
pub struct Wired {
    pub name: String,
    pub def: FlowDef,
    /// What its steps' runs on the list raised (`rl emit`, `$RELAY_OUT`): by step, the kind.
    pub emitted: BTreeSet<(String, String)>,
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
                .filter_map(|i| match &i.run.cause {
                    Cause::Flow { flow, step } if *flow == f.name => Some((step, &i.emitted)),
                    _ => None,
                })
                .flat_map(|(step, signals)| signals.iter().map(|s| (step.clone(), s.kind.clone())))
                .collect();
            Some(Wired {
                name: f.name.clone(),
                def,
                emitted,
            })
        })
        .collect()
}

/// What makes an event a flow raises a link worth saying. Every flow's runs and firings raise
/// the same kinds, so a `where` that does not single one out would list a link under all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// Only under a `where.cause` that goes past `flow:`.
    ByCause,
    /// Only under a `where.data.flow`.
    ByFlow,
    /// Always: the flow's own questions and signals.
    Always,
}

/// One event `a`'s firings would raise: its kind, the step whose run it is about, and when it
/// is worth saying.
struct Raised {
    kind: String,
    step: Option<String>,
    shown: Shown,
}

/// What `a`'s firings raise: each run step's run kinds; each ask's question's; the daemon's
/// `flow.*` of the firing, which carry `data.flow`; what its runs emitted.
fn raises(a: &Wired) -> Vec<Raised> {
    let mut raised = Vec::new();
    let of = |prefix: &'static str| event_kind::ALL.iter().filter(move |k| k.starts_with(prefix));
    for s in &a.def.steps {
        match s.does {
            Does::Run(_) => raised.extend(of("run.").map(|k| Raised {
                kind: k.to_string(),
                step: Some(s.name.clone()),
                shown: Shown::ByCause,
            })),
            Does::Ask(_) => raised.extend(of("decision.").map(|k| Raised {
                kind: k.to_string(),
                step: None,
                shown: Shown::Always,
            })),
        }
    }
    raised.extend(of("flow.").map(|k| Raised {
        kind: k.to_string(),
        step: None,
        shown: Shown::ByFlow,
    }));
    raised.extend(a.emitted.iter().map(|(step, k)| Raised {
        kind: k.clone(),
        step: Some(step.clone()),
        shown: Shown::Always,
    }));
    raised
}

/// The patterns of `b`'s `on` that `a`'s firings would start it on, as written. None when they
/// are one flow: a flow does not hear its own doing. What of a `where` cannot be known ahead —
/// a run's tags and cli, a group, most of `data` — rules nothing out.
pub fn heard(a: &Wired, b: &Wired) -> Vec<String> {
    if a.name == b.name {
        return Vec::new();
    }
    let w = b.def.trigger.filter.as_ref();
    let by_cause = w
        .and_then(|w| w.cause.as_deref())
        .is_some_and(|c| c.len() > "flow:".len());
    let by_flow = w.is_some_and(|w| w.data.contains_key("flow"));
    let unknown = |_: &str| None;
    let flow = |field: &str| (field == "flow").then(|| Some(a.name.clone()));
    let takes = |r: &Raised| {
        let shown = match r.shown {
            Shown::ByCause => by_cause,
            Shown::ByFlow => by_flow,
            Shown::Always => true,
        };
        let offered = Offered {
            run: r.step.as_ref().map(|step| OfferedRun {
                cause: format!("flow:{}/{step}", a.name),
                tags: None,
                cli: None,
            }),
            group: None,
            data: if r.shown == Shown::ByFlow { &flow } else { &unknown },
        };
        shown && w.is_none_or(|w| events::passes(w, &offered).is_ok())
    };
    let raised: Vec<Raised> = raises(a).into_iter().filter(takes).collect();
    b.def
        .trigger
        .on
        .iter()
        .filter(|p| raised.iter().any(|r| events::kind_matches(p, &r.kind)))
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
    use rl_proto::flow::{Filter, Match};
    use rl_proto::types::Signal;
    use std::collections::BTreeMap;

    /// A flow on `on` with its one step `s`, an ask when `ask`.
    fn def(name: &str, on: &[&str], filter: Option<Filter>, ask: bool) -> FlowDef {
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
            "steps": [{ "name": "s", "after": [], "if_": null, "in_": null, "does": does }]
        }))
        .unwrap();
        d.trigger.filter = filter;
        d
    }

    fn cause(c: &str) -> Option<Filter> {
        Some(Filter {
            cause: Some(c.into()),
            ..Default::default()
        })
    }

    fn w(name: &str, on: &[&str], filter: Option<Filter>) -> Wired {
        Wired {
            name: name.into(),
            def: def(name, on, filter, false),
            emitted: BTreeSet::new(),
        }
    }

    fn asking(name: &str) -> Wired {
        Wired {
            def: def(name, &[], None, true),
            ..w(name, &[], None)
        }
    }

    fn starts(name: &str, all: &[Wired]) -> String {
        starts_words(&links(name, all).starts)
    }

    #[test]
    fn a_flow_whose_where_names_another_is_started_by_its_runs() {
        let all = [w("a", &[], None), w("b", &["run.failed"], cause("flow:a"))];
        assert_eq!(links("b", &all).by, ["a"]);
        assert_eq!(starts("a", &all), "b (on run.failed)");
        // A step of it that runs, named, is it too.
        let all = [w("a", &[], None), w("b", &["run.*"], cause("flow:a/s"))];
        assert_eq!(links("b", &all).by, ["a"]);
        // A step it does not have, or `flow:` alone, is not.
        let all = [w("a", &[], None), w("b", &["run.*"], cause("flow:a/fetch"))];
        assert!(links("b", &all).by.is_empty());
        let all = [w("a", &[], None), w("b", &["run.failed"], cause("flow:"))];
        assert!(links("b", &all).by.is_empty());
        // Another flow's cause rules it out.
        let all = [w("a", &[], None), w("b", &["run.failed"], cause("flow:ab"))];
        assert!(links("a", &all).starts.is_empty());
    }

    #[test]
    fn a_cause_is_a_prefix_as_the_daemon_reads_it() {
        let all = [
            w("nightly", &[], None),
            w("nightly-2", &[], None),
            w("b", &["run.failed"], cause("flow:nightly")),
        ];
        assert_eq!(links("b", &all).by, ["nightly", "nightly-2"]);
        assert_eq!(starts("nightly-2", &all), "b (on run.failed)");
    }

    #[test]
    fn a_flow_that_runs_nothing_raises_no_runs() {
        let all = [asking("a"), w("b", &["run.failed"], cause("flow:a"))];
        assert!(links("b", &all).by.is_empty());
        let all = [asking("a"), w("b", &["run.*"], cause("flow:a/s"))];
        assert!(links("b", &all).by.is_empty());
    }

    #[test]
    fn an_unfiltered_run_or_flow_pattern_is_not_listed_under_every_flow() {
        let all = [w("a", &[], None), w("b", &["run.failed", "flow.ended"], None)];
        assert!(links("a", &all).starts.is_empty());
        assert!(links("b", &all).by.is_empty());
    }

    #[test]
    fn what_a_flows_runs_emitted_starts_others() {
        let mut a = w("a", &[], None);
        a.emitted.insert(("s".into(), "deploy.done".into()));
        let all = [a.clone(), w("b", &["deploy.done"], None)];
        assert_eq!(starts("a", &all), "b (on deploy.done)");
        // The step that emitted it is the run's cause.
        let all = [a.clone(), w("b", &["deploy.done"], cause("flow:a/s"))];
        assert_eq!(starts("a", &all), "b (on deploy.done)");
        let all = [a.clone(), w("b", &["deploy.done"], cause("flow:a/other"))];
        assert!(links("a", &all).starts.is_empty());
        let all = [a, w("b", &["deploy.done"], cause("flow:other"))];
        assert!(links("a", &all).starts.is_empty());
    }

    #[test]
    fn a_question_starts_what_hears_it_unless_its_where_needs_a_run() {
        let all = [asking("a"), w("c", &["decision.*", "run.failed"], None)];
        assert_eq!(starts("a", &all), "c (on decision.*)");
        // A question is about no run: the daemon refuses it a cause, a tag or a cli.
        let tag = Some(Filter {
            tag: Some("x".into()),
            ..Default::default()
        });
        let cli = Some(Filter {
            cli: Some("claude".into()),
            ..Default::default()
        });
        for filter in [cause("flow:a"), cause("flow:a/s"), tag, cli] {
            let all = [asking("a"), w("c", &["decision.taken"], filter.clone())];
            assert!(links("a", &all).starts.is_empty(), "{filter:?}");
        }
    }

    #[test]
    fn a_firings_end_starts_a_flow_whose_where_names_it() {
        let on = |flow: &str| {
            Some(Filter {
                data: BTreeMap::from([("flow".to_string(), Match::One(flow.into()))]),
                ..Default::default()
            })
        };
        let all = [w("a", &[], None), w("b", &["flow.ended"], on("a"))];
        assert_eq!(starts("a", &all), "b (on flow.ended)");
        let all = [w("a", &[], None), w("b", &["flow.ended"], on("other"))];
        assert!(links("a", &all).starts.is_empty());
        // A firing's own events are about no run.
        let mut both = on("a");
        both.as_mut().unwrap().cause = Some("flow:a".into());
        let all = [w("a", &[], None), w("b", &["flow.ended"], both)];
        assert!(links("a", &all).starts.is_empty());
    }

    #[test]
    fn a_flow_never_starts_itself() {
        let mut a = w("a", &["deploy.done", "run.failed"], cause("flow:a"));
        a.emitted.insert(("s".into(), "deploy.done".into()));
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
        assert_eq!(
            all[0].emitted,
            BTreeSet::from([("s".to_string(), "deploy.done".to_string())])
        );
        assert!(all[1].emitted.is_empty());
    }
}
