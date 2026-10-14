//! What a node of the wiring graph says: its glyph and colour, the two lines in its box, what a
//! search matches it on, and the pane on it. The layout and the drawing are `graph`'s.

use crate::tui::query::{Named, Query, Subject};
use crate::tui::rows::Names;
use crate::tui::theme;
use jiff::Timestamp;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use rl_core::time::fmt_age;
use rl_proto::flow::FlowStatus;
use rl_proto::graph::{Graph, Node, NodeKind};
use rl_proto::types::{Checkout, RunState};
use std::collections::{HashSet, VecDeque};

pub const AGENT: Color = Color::Rgb(0xB0, 0x5C, 0xC8); // magenta

/// One column wide each, every one: the grid is a cell per character, and an
/// emoji-width glyph (`⚡`, `⏱`) would push a box's border out of line.
pub fn glyph(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Flow => "↻",
        NodeKind::Ask => "◇",
        NodeKind::Shell => "❯",
        NodeKind::Agent => "✦",
        NodeKind::Event => "◆",
        NodeKind::Outside => "⇢",
    }
}

/// The colour a kind of node wears.
pub fn colour(kind: NodeKind) -> Style {
    match kind {
        NodeKind::Flow => Style::default().fg(theme::LIVE),
        NodeKind::Ask => Style::default().fg(theme::NEEDS),
        NodeKind::Shell => Style::default().fg(theme::ACCENT),
        NodeKind::Agent => Style::default().fg(AGENT),
        NodeKind::Event | NodeKind::Outside => theme::dim(),
    }
}

/// The nodes the filter — a search, see `query` — keeps: those that match,
/// and everything upstream and downstream of them, so a folder's filter still
/// shows what reaches it from elsewhere and where it leads.
pub fn keep(g: &Graph, filter: &str, names: &Names, checkouts: &[Checkout]) -> HashSet<String> {
    let all = || g.nodes.iter().map(|n| n.id.clone()).collect();
    let query = Query::parse(filter);
    if query.is_empty() {
        return all();
    }
    let hit = |n: &Node| {
        let folder = folder_word(n, names);
        let on = n
            .folder
            .as_deref()
            .and_then(|f| crate::tui::rows::checkout_of(checkouts, f))
            .and_then(|c| c.branch.as_deref());
        query.hits(&Subject {
            on,
            ..node_subject(n, &folder)
        })
    };
    let mut kept: HashSet<String> = HashSet::new();
    for dir in [true, false] {
        let mut queue: VecDeque<&str> = g.nodes.iter().filter(|n| hit(n)).map(|n| n.id.as_str()).collect();
        let mut seen: HashSet<&str> = queue.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            kept.insert(id.to_string());
            for e in &g.edges {
                let (from, to) = if dir { (&e.from, &e.to) } else { (&e.to, &e.from) };
                if from == id && seen.insert(to) {
                    queue.push_back(to);
                }
            }
        }
    }
    kept
}

/// What a search matches a node on: its name and detail, its folder, how its
/// runs went, what kind of node it is, and the flow it is, or is a step of.
fn node_subject<'a>(n: &'a Node, folder: &'a str) -> Subject<'a> {
    let mut is = Vec::new();
    match n.runs.last {
        Some(RunState::Failed) => is.push("failed"),
        Some(RunState::Finished) => is.push("done"),
        Some(RunState::Stopped) => is.push("stopped"),
        _ => {}
    }
    if n.runs.going > 0 {
        is.push("going");
    }
    if !n.waiting.is_empty() || n.status == Some(FlowStatus::NeedsApproval) {
        is.push("needs");
    }
    let kind = match n.kind {
        NodeKind::Flow => vec!["flow"],
        NodeKind::Ask if is_step(n) => vec!["step", "question"],
        NodeKind::Ask => vec!["question"],
        NodeKind::Shell if is_step(n) => vec!["step", "shell", "command"],
        NodeKind::Shell => vec!["shell", "command"],
        NodeKind::Agent if is_step(n) => vec!["step", "agent"],
        NodeKind::Agent => vec!["agent"],
        NodeKind::Event => vec!["event"],
        NodeKind::Outside => vec!["outside"],
    };
    Subject {
        text: vec![&n.name, &n.detail],
        project: vec![folder, n.folder.as_deref().and_then(|p| p.to_str()).unwrap_or("")],
        is,
        kind,
        by: flow_name(n).into_iter().collect(),
        on: None,
    }
}

