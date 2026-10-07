//! `graph.get`: the wiring graph, built from the flows as they stand and what
//! the ledger saw in a window. Pure — rows in, a `Graph` out.
//!
//! Every flow is a node, there before it has fired. The ledger gives the
//! rest: which runs raised which events, and so which flows heard them, and
//! the questions a flow's ask steps put to you. A run is its flow's node when
//! one of the flow's steps started it; a run you started stands in one node
//! per folder and kind (`shells` in relay). An event a flow's `on` takes that
//! nothing raised in the window still shows, as an event node, so every flow
//! that listens has a way in. The steps inside a flow are not drawn yet.

use crate::action::expand_home;
use crate::flows::kind_matches;
use rl_flow::def::FlowDef;
use rl_ledger::graph::{AskRow, EventCount, RunRow};
use rl_proto::decision::DecisionStatus;
use rl_proto::flow::Flow;
use rl_proto::graph::{Edge, Graph, Node, NodeKind, Tally};
use rl_proto::types::{Cause, RunKind, RunState};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct Inputs<'a> {
    pub flows: &'a [Flow],
    pub runs: &'a [RunRow],
    pub events: &'a [EventCount],
    pub asks: &'a [AskRow],
}

pub fn build(since: jiff::Timestamp, inp: Inputs<'_>) -> Graph {
    // Each flow as loaded: its folder, and what its `on` takes.
    let flows: Vec<(&Flow, Option<PathBuf>, Vec<String>)> = inp
        .flows
        .iter()
        .filter(|f| f.removed_at.is_none())
        .map(|f| {
            let def = serde_json::from_value::<FlowDef>(f.def.clone()).ok();
            let folder = def.as_ref().and_then(|d| d.in_.as_deref()).map(expand_home);
            (f, folder, def.map(|d| d.trigger.on).unwrap_or_default())
        })
        .collect();
    let mut g = Builder::default();

    for (f, folder, _) in &flows {
        g.node(Node {
            status: Some(f.status()),
            detail: f.when(),
            next_at: f.next_at,
            ..bare(&flow_id(&f.name), NodeKind::Flow, &f.name, folder.clone())
        });
    }

    // Questions its ask steps put to you: how many, and who is waiting.
    for a in inp.asks {
        let from = flow_id(&a.flow);
        let Some(owner) = g.nodes.get(&from) else { continue };
        let (folder, status) = (owner.folder.clone(), owner.status);
        let id = ask_id(&a.flow);
        let node = g.nodes.entry(id.clone()).or_insert_with(|| Node {
            status,
            ..bare(&id, NodeKind::Ask, "", folder)
        });
        // Oldest first: the newest question names it.
        node.name = a.title.clone();
        if a.status == DecisionStatus::Open {
            node.waiting.push(a.id.clone());
        }
        g.count(&from, &id, "", 1);
    }

    // Runs: each onto the node that stands for it.
    for run in inp.runs {
        let id = g.producer(&run.cause, run.kind, &run.workspace);
        g.ensure(&id, run.kind, &run.workspace);
        let t = &mut g.nodes.get_mut(&id).unwrap().runs;
        t.total += 1;
        if run.state == RunState::Failed {
            t.failed += 1;
        }
        if !run.state.is_terminal() {
            t.going += 1;
        }
        t.last = Some(run.state);
        t.last_at = Some(run.started_at);
    }

    // Events: from whoever raised them to every flow that hears them.
    let mut heard: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in inp.events {
        let from = match (&e.run, &e.by_flow) {
            (Some((cause, kind, ws)), _) => g.producer(cause, *kind, ws),
            (None, Some(name)) if e.kind.starts_with("decision.") && g.has(&ask_id(name)) => ask_id(name),
            (None, Some(name)) if g.has(&flow_id(name)) => flow_id(name),
            (None, _) => outside(&e.workspace),
        };
        for (f, _, on) in &flows {
            if !on.iter().any(|p| kind_matches(p, &e.kind)) {
                continue;
            }
            let to = flow_id(&f.name);
            // Its own doing: a flow does not hear what its steps did or asked.
            if from == to || from == ask_id(&f.name) {
                continue;
            }
            match &e.run {
                Some((_, kind, ws)) => g.ensure(&from, *kind, ws),
                None => g.ensure_outside(&from, &e.workspace),
            }
            g.count(&from, &to, &e.kind, e.count);
            heard.entry(to).or_default().insert(e.kind.clone());
        }
    }

    // A way in for every pattern nothing raised.
    for (f, _, on) in &flows {
        let to = flow_id(&f.name);
        let got = heard.get(&to);
        for pattern in on {
            if got.is_some_and(|kinds| kinds.iter().any(|k| kind_matches(pattern, k))) {
                continue;
            }
            let id = format!("event:{pattern}");
            g.node(bare(&id, NodeKind::Event, pattern, None));
            g.edge(&id, &to, pattern, true);
        }
    }

    g.finish(since)
}

