//! `graph.get`: the wiring graph, built from the flows as they stand and what
//! the ledger saw in a window. Pure — rows in, a `Graph` out.
//!
//! Every flow is a node, there before it has fired, and so is each of its
//! steps (`step:<flow>/<step>`), wired as the flow writes them. The ledger
//! gives the rest: which runs raised which events, and so which flows heard
//! them, and the questions a flow's ask steps put to you. A run is its step's
//! node when the step is the flow's still, else its flow's; a run you started
//! stands in one node per folder and kind (`shells` in relay). An event a
//! flow's `on` takes that nothing raised in the window still shows, as an
//! event node, so every flow that listens has a way in.

use crate::flows::kind_matches;
use rl_flow::def::{Does, FlowDef};
use rl_ledger::graph::{AskRow, EventCount, RunRow};
use rl_proto::action::Task;
use rl_proto::decision::DecisionStatus;
use rl_proto::flow::Flow;
use rl_proto::graph::{Edge, Graph, Node, NodeKind, Tally};
use rl_proto::types::{Cause, RunKind, RunState};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct Inputs<'a> {
    /// As a client is shown them: each one's `in` resolved (`Flow::folder`).
    pub flows: &'a [Flow],
    pub runs: &'a [RunRow],
    pub events: &'a [EventCount],
    pub asks: &'a [AskRow],
}

/// A flow as loaded: its folder, its def, and what its `on` takes.
type Loaded<'a> = (&'a Flow, Option<PathBuf>, Option<FlowDef>, Vec<String>);