/// The flow a node is, or is a step of.
pub fn flow_name(n: &Node) -> Option<&str> {
    n.flow.as_deref()
}

/// A node as the search ranks it (`query::ranked`): its name, its folder,
/// its detail.
pub fn named(n: &Node, names: &Names) -> Named {
    Named {
        title: n.name.clone(),
        project: folder_word(n, names),
        summary: n.detail.clone(),
        rest: n.folder.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        ..Named::default()
    }
}

/// Where a node is, in words: its folder's name, `global` for a flow with
/// no `in`, nothing for an event.
fn folder_word(n: &Node, names: &Names) -> String {
    match (&n.folder, n.kind) {
        (Some(p), _) => names.workspace(p),
        (None, NodeKind::Event) => String::new(),
        (None, _) => "global".into(),
    }
}

/// The two lines inside a node's box.
pub fn texts(n: &Node, names: &Names, now: Timestamp) -> (String, String) {
    let mark = if n.status == Some(FlowStatus::NeedsApproval) {
        "! "
    } else {
        ""
    };
    let top = format!("{} {mark}{}", glyph(n.kind), title(n));
    let what = match n.kind {
        NodeKind::Flow => match (n.status, n.next_at) {
            (Some(FlowStatus::On), Some(t)) if t > now => {
                format!("{} · in {}", trigger_words(&n.detail), fmt_age(now, t))
            }
            (Some(FlowStatus::On), _) => trigger_words(&n.detail),
            (Some(s), _) => s.to_string(),
            (None, _) => String::new(),
        },
        NodeKind::Ask => match n.waiting.len() {
            0 if is_step(n) => "asks".into(),
            0 => String::new(),
            k => format!("{k} waiting"),
        },
        NodeKind::Shell | NodeKind::Agent => tally(n),
        NodeKind::Event => "nothing raised it yet".into(),
        NodeKind::Outside => "raised outside a run".into(),
    };
    let place = if n.kind == NodeKind::Flow || is_step(n) {
        String::new()
    } else {
        folder_word(n, names)
    };
    let bottom = match (place.is_empty(), what.is_empty()) {
        (true, _) => what,
        (false, true) => place,
        (false, false) => format!("{place} · {what}"),
    };
    (top, bottom)
}