#[derive(Default)]
struct Builder {
    nodes: BTreeMap<String, Node>,
    /// By (from, to): the kinds or keys it carries, its count, whether declared.
    edges: BTreeMap<(String, String), (BTreeSet<String>, u64, bool)>,
}

impl Builder {
    fn has(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// The node a run stands in: its flow, when one of its steps started it,
    /// or its folder's own runs of its kind.
    fn producer(&self, cause: &str, kind: RunKind, ws: &Path) -> String {
        if let Ok(Cause::Flow { flow, .. }) = cause.parse::<Cause>()
            && self.has(&flow_id(&flow))
        {
            return flow_id(&flow);
        }
        let k = match kind {
            RunKind::Agent => "agents",
            RunKind::Shell => "shells",
            RunKind::Command | RunKind::Plugin => "commands",
        };
        format!("runs:{k}:{}", ws.display())
    }

    fn node(&mut self, n: Node) {
        self.nodes.entry(n.id.clone()).or_insert(n);
    }

    /// A folder's own runs, by kind, the first time something points at them.
    fn ensure(&mut self, id: &str, kind: RunKind, ws: &Path) {
        if self.has(id) {
            return;
        }
        let (kind, name) = match kind {
            RunKind::Agent => (NodeKind::Agent, "agents"),
            RunKind::Shell => (NodeKind::Shell, "shells"),
            RunKind::Command | RunKind::Plugin => (NodeKind::Shell, "commands"),
        };
        self.node(bare(id, kind, name, Some(ws.to_path_buf())));
    }

    fn ensure_outside(&mut self, id: &str, ws: &Path) {
        if !self.has(id) {
            self.node(bare(id, NodeKind::Outside, "outside", Some(ws.to_path_buf())));
        }
    }

    fn edge(&mut self, from: &str, to: &str, label: &str, declared: bool) {
        let e = self.edges.entry((from.into(), to.into())).or_default();
        if !label.is_empty() {
            e.0.insert(label.into());
        }
        e.2 |= declared;
    }

    fn count(&mut self, from: &str, to: &str, label: &str, n: u64) {
        let e = self.edges.entry((from.into(), to.into())).or_default();
        if !label.is_empty() {
            e.0.insert(label.into());
        }
        e.1 += n;
    }

    fn finish(self, since: jiff::Timestamp) -> Graph {
        Graph {
            since,
            nodes: self.nodes.into_values().collect(),
            edges: self
                .edges
                .into_iter()
                .map(|((from, to), (labels, count, declared))| Edge {
                    from,
                    to,
                    label: labels.into_iter().collect::<Vec<_>>().join(", "),
                    count,
                    declared,
                })
                .collect(),
        }
    }
}

fn bare(id: &str, kind: NodeKind, name: &str, folder: Option<PathBuf>) -> Node {
    Node {
        id: id.into(),
        kind,
        name: name.into(),
        folder,
        rule: None,
        status: None,
        detail: String::new(),
        next_at: None,
        runs: Tally::default(),
        waiting: vec![],
    }
}

fn flow_id(name: &str) -> String {
    format!("flow:{name}")
}

fn ask_id(flow: &str) -> String {
    format!("ask:{flow}")
}

fn outside(ws: &Path) -> String {
    format!("outside:{}", ws.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn flow(name: &str, yaml: &str) -> Flow {
        let text = format!("---\n{yaml}steps:\n  a:\n    run: x\n---\n");
        let def = rl_flow::def::check(name, rl_flow::doc::read(&text).unwrap(), &|_| true).unwrap();
        Flow {
            name: name.into(),
            hash: "h".into(),
            def: serde_json::to_value(&def).unwrap(),
            source: PathBuf::new(),
            folder: None,
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    fn run(cause: &str, kind: RunKind, ws: &str, state: RunState) -> RunRow {
        RunRow {
            cause: cause.into(),
            kind,
            workspace: ws.into(),
            state,
            started_at: at("2026-09-16T07:00:00Z"),
        }
    }

    fn event(kind: &str, ws: &str, run: Option<(&str, RunKind, &str)>, count: u64) -> EventCount {
        EventCount {
            kind: kind.into(),
            workspace: ws.into(),
            run: run.map(|(c, k, w)| (c.to_string(), k, PathBuf::from(w))),
            by_flow: None,
            count,
        }
    }

    fn edge<'a>(g: &'a Graph, from: &str, to: &str) -> &'a Edge {
        g.edges
            .iter()
            .find(|e| e.from == from && e.to == to)
            .unwrap_or_else(|| panic!("no edge {from} → {to} in {:#?}", g.edges))
    }

    #[test]
    fn one_network_of_flows_from_what_happened() {
        let flows = vec![
            flow("nightly", "in: /w/relay\nwhen: every 1h\n"),
            // Hears failures from every folder, and a kind nothing raised.
            flow("notify", "on: [run.failed, never.*]\n"),
            flow("review", "on: weather.alert\n"),
            Flow {
                removed_at: Some(at("2026-09-01T00:00:00Z")),
                ..flow("gone", "on: run.failed\n")
            },
        ];
        let runs = vec![
            run("flow:nightly/probe", RunKind::Command, "/w/relay", RunState::Finished),
            run("flow:nightly/test", RunKind::Command, "/w/relay", RunState::Failed),
            run("manual", RunKind::Shell, "/w/api", RunState::Failed),
            run("manual", RunKind::Shell, "/w/api", RunState::Working),
            run("flow:notify/a", RunKind::Agent, "/w", RunState::Failed),
        ];
        let events = vec![
            event(
                "weather.alert",
                "/w/relay",
                Some(("flow:nightly/probe", RunKind::Command, "/w/relay")),
                3,
            ),
            event(
                "run.failed",
                "/w/relay",
                Some(("flow:nightly/test", RunKind::Command, "/w/relay")),
                1,
            ),
            event("run.failed", "/w/api", Some(("manual", RunKind::Shell, "/w/api")), 2),
            // Its own failure is not news to it.
            event("run.failed", "/w", Some(("flow:notify/a", RunKind::Agent, "/w")), 1),
            EventCount {
                by_flow: Some("review".into()),
                ..event("decision.raised", "/w", None, 2)
            },
        ];
        let asks = vec![
            AskRow {
                id: "d1".into(),
                flow: "review".into(),
                title: "add a note?".into(),
                status: DecisionStatus::Taken,
            },
            AskRow {
                id: "d2".into(),
                flow: "review".into(),
                title: "add another?".into(),
                status: DecisionStatus::Open,
            },
            AskRow {
                id: "d3".into(),
                flow: "gone".into(),
                title: "never mind".into(),
                status: DecisionStatus::Open,
            },
        ];
        let g = build(
            at("2026-09-09T00:00:00Z"),
            Inputs {
                flows: &flows,
                runs: &runs,
                events: &events,
                asks: &asks,
            },
        );

        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "ask:review",
                "event:never.*",
                "flow:nightly",
                "flow:notify",
                "flow:review",
                "runs:shells:/w/api",
            ],
            "a removed flow is not drawn"
        );

        // A flow, and its runs whichever step ran them.
        let nightly = g.node("flow:nightly").unwrap();
        assert_eq!(
            (nightly.kind, nightly.detail.as_str(), nightly.folder.as_deref()),
            (NodeKind::Flow, "every 1h", Some(Path::new("/w/relay")))
        );
        assert_eq!((nightly.runs.total, nightly.runs.failed), (2, 1));

        // What only the ledger knows: nightly raised the alert and failed.
        let e = edge(&g, "flow:nightly", "flow:review");
        assert_eq!((e.label.as_str(), e.count, e.declared), ("weather.alert", 3, false));
        assert_eq!(edge(&g, "flow:nightly", "flow:notify").count, 1);
        let e = edge(&g, "runs:shells:/w/api", "flow:notify");
        assert_eq!((e.label.as_str(), e.count), ("run.failed", 2));
        let shells = g.node("runs:shells:/w/api").unwrap();
        assert_eq!((shells.runs.total, shells.runs.going), (2, 1));
        assert!(
            !g.edges.iter().any(|e| e.from == "flow:notify" && e.to == "flow:notify"),
            "no flow hears itself"
        );

        // Its questions: one waiting, the newest naming it.
        let ask = g.node("ask:review").unwrap();
        assert_eq!((ask.kind, ask.name.as_str()), (NodeKind::Ask, "add another?"));
        assert_eq!(ask.waiting, ["d2"]);
        assert_eq!(edge(&g, "flow:review", "ask:review").count, 2);

        // A pattern nothing raised still has a way in.
        let e = edge(&g, "event:never.*", "flow:notify");
        assert_eq!((e.count, e.declared), (0, true));
        assert!(g.node("event:run.failed").is_none(), "run.failed was heard");
    }