pub fn build(since: jiff::Timestamp, inp: Inputs<'_>) -> Graph {
    let flows: Vec<Loaded<'_>> = inp
        .flows
        .iter()
        .filter(|f| f.removed_at.is_none())
        .map(|f| {
            let def = serde_json::from_value::<FlowDef>(f.def.clone()).ok();
            let on = def.as_ref().map(|d| d.trigger.on.clone()).unwrap_or_default();
            (f, f.folder.clone(), def, on)
        })
        .collect();
    let mut g = Builder::default();

    for (f, folder, def, _) in &flows {
        g.node(Node {
            status: Some(f.status()),
            detail: f.when(),
            next_at: f.next_at,
            flow: Some(f.name.clone()),
            ..bare(&flow_id(&f.name), NodeKind::Flow, &f.name, folder.clone())
        });
        // Its steps, wired as it writes them: from the flow itself when a step waits on
        // nothing, else from each step it comes after; its `if:` on the edge.
        for s in def.iter().flat_map(|d| &d.steps) {
            let id = step_id(&f.name, &s.name);
            let (kind, detail) = match &s.does {
                Does::Ask(a) => (NodeKind::Ask, a.text.clone()),
                Does::Run(a) => match &a.run {
                    Some(Task::Agent { .. }) => (NodeKind::Agent, a.label()),
                    _ => (NodeKind::Shell, a.label()),
                },
            };
            g.node(Node {
                status: Some(f.status()),
                detail,
                flow: Some(f.name.clone()),
                ..bare(&id, kind, &s.name, folder.clone())
            });
            let label = s.if_.as_ref().map(|c| format!("if {c}")).unwrap_or_default();
            if s.after.is_empty() {
                g.edge(&flow_id(&f.name), &id, &label, true);
            }
            for a in &s.after {
                g.edge(&step_id(&f.name, a), &id, &label, true);
            }
        }
    }

    // Questions its ask steps put to you, on the step that asked: who is waiting, and the
    // newest naming it. A step the flow no longer has still shows while it asks.
    for a in inp.asks {
        let flow = flow_id(&a.flow);
        let Some(owner) = g.nodes.get(&flow) else { continue };
        let (folder, status) = (owner.folder.clone(), owner.status);
        let id = step_id(&a.flow, &a.step);
        if !g.has(&id) {
            g.node(Node {
                status,
                flow: Some(a.flow.clone()),
                ..bare(&id, NodeKind::Ask, &a.step, folder)
            });
            g.edge(&flow, &id, "", true);
        }
        let node = g.nodes.get_mut(&id).unwrap();
        node.detail = a.title.clone();
        if a.status == DecisionStatus::Open {
            node.waiting.push(a.id.clone());
        }
        g.did(&id);
    }

    // Runs: each onto the node that stands for it; a step's on its flow's too.
    for run in inp.runs {
        let id = g.producer(&run.cause, run.kind, &run.workspace);
        g.ensure(&id, run.kind, &run.workspace);
        g.tally(&id, run);
        if id.starts_with("step:")
            && let Some(flow) = g.nodes[&id].flow.clone()
        {
            g.tally(&flow_id(&flow), run);
        }
        // What a step did; an ask's option runs as the step's own, and the question
        // already counted it.
        if id.starts_with("step:") && g.nodes[&id].kind != NodeKind::Ask {
            g.did(&id);
        }
    }

    // Events: from whoever raised them to every flow that hears them.
    let mut heard: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in inp.events {
        let from = match (&e.run, &e.by_flow) {
            (Some((cause, kind, ws)), _) => g.producer(cause, *kind, ws),
            (None, Some(name)) if e.kind.starts_with("decision.") => {
                match e.by_step.as_deref().map(|s| step_id(name, s)).filter(|id| g.has(id)) {
                    Some(id) => id,
                    None if g.has(&flow_id(name)) => flow_id(name),
                    None => outside(&e.workspace),
                }
            }
            (None, Some(name)) if g.has(&flow_id(name)) => flow_id(name),
            (None, _) => outside(&e.workspace),
        };
        for (f, _, _, on) in &flows {
            if !on.iter().any(|p| kind_matches(p, &e.kind)) {
                continue;
            }
            // Its own doing: a flow does not hear what its steps did or asked.
            if g.owner(&from) == Some(f.name.as_str()) {
                continue;
            }
            let to = flow_id(&f.name);
            match &e.run {
                Some((_, kind, ws)) => g.ensure(&from, *kind, ws),
                None => g.ensure_outside(&from, &e.workspace),
            }
            g.count(&from, &to, &e.kind, e.count);
            heard.entry(to).or_default().insert(e.kind.clone());
        }
    }

    // A way in for every pattern nothing raised.
    for (f, _, _, on) in &flows {
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
    /// By node: what it did in the window, which a declared edge into it counts.
    did: BTreeMap<String, u64>,
}

impl Builder {
    fn has(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// The flow a node is, or is a step of.
    fn owner(&self, id: &str) -> Option<&str> {
        self.nodes.get(id)?.flow.as_deref()
    }

    /// Something `id` did in the window: a run of it, or a question it asked.
    fn did(&mut self, id: &str) {
        *self.did.entry(id.to_string()).or_default() += 1;
    }

    fn tally(&mut self, id: &str, run: &RunRow) {
        let t = &mut self.nodes.get_mut(id).unwrap().runs;
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

    /// The node a run stands in: its step, when the flow still has the step
    /// that started it, else its flow; or its folder's own runs of its kind.
    fn producer(&self, cause: &str, kind: RunKind, ws: &Path) -> String {
        if let Ok(Cause::Flow { flow, step }) = cause.parse::<Cause>() {
            for id in [step_id(&flow, &step), flow_id(&flow)] {
                if self.has(&id) {
                    return id;
                }
            }
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

    /// A declared edge counts what its target step did; a way in, from an
    /// event node, is what could fire a flow, not what did, and stays 0. One
    /// only the ledger knows keeps its own count.
    fn finish(self, since: jiff::Timestamp) -> Graph {
        let did = self.did;
        Graph {
            since,
            nodes: self.nodes.into_values().collect(),
            edges: self
                .edges
                .into_iter()
                .map(|((from, to), (labels, count, declared))| Edge {
                    count: match (declared, from.starts_with("event:")) {
                        (true, true) => 0,
                        (true, false) => did.get(&to).copied().unwrap_or(0),
                        (false, _) => count,
                    },
                    from,
                    to,
                    label: labels.into_iter().collect::<Vec<_>>().join(", "),
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
        status: None,
        detail: String::new(),
        next_at: None,
        runs: Tally::default(),
        waiting: vec![],
        flow: None,
    }
}

fn flow_id(name: &str) -> String {
    format!("flow:{name}")
}

fn step_id(flow: &str, step: &str) -> String {
    format!("step:{flow}/{step}")
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
            folder: def.in_.clone(),
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
            by_step: None,
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
            // From a step notify no longer has: it stands on the flow.
            run("flow:notify/gone", RunKind::Agent, "/w", RunState::Finished),
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
                step: "ask".into(),
                title: "add a note?".into(),
                status: DecisionStatus::Taken,
            },
            AskRow {
                id: "d2".into(),
                flow: "review".into(),
                step: "ask".into(),
                title: "add another?".into(),
                status: DecisionStatus::Open,
            },
            AskRow {
                id: "d3".into(),
                flow: "gone".into(),
                step: "ask".into(),
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
                "event:never.*",
                "flow:nightly",
                "flow:notify",
                "flow:review",
                "runs:shells:/w/api",
                "step:nightly/a",
                "step:notify/a",
                "step:review/a",
                "step:review/ask",
            ],
            "a removed flow is not drawn; an ask step it no longer has still shows while it asks"
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
        let ask = g.node("step:review/ask").unwrap();
        assert_eq!(
            (ask.kind, ask.name.as_str(), ask.detail.as_str()),
            (NodeKind::Ask, "ask", "add another?")
        );
        assert_eq!(ask.waiting, ["d2"]);
        assert_eq!(edge(&g, "flow:review", "step:review/ask").count, 2);

        // A pattern nothing raised still has a way in, which nothing came by — though the
        // flow ran, from a step it no longer has.
        assert_eq!(g.node("flow:notify").unwrap().runs.total, 2);
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
                by_step: Some("ask".into()),
                ..event("decision.raised", "/h", None, 1)
            },
            event("decision.raised", "/w", None, 1),
        ];
        let asks = vec![AskRow {
            id: "d1".into(),
            flow: "asker".into(),
            step: "ask".into(),
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
        assert_eq!(edge(&g, "step:asker/ask", "flow:watch").label, "decision.raised");
        let e = edge(&g, "outside:/w", "flow:watch");
        assert_eq!((e.label.as_str(), e.count), ("decision.raised", 1));
        assert!(edge(&g, "event:custom.thing", "flow:asker").declared);
    }

    /// A flow as `rld` has it, from its whole `flow.md`.
    fn flow_md(name: &str, text: &str) -> Flow {
        let def = rl_flow::def::check(name, rl_flow::doc::read(text).unwrap(), &|_| true).unwrap();
        Flow {
            name: name.into(),
            hash: "h".into(),
            def: serde_json::to_value(&def).unwrap(),
            source: PathBuf::new(),
            folder: def.in_.clone(),
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    #[test]
    fn a_flows_steps_hang_off_it_as_it_writes_them() {
        let prod_logs = "---\nin: /w/api\nwhen: every 30m\nsteps:\n  probe:\n    run: new-log-errors.sh\n  \
            triage:\n    after: probe\n    if: probe.out\n    run: { agent: claude, prompt: look }\n  \
            review:\n    after: triage\n    ask: \"{triage.out}\"\n    options:\n      \
            apply: { run: { agent: claude, prompt: do it } }\n      ignore: {}\n---\n";
        let flows = vec![flow_md("prod-logs", prod_logs), flow("notify", "on: run.failed\n")];
        let runs = vec![
            run("flow:prod-logs/probe", RunKind::Command, "/w/api", RunState::Finished),
            run("flow:prod-logs/probe", RunKind::Command, "/w/api", RunState::Finished),
            run("flow:prod-logs/triage", RunKind::Agent, "/w/api", RunState::Failed),
        ];
        let events = vec![event(
            "run.failed",
            "/w/api",
            Some(("flow:prod-logs/triage", RunKind::Agent, "/w/api")),
            1,
        )];
        let asks = vec![AskRow {
            id: "d1".into(),
            flow: "prod-logs".into(),
            step: "review".into(),
            title: "restart the api pod?".into(),
            status: DecisionStatus::Open,
        }];
        let g = build(
            at("2026-09-09T00:00:00Z"),
            Inputs {
                flows: &flows,
                runs: &runs,
                events: &events,
                asks: &asks,
            },
        );
        let node = |id: &str| g.node(id).unwrap_or_else(|| panic!("no {id} in {:#?}", g.nodes));
        let probe = node("step:prod-logs/probe");
        assert_eq!(
            (
                probe.kind,
                probe.name.as_str(),
                probe.detail.as_str(),
                probe.flow.as_deref()
            ),
            (NodeKind::Shell, "probe", "new-log-errors.sh", Some("prod-logs"))
        );
        assert_eq!(probe.runs.total, 2);
        assert_eq!(node("step:prod-logs/triage").kind, NodeKind::Agent);
        let review = node("step:prod-logs/review");
        assert_eq!(
            (review.kind, review.detail.as_str()),
            (NodeKind::Ask, "restart the api pod?")
        );
        assert_eq!(review.waiting, ["d1"]);
        assert_eq!(node("flow:prod-logs").flow.as_deref(), Some("prod-logs"));
        assert_eq!(
            (node("flow:prod-logs").runs.total, node("flow:prod-logs").runs.failed),
            (3, 1),
            "a step's runs count on its flow too"
        );

        // Declared, as the flow writes them, each counting what its step did.
        let e = edge(&g, "flow:prod-logs", "step:prod-logs/probe");
        assert_eq!((e.declared, e.count, e.label.as_str()), (true, 2, ""));
        let e = edge(&g, "step:prod-logs/probe", "step:prod-logs/triage");
        assert_eq!((e.declared, e.count, e.label.as_str()), (true, 1, "if probe.out"));
        assert_eq!(
            edge(&g, "step:prod-logs/triage", "step:prod-logs/review").count,
            1,
            "one question"
        );

        // Between flows, the event leaves the step that raised it.
        let e = edge(&g, "step:prod-logs/triage", "flow:notify");
        assert_eq!((e.label.as_str(), e.count, e.declared), ("run.failed", 1, false));
        assert!(
            g.nodes.iter().all(|n| !n.id.starts_with("ask:")),
            "no ask:<flow> node any more"
        );
    }
}