/// What starts a flow, as its box says it — each part of `when()` on its own: `◷ every 30m`,
/// `◷ at 07:30, Mon–Fri`, `◆ run.failed`, `by hand`.
pub fn trigger_words(when: &str) -> String {
    if when.is_empty() {
        return String::new();
    }
    when.split(" or ")
        .map(|part| match part.strip_prefix("on ") {
            Some(kinds) => format!("◆ {kinds}"),
            None if part == "by hand" => part.to_string(),
            None => format!("◷ {}", crate::tui::flows::when_words(part)),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A flow's step, rather than a folder's runs or a flow itself.
fn is_step(n: &Node) -> bool {
    n.id.starts_with("step:")
}

/// What a node is called where it is drawn: an ask step by the question it
/// put, once it has put one; anything else by its name.
fn title(n: &Node) -> &str {
    match n.kind {
        NodeKind::Ask if is_step(n) && !n.detail.is_empty() => &n.detail,
        _ => &n.name,
    }
}

/// `ok ×5 · 1 failed`, `2 going`.
fn tally(n: &Node) -> String {
    let t = &n.runs;
    if t.total == 0 {
        return "not run yet".into();
    }
    let mut parts = Vec::new();
    if t.going > 0 {
        parts.push(format!("{} going", t.going));
    }
    let ok = t.total - t.failed - t.going;
    if ok > 0 {
        parts.push(format!("ok ×{ok}"));
    }
    if t.failed > 0 {
        parts.push(format!("{} failed", t.failed));
    }
    parts.join(" · ")
}

/// A kind of node, in a word.
fn kind_word(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Flow => "flow",
        NodeKind::Ask => "question",
        NodeKind::Shell => "command",
        NodeKind::Agent => "agent",
        NodeKind::Event => "event",
        NodeKind::Outside => "outside",
    }
}

/// What the pane says about the selected node: what it is, its flow as the
/// list's pane would show it, what it is wired to either side, and — on a
/// question — the first one waiting, numbered to answer.
#[allow(clippy::too_many_arguments)]
pub fn pane_lines(
    g: &Graph,
    n: &Node,
    flow: Option<&rl_proto::flow::Flow>,
    firing: Option<&rl_proto::flow::Firing>,
    waiting: Option<(&rl_proto::decision::Decision, Option<&rl_proto::types::Run>)>,
    picked: &[usize],
    at: Option<usize>,
    width: usize,
    names: &Names,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", glyph(n.kind)), colour(n.kind)),
        Span::styled(title(n).to_string(), theme::name()),
    ])];
    let place = folder_word(n, names);
    let mut what = match (&n.flow, is_step(n)) {
        // An ask step drawn by its question still says which step it is.
        (Some(flow), true) if n.kind == NodeKind::Ask && title(n) != n.name => format!("step {} of {flow}", n.name),
        (Some(flow), true) => format!("step of {flow}"),
        _ => kind_word(n.kind).to_string(),
    };
    if !place.is_empty() {
        what.push_str(&format!(" · {place}"));
    }
    lines.push(Line::from(Span::styled(format!(" {what}"), theme::dim())));
    // An ask step's detail is its question, which the waiting question's lines already say.
    if is_step(n) && !n.detail.is_empty() && n.kind != NodeKind::Ask {
        lines.push(Line::raw(format!(" {:<8} {}", "does", n.detail)));
    }
    if matches!(n.kind, NodeKind::Shell | NodeKind::Agent) {
        let mut spans = vec![Span::raw(format!(" {}", tally(n)))];
        if let Some(at) = n.runs.last_at {
            spans.push(Span::raw(format!(" · latest {} ago", fmt_age(at, now))));
        }
        spans.push(Span::styled(format!(" · past {}", fmt_age(g.since, now)), theme::dim()));
        lines.push(Line::from(spans));
    }
    if let Some((q, asker)) = waiting {
        lines.push(Line::raw(""));
        let more = n.waiting.len().saturating_sub(1);
        lines.extend(
            crate::tui::pane::question_lines(
                q,
                asker,
                firing.filter(|f| f.id == q.chain),
                None,
                picked,
                at,
                width,
                now,
            )
            .lines,
        );
        if more > 0 {
            lines.push(Line::from(Span::styled(
                format!(" and {more} more waiting"),
                theme::dim(),
            )));
        }
    }
    if let Some(f) = flow {
        lines.push(Line::raw(""));
        match serde_json::from_value::<rl_flow::def::FlowDef>(f.def.clone()) {
            Ok(def) if n.kind == NodeKind::Flow => {
                lines.extend(crate::tui::flows::pane_lines(
                    f,
                    &def,
                    firing,
                    None,
                    &Default::default(),
                    names,
                    width,
                    now,
                ));
            }
            _ => lines.push(Line::from(vec![
                Span::styled(" from ", theme::dim()),
                Span::raw(format!("flow {}", f.name)),
            ])),
        }
    }
    let side = |into: bool| -> Vec<Line<'static>> {
        g.edges
            .iter()
            .filter(|e| if into { e.to == n.id } else { e.from == n.id })
            .filter_map(|e| {
                let other = g.node(if into { &e.from } else { &e.to })?;
                let mut label = e.label.clone();
                if e.count > 0 {
                    label = if label.is_empty() {
                        format!("×{}", e.count)
                    } else {
                        format!("{label} ×{}", e.count)
                    };
                }
                Some(Line::from(vec![
                    Span::styled(if into { "  ← " } else { "  → " }, theme::dim()),
                    Span::styled(format!("{} ", glyph(other.kind)), colour(other.kind)),
                    Span::raw(title(other).to_string()),
                    Span::styled(
                        if label.is_empty() {
                            String::new()
                        } else {
                            format!("  {label}")
                        },
                        theme::dim(),
                    ),
                ]))
            })
            .collect()
    };
    let (ins, outs) = (side(true), side(false));
    if !ins.is_empty() || !outs.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(ins);
        lines.extend(outs);
    }
    lines
}