    #[test]
    fn a_flows_own_events_and_questions_come_from_it() {
        let flows = vec![
            flow("asker", "on: custom.thing\n"),
            flow("watch", "on: [flow.ended, decision.*]\n"),
        ];
        let events = vec![
            EventCount {
                by_flow: Some("asker".into()),
                ..event("flow.ended", "/h", None, 1)
            },
            EventCount {
                by_flow: Some("asker".into()),
                ..event("decision.raised", "/h", None, 1)
            },
            event("decision.raised", "/w", None, 1),
        ];
        let asks = vec![AskRow {
            id: "d1".into(),
            flow: "asker".into(),
            title: "go?".into(),
            status: DecisionStatus::Open,
        }];
        let g = build(
            at("2026-09-09T00:00:00Z"),
            Inputs {
                flows: &flows,
                runs: &[],
                events: &events,
                asks: &asks,
            },
        );
        assert_eq!(edge(&g, "flow:asker", "flow:watch").label, "flow.ended");
        assert_eq!(edge(&g, "ask:asker", "flow:watch").label, "decision.raised");
        let e = edge(&g, "outside:/w", "flow:watch");
        assert_eq!((e.label.as_str(), e.count), ("decision.raised", 1));
        assert!(edge(&g, "event:custom.thing", "flow:asker").declared);
    }
}
