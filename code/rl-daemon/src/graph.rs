//! `graph.get`: the wiring graph, built from the rules as they stand and what
//! the ledger saw in a window. Pure — rows in, a `Graph` out.
//!
//! The rules give the nodes and the edges that are always there: a schedule
//! and what it runs, a reaction and what it runs or asks, a question's
//! answers and what each runs. The ledger gives the rest: which runs raised
//! which events, and so which reactions heard them. A run is its rule's
//! action node when a rule started it; a run you started stands in one node
//! per folder and kind (`shells` in relay), drawn only when it set something
//! off. An event a reaction listens for that nothing raised in the window
//! still shows, as an event node, so every reaction has a way in.

use rl_ledger::graph::{AskRow, EventCount, RunRow};
use rl_proto::action::Task;
use rl_proto::decision::DecisionStatus;
use rl_proto::graph::{Edge, Graph, Node, NodeKind, Tally};
use rl_proto::reaction::Reaction;
use rl_proto::rule::{Rule, RuleKind};
use rl_proto::schedule::GLOBAL_SCOPE;
use rl_proto::types::{Cause, RunKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub struct Inputs<'a> {
    pub rules: &'a [Rule],
    pub runs: &'a [RunRow],
    pub events: &'a [EventCount],
    pub asks: &'a [AskRow],
}