/// The headline over the graph.
pub fn headline(g: &Graph, kept: usize, filter: &str) -> String {
    let flows = g.nodes.iter().filter(|n| n.kind == NodeKind::Flow).count();
    let waiting: usize = g.nodes.iter().map(|n| n.waiting.len()).sum();
    let mut s = format!("wiring · {flows} flow{}", if flows == 1 { "" } else { "s" });
    if waiting > 0 {
        s.push_str(&format!(" · {waiting} waiting"));
    }
    if !filter.is_empty() {
        s.push_str(&format!(" · {kept} of {} · ⌕ {filter}", g.nodes.len()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::graph::{Edge, Tally};
    use std::path::PathBuf;

    fn node(id: &str, kind: NodeKind, name: &str, folder: Option<&str>) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            folder: folder.map(PathBuf::from),
            status: Some(FlowStatus::On),
            detail: String::new(),
            next_at: None,
            runs: Tally::default(),
            waiting: vec![],
            flow: None,
        }
    }

    fn edge(from: &str, to: &str, label: &str, count: u64) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            label: label.into(),
            count,
            declared: count == 0,
        }
    }

    fn now() -> Timestamp {
        "2026-09-16T12:00:00Z".parse().unwrap()
    }

    fn weather() -> Graph {
        Graph {
            since: now(),
            nodes: vec![
                node("s", NodeKind::Flow, "check-weather", Some("/w/relay")),
                node("s.run", NodeKind::Shell, "check-weather", Some("/w/relay")),
                node("r", NodeKind::Flow, "advisory", Some("/w/relay")),
                node("a", NodeKind::Ask, "add a note?", Some("/w/relay")),
                node("a.note", NodeKind::Shell, "echo >> notes", Some("/w/relay")),
                node("shells", NodeKind::Shell, "shells", Some("/w/api")),
                node("t", NodeKind::Flow, "tell-me", None),
            ],
            edges: vec![
                edge("s", "s.run", "", 6),
                edge("s.run", "r", "weather.alert", 3),
                edge("r", "a", "", 3),
                edge("a", "a.note", "note", 2),
                edge("s.run", "t", "run.failed", 1),
                edge("shells", "t", "run.failed", 4),
            ],
        }
    }

    fn names() -> Names {
        Names::new([
            (PathBuf::from("/w/relay"), "relay".to_string()),
            (PathBuf::from("/w/api"), "api".to_string()),
        ])
    }

    #[test]
    fn the_pane_says_what_a_node_is_and_what_it_is_wired_to() {
        let g = weather();
        let r = g.node("r").unwrap();
        let text: Vec<String> = pane_lines(&g, r, None, None, None, &[], None, 80, &names(), now())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], " ↻ advisory");
        assert_eq!(text[1], " flow · relay");
        assert!(
            text.iter().any(|l| l == "  ← ❯ check-weather  weather.alert ×3"),
            "{text:#?}"
        );
        assert!(text.iter().any(|l| l == "  → ◇ add a note?  ×3"), "{text:#?}");
    }

    #[test]
    fn a_node_is_found_by_the_flow_it_is_or_is_a_step_of() {
        let of = |n: Node, flow: &str| Node {
            flow: Some(flow.into()),
            ..n
        };
        let g = Graph {
            since: now(),
            nodes: vec![
                of(
                    node("flow:nightly", NodeKind::Flow, "nightly", Some("/w/relay")),
                    "nightly",
                ),
                of(
                    node("step:nightly/review", NodeKind::Ask, "review", Some("/w/relay")),
                    "nightly",
                ),
                node("runs:shell:/w/api", NodeKind::Shell, "shells", Some("/w/api")),
            ],
            edges: vec![edge("flow:nightly", "step:nightly/review", "", 1)],
        };
        let kept = keep(&g, "by:nightly", &names(), &[]);
        assert!(
            kept.contains("flow:nightly") && kept.contains("step:nightly/review"),
            "{kept:?}"
        );
        assert!(!kept.contains("runs:shell:/w/api"));
        assert_eq!(flow_name(g.node("step:nightly/review").unwrap()), Some("nightly"));
        assert_eq!(flow_name(g.node("runs:shell:/w/api").unwrap()), None);
        assert_eq!(headline(&g, 3, ""), "wiring · 1 flow");
    }

    #[test]
    fn a_flows_box_says_its_trigger_and_a_steps_how_it_went() {
        let names = names();
        let mut f = node("flow:prod-logs", NodeKind::Flow, "prod-logs", Some("/w/relay"));
        f.flow = Some("prod-logs".into());
        f.detail = "every 30m".into();
        assert_eq!(texts(&f, &names, now()), ("↻ prod-logs".into(), "◷ every 30m".into()));
        f.next_at = Some(now() + jiff::SignedDuration::from_mins(12));
        assert_eq!(texts(&f, &names, now()).1, "◷ every 30m · in 12m");
        f.next_at = None;
        f.detail = "30 7 * * 1-5 or on run.failed".into();
        assert_eq!(texts(&f, &names, now()).1, "◷ at 07:30, Mon–Fri · ◆ run.failed");
        f.detail = "by hand".into();
        assert_eq!(texts(&f, &names, now()).1, "by hand");
        f.status = Some(FlowStatus::NeedsApproval);
        assert_eq!(texts(&f, &names, now()).1, "needs approval");

        let mut s = node("step:prod-logs/triage", NodeKind::Agent, "triage", Some("/w/relay"));
        s.flow = Some("prod-logs".into());
        s.runs = Tally {
            total: 3,
            failed: 1,
            ..Default::default()
        };
        assert_eq!(
            texts(&s, &names, now()),
            ("✦ triage".into(), "ok ×2 · 1 failed".into()),
            "no folder: its flow's"
        );
        let mut r = node("step:prod-logs/review", NodeKind::Ask, "review", Some("/w/relay"));
        r.flow = Some("prod-logs".into());
        assert_eq!(texts(&r, &names, now()).1, "asks");
        r.waiting = vec!["d1".into()];
        assert_eq!(texts(&r, &names, now()).1, "1 waiting");
    }

    #[test]
    fn an_ask_steps_box_says_its_question() {
        let mut r = node("step:prod-logs/review", NodeKind::Ask, "review", Some("/w/relay"));
        r.flow = Some("prod-logs".into());
        assert_eq!(texts(&r, &names(), now()).0, "◇ review", "nothing raised yet: its name");
        // As rld sends it once raised: the question, someone waiting on it.
        r.detail = "restart the api pod?".into();
        r.waiting = vec!["d1".into()];
        assert_eq!(
            texts(&r, &names(), now()),
            ("◇ restart the api pod?".into(), "1 waiting".into())
        );
        let g = Graph {
            since: now(),
            nodes: vec![
                node("flow:prod-logs", NodeKind::Flow, "prod-logs", Some("/w/relay")),
                r.clone(),
            ],
            edges: vec![edge("flow:prod-logs", "step:prod-logs/review", "", 0)],
        };
        let flow = g.node("flow:prod-logs").unwrap();
        let text: Vec<String> = pane_lines(&g, flow, None, None, None, &[], None, 80, &names(), now())
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert!(text.iter().any(|l| l == "  → ◇ restart the api pod?"), "{text:#?}");
        let text: Vec<String> = pane_lines(&g, &r, None, None, None, &[], None, 80, &names(), now())
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(text[0], " ◇ restart the api pod?");
        assert_eq!(text[1], " step review of prod-logs · relay");
        assert!(!text.iter().any(|l| l.starts_with(" does")), "{text:#?}");
    }

    #[test]
    fn a_steps_pane_says_its_flow_and_what_it_does() {
        let mut s = node("step:prod-logs/triage", NodeKind::Agent, "triage", Some("/w/relay"));
        s.flow = Some("prod-logs".into());
        s.detail = "claude · look".into();
        let g = Graph {
            since: now(),
            nodes: vec![
                node("flow:prod-logs", NodeKind::Flow, "prod-logs", Some("/w/relay")),
                s.clone(),
            ],
            edges: vec![edge("flow:prod-logs", "step:prod-logs/triage", "if probe.out", 0)],
        };
        let text: Vec<String> = pane_lines(&g, &s, None, None, None, &[], None, 80, &names(), now())
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(text[0], " ✦ triage");
        assert_eq!(text[1], " step of prod-logs · relay");
        assert!(text.iter().any(|l| l == " does     claude · look"), "{text:#?}");
        assert!(text.iter().any(|l| l == "  ← ↻ prod-logs  if probe.out"), "{text:#?}");
    }
}