pub fn build(since: jiff::Timestamp, inp: Inputs<'_>) -> Graph {
    let rules: Vec<&Rule> = inp.rules.iter().filter(|r| r.removed_at().is_none()).collect();
    let mut g = Builder::default();

    // What the files say.
    for rule in &rules {
        let id = rule_id(rule);
        let folder = folder_of(rule.scope());
        let key = id.clone();
        g.node(Node {
            id: id.clone(),
            kind: match rule.kind() {
                RuleKind::Schedule => NodeKind::Schedule,
                RuleKind::Reaction => NodeKind::Reaction,
            },
            name: rule.name().to_string(),
            folder: folder.clone(),
            rule: Some(key.clone()),
            status: Some(rule.status()),
            detail: rule.when(),
            next_at: rule.next_at(),
            runs: Tally::default(),
            waiting: vec![],
        });
        let task = match rule {
            Rule::Schedule(s) => s.spec.run.as_ref(),
            Rule::Reaction(r) if r.spec.ask.is_none() => r.spec.run.as_ref(),
            Rule::Reaction(_) => None,
        };
        if let Some(task) = task {
            let run = format!("run:{id}");
            g.node(action(&run, task, folder.clone(), &key, rule));
            g.edge(&id, &run, "", true);
        }
        if let Rule::Reaction(r) = rule
            && let Some(ask) = &r.spec.ask
        {
            let ask_id = format!("ask:{id}");
            g.node(Node {
                id: ask_id.clone(),
                kind: NodeKind::Ask,
                name: ask.title.clone(),
                folder: folder.clone(),
                rule: Some(key.clone()),
                status: Some(rule.status()),
                detail: String::new(),
                next_at: None,
                runs: Tally::default(),
                waiting: vec![],
            });
            g.edge(&id, &ask_id, "", true);
            for opt in &ask.options {
                if let Some(task) = &opt.run {
                    let run = format!("run:{ask_id}/{}", opt.key);
                    g.node(action(&run, task, folder.clone(), &key, rule));
                    g.edge(&ask_id, &run, &opt.key, true);
                }
            }
        }
    }

    // Questions: who is waiting, and what was answered.
    let asks: HashMap<&str, &AskRow> = inp.asks.iter().map(|a| (a.id.as_str(), a)).collect();
    for a in inp.asks {
        let Some(r) = find(&rules, RuleKind::Reaction, &a.by_reaction, &a.workspace) else {
            continue;
        };
        let ask_id = format!("ask:{}", rule_id(r));
        if !g.has(&ask_id) {
            continue;
        }
        g.count(&rule_id(r), &ask_id, "", 1);
        if a.status == DecisionStatus::Open {
            g.nodes.get_mut(&ask_id).unwrap().waiting.push(a.id.clone());
        }
        for key in &a.chosen {
            let run = format!("run:{ask_id}/{key}");
            if g.has(&run) {
                g.count(&ask_id, &run, key, 1);
            }
        }
    }

    // Runs: each onto the node that stands for it.
    for run in inp.runs {
        let id = g.producer(&rules, &asks, &run.cause, run.kind, &run.workspace);
        g.ensure(&id, run.kind, &run.workspace);
        let t = &mut g.nodes.get_mut(&id).unwrap().runs;
        t.total += 1;
        if run.state == rl_proto::types::RunState::Failed {
            t.failed += 1;
        }
        if !run.state.is_terminal() {
            t.going += 1;
        }
        t.last = Some(run.state);
        t.last_at = Some(run.started_at);
        // A rule's own edge to what it runs counts its runs.
        if let Some(rule) = id.strip_prefix("run:").filter(|r| !r.starts_with("ask:")) {
            let rule = rule.to_string();
            g.count(&rule, &id, "", 1);
        }
    }

    // Events: from whoever raised them to every reaction that hears them.
    let reactions: Vec<&Reaction> = rules
        .iter()
        .filter_map(|r| match r {
            Rule::Reaction(r) => Some(r),
            _ => None,
        })
        .collect();
    let mut heard: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in inp.events {
        let from = match (&e.run, &e.asked_by) {
            (Some((cause, kind, ws)), _) => g.producer(&rules, &asks, cause, *kind, ws),
            (None, Some(name)) => match find(&rules, RuleKind::Reaction, name, &e.workspace) {
                Some(r) => format!("ask:{}", rule_id(r)),
                None => outside(&e.workspace),
            },
            (None, None) => outside(&e.workspace),
        };
        for r in &reactions {
            if !r.spec.matches_kind(&e.kind) {
                continue;
            }
            let to = format!("reaction:{}/{}", r.scope, r.name);
            // Its own doing: a reaction does not hear what it started or asked.
            if from == format!("run:{to}") || from == format!("ask:{to}") {
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

    // What the files say a command raises.
    for rule in &rules {
        let emits = match rule {
            Rule::Schedule(s) => &s.spec.emits,
            Rule::Reaction(r) => &r.spec.emits,
        };
        let from = format!("run:{}", rule_id(rule));
        if emits.is_empty() || !g.has(&from) {
            continue;
        }
        for kind in emits {
            for r in &reactions {
                let to = format!("reaction:{}/{}", r.scope, r.name);
                if r.spec.matches_kind(kind) && from != format!("run:{to}") {
                    g.edge(&from, &to, kind, true);
                    heard.entry(to).or_default().insert(kind.clone());
                }
            }
        }
    }

    // A way in for every pattern nothing raised.
    for r in &reactions {
        let to = format!("reaction:{}/{}", r.scope, r.name);
        let got = heard.get(&to);
        for pattern in &r.spec.on {
            let covered = got.is_some_and(|kinds| kinds.iter().any(|k| matches(pattern, k)));
            if covered {
                continue;
            }
            let id = format!("event:{pattern}");
            if !g.has(&id) {
                g.node(Node {
                    id: id.clone(),
                    kind: NodeKind::Event,
                    name: pattern.clone(),
                    folder: None,
                    rule: None,
                    status: None,
                    detail: String::new(),
                    next_at: None,
                    runs: Tally::default(),
                    waiting: vec![],
                });
            }
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

    /// The node a run stands in: its rule's action, an answer's action, or
    /// its folder's own runs of its kind.
    fn producer(
        &self,
        rules: &[&Rule],
        asks: &HashMap<&str, &AskRow>,
        cause: &str,
        kind: RunKind,
        ws: &Path,
    ) -> String {
        let by_rule = match cause.parse::<Cause>() {
            Ok(Cause::Schedule(n)) => find(rules, RuleKind::Schedule, &n, ws).map(|r| format!("run:{}", rule_id(r))),
            Ok(Cause::Reaction(n)) => find(rules, RuleKind::Reaction, &n, ws).map(|r| format!("run:{}", rule_id(r))),
            Ok(Cause::Decision(id)) => asks.get(id.as_str()).and_then(|a| {
                let r = find(rules, RuleKind::Reaction, &a.by_reaction, &a.workspace)?;
                let key = a.chosen.first()?;
                Some(format!("run:ask:{}/{key}", rule_id(r)))
            }),
            _ => None,
        };
        match by_rule {
            Some(id) if self.has(&id) => id,
            _ => {
                let k = match kind {
                    RunKind::Agent => "agents",
                    RunKind::Shell => "shells",
                    RunKind::Command | RunKind::Plugin => "commands",
                };
                format!("runs:{k}:{}", ws.display())
            }
        }
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

/// What a rule, or one of its answers, runs.
fn action(id: &str, task: &Task, folder: Option<PathBuf>, key: &str, rule: &Rule) -> Node {
    Node {
        id: id.into(),
        kind: match task {
            Task::Agent { .. } => NodeKind::Agent,
            Task::Shell(_) | Task::Argv(_) => NodeKind::Shell,
        },
        name: task.label(),
        folder,
        rule: Some(key.into()),
        status: Some(rule.status()),
        detail: String::new(),
        next_at: None,
        runs: Tally::default(),
        waiting: vec![],
    }
}

fn rule_id(r: &Rule) -> String {
    format!("{}:{}/{}", r.kind(), r.scope(), r.name())
}

fn folder_of(scope: &str) -> Option<PathBuf> {
    (scope != GLOBAL_SCOPE).then(|| PathBuf::from(scope))
}

fn outside(ws: &Path) -> String {
    format!("outside:{}", ws.display())
}

fn matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == kind,
    }
}

/// A rule by kind and name. Names can repeat across files: the one in the
/// run's own folder wins, then the global one, then any.
fn find<'a>(rules: &[&'a Rule], kind: RuleKind, name: &str, ws: &Path) -> Option<&'a Rule> {
    let named: Vec<&'a Rule> = rules
        .iter()
        .copied()
        .filter(|r| r.kind() == kind && r.name() == name)
        .collect();
    named
        .iter()
        .find(|r| Path::new(r.scope()) == ws)
        .or_else(|| named.iter().find(|r| r.is_global()))
        .or_else(|| named.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::reaction::ReactionSpec;
    use rl_proto::schedule::{Schedule, ScheduleSpec};
    use rl_proto::types::RunState;
    use serde_json::json;

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn schedule(scope: &str, name: &str, spec: serde_json::Value) -> Rule {
        let spec: ScheduleSpec = serde_json::from_value(spec).unwrap();
        Rule::Schedule(Schedule {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/x".into(),
            spec,
            paused: false,
            last_fired_at: None,
            last_run: None,
            next_at: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
    }

    fn reaction(scope: &str, name: &str, spec: serde_json::Value) -> Rule {
        let spec: ReactionSpec = serde_json::from_value(spec).unwrap();
        Rule::Reaction(Reaction {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/x".into(),
            spec,
            paused: false,
            last_fired_at: None,
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
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
            asked_by: None,
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
    fn one_network_across_folders_from_files_and_what_happened() {
        let rules = vec![
            schedule(
                "/w/relay",
                "check-weather",
                json!({"cron": "0 7 * * *", "run": "check-weather"}),
            ),
            reaction(
                "/w/relay",
                "advisory",
                json!({"on": "weather.alert", "ask": {"title": "add a note?", "options": [
                    {"key": "note", "run": "echo note >> notes"},
                    {"key": "skip"},
                ]}}),
            ),
            // Global, and hears failures from every folder.
            reaction(
                GLOBAL_SCOPE,
                "tell-me",
                json!({"on": ["run.failed", "never.*"], "cwd": "/w", "run": {"agent": "claude", "prompt": "why"}}),
            ),
        ];
        let runs = vec![
            run(
                "schedule:check-weather",
                RunKind::Command,
                "/w/relay",
                RunState::Finished,
            ),
            run("schedule:check-weather", RunKind::Command, "/w/relay", RunState::Failed),
            run("manual", RunKind::Shell, "/w/api", RunState::Failed),
            run("manual", RunKind::Shell, "/w/api", RunState::Working),
            run("decision:d1", RunKind::Command, "/w/relay", RunState::Finished),
            run("reaction:tell-me", RunKind::Agent, "/w", RunState::Failed),
        ];
        let events = vec![
            event(
                "weather.alert",
                "/w/relay",
                Some(("schedule:check-weather", RunKind::Command, "/w/relay")),
                3,
            ),
            event(
                "run.failed",
                "/w/relay",
                Some(("schedule:check-weather", RunKind::Command, "/w/relay")),
                1,
            ),
            event("run.failed", "/w/api", Some(("manual", RunKind::Shell, "/w/api")), 2),
            // The rule's own failure is not news to it.
            event("run.failed", "/w", Some(("reaction:tell-me", RunKind::Agent, "/w")), 1),
        ];
        let asks = vec![
            AskRow {
                id: "d1".into(),
                by_reaction: "advisory".into(),
                workspace: "/w/relay".into(),
                status: DecisionStatus::Taken,
                chosen: vec!["note".into()],
            },
            AskRow {
                id: "d2".into(),
                by_reaction: "advisory".into(),
                workspace: "/w/relay".into(),
                status: DecisionStatus::Open,
                chosen: vec![],
            },
        ];
        let g = build(
            at("2026-09-09T00:00:00Z"),
            Inputs {
                rules: &rules,
                runs: &runs,
                events: &events,
                asks: &asks,
            },
        );

        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "ask:reaction:/w/relay/advisory",
                "event:never.*",
                "reaction:/w/relay/advisory",
                "reaction:global/tell-me",
                "run:ask:reaction:/w/relay/advisory/note",
                "run:reaction:global/tell-me",
                "run:schedule:/w/relay/check-weather",
                "runs:shells:/w/api",
                "schedule:/w/relay/check-weather",
            ],
            "a folder's own runs show only because they set something off"
        );

        // The schedule and what it runs: declared, counted.
        let e = edge(
            &g,
            "schedule:/w/relay/check-weather",
            "run:schedule:/w/relay/check-weather",
        );
        assert_eq!((e.count, e.declared), (2, true));
        let action = g.node("run:schedule:/w/relay/check-weather").unwrap();
        assert_eq!((action.kind, action.name.as_str()), (NodeKind::Shell, "check-weather"));
        assert_eq!((action.runs.total, action.runs.failed), (2, 1));

        // What only the ledger knows: the command raised the alert.
        let e = edge(&g, "run:schedule:/w/relay/check-weather", "reaction:/w/relay/advisory");
        assert_eq!((e.label.as_str(), e.count, e.declared), ("weather.alert", 3, false));

        // The question: one waiting, one answered `note`, whose run is its action's.
        let ask = g.node("ask:reaction:/w/relay/advisory").unwrap();
        assert_eq!(ask.waiting, ["d2"]);
        assert_eq!(
            edge(&g, "reaction:/w/relay/advisory", "ask:reaction:/w/relay/advisory").count,
            2
        );
        let e = edge(
            &g,
            "ask:reaction:/w/relay/advisory",
            "run:ask:reaction:/w/relay/advisory/note",
        );
        assert_eq!((e.label.as_str(), e.count), ("note", 1));
        assert_eq!(g.node("run:ask:reaction:/w/relay/advisory/note").unwrap().runs.total, 1);

        // Across folders: relay's schedule and api's shells both reach a global rule.
        assert_eq!(
            edge(&g, "run:schedule:/w/relay/check-weather", "reaction:global/tell-me").count,
            1
        );
        let e = edge(&g, "runs:shells:/w/api", "reaction:global/tell-me");
        assert_eq!((e.label.as_str(), e.count), ("run.failed", 2));
        let shells = g.node("runs:shells:/w/api").unwrap();
        assert_eq!((shells.runs.total, shells.runs.going), (2, 1));
        assert_eq!(g.node("run:reaction:global/tell-me").unwrap().kind, NodeKind::Agent);
        assert!(
            !g.edges
                .iter()
                .any(|e| e.from == "run:reaction:global/tell-me" && e.to == "reaction:global/tell-me"),
            "no rule hears itself"
        );

        // A pattern nothing raised still has a way in.
        let e = edge(&g, "event:never.*", "reaction:global/tell-me");
        assert_eq!((e.count, e.declared), (0, true));
        assert!(g.node("event:run.failed").is_none(), "run.failed was heard");
    }

    #[test]
    fn emits_wires_a_command_before_it_has_run() {
        let rules = vec![
            schedule(
                "/w/a",
                "probe",
                json!({"every": "1h", "run": "probe", "emits": ["logs.error"]}),
            ),
            reaction("/w/b", "triage", json!({"on": "logs.*", "run": "triage"})),
        ];
        let g = build(
            at("2026-09-09T00:00:00Z"),
            Inputs {
                rules: &rules,
                runs: &[],
                events: &[],
                asks: &[],
            },
        );
        let e = edge(&g, "run:schedule:/w/a/probe", "reaction:/w/b/triage");
        assert_eq!((e.label.as_str(), e.count, e.declared), ("logs.error", 0, true));
        assert!(g.node("event:logs.*").is_none(), "declared counts as heard");
    }
}
