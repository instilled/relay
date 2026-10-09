//! Edges of different sources never run as one line: where two meet, one only crosses the other.

use super::tests::{all, edge, names, node, now};
use super::*;
use rl_proto::graph::NodeKind;

/// Two flows as the daemon wires them: morning-brief's four sources into its brief, and
/// prod-logs' chain.
fn two_flows() -> Graph {
    let step = |flow: &str, s: &str, kind| node(&format!("step:{flow}/{s}"), kind, s, Some("/w/relay"));
    let flow = |name: &str| node(&format!("flow:{name}"), NodeKind::Flow, name, Some("/w/relay"));
    let mb = |s: &str| format!("step:morning-brief/{s}");
    let pl = |s: &str| format!("step:prod-logs/{s}");
    let mut edges = vec![];
    for s in ["days", "gchat", "github", "notion"] {
        edges.push(edge("flow:morning-brief", &mb(s), "", 0));
        edges.push(edge(&mb(s), &mb("brief"), "", 0));
    }
    edges.push(edge("flow:prod-logs", &pl("probe"), "", 0));
    edges.push(edge(&pl("probe"), &pl("triage"), "", 0));
    edges.push(edge(&pl("triage"), &pl("review"), "", 0));
    Graph {
        since: now(),
        nodes: vec![
            flow("morning-brief"),
            flow("prod-logs"),
            step("morning-brief", "brief", NodeKind::Agent),
            step("morning-brief", "days", NodeKind::Shell),
            step("morning-brief", "gchat", NodeKind::Shell),
            step("morning-brief", "github", NodeKind::Shell),
            step("morning-brief", "notion", NodeKind::Shell),
            step("prod-logs", "probe", NodeKind::Shell),
            step("prod-logs", "review", NodeKind::Ask),
            step("prod-logs", "triage", NodeKind::Agent),
        ],
        edges,
    }
}

/// The cells a route's line marks, and the sides it leaves each by.
type Marks = HashMap<(usize, usize), u8>;

/// What `draw` marks of a route: its runs across, `(x0, x1, y)`, and down, `(y0, y1, x)`.
fn marks(r: &Route) -> Marks {
    let (flat, tall) = match r.back {
        None => (
            vec![(r.sx, r.lane, r.sy), (r.lane, r.tx, r.ty)],
            vec![(r.sy, r.ty, r.lane)],
        ),
        Some((yb, down)) => (
            vec![(r.sx, down, r.sy), (down, r.lane, yb), (r.lane, r.tx, r.ty)],
            vec![(r.sy, yb, down), (yb, r.ty, r.lane)],
        ),
    };
    let mut m = Marks::new();
    for (x0, x1, y) in flat {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            *m.entry((x, y)).or_default() |= (u8::from(x > a) * LEFT) | (u8::from(x < b) * RIGHT);
        }
    }
    for (y0, y1, x) in tall {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            *m.entry((x, y)).or_default() |= (u8::from(y > a) * UP) | (u8::from(y < b) * DOWN);
        }
    }
    m
}

/// The flow a node of `two_flows` is of.
fn flow_of(id: &str) -> &str {
    let rest = id.split_once(':').map_or(id, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// Asserts no two of `g`'s edges run as one line, and two flows' edges never meet.
fn apart(g: &Graph) {
    let p = plan(g, &all(g), &names(), now());
    let text = draw(g, &p, &names(), None, now()).plain();
    let drawn: Vec<(&Route, Marks)> = p.routes.iter().map(|r| (r, marks(r))).collect();
    for (a, am) in &drawn {
        for (b, bm) in &drawn {
            if a.from >= b.from {
                continue;
            }
            let apart = flow_of(&a.from) != flow_of(&b.from);
            for (cell, x) in am {
                let Some(y) = bm.get(cell) else { continue };
                let what = format!("{} → {} and {} → {} at {cell:?}", a.from, a.to, b.from, b.to);
                // Two flows' edges do not even cross: at a glance a crossing reads as one feeding
                // the other.
                assert!(!apart, "{what}: two flows' edges meet:\n{text}");
                let crossing = (*x == LEFT | RIGHT && *y == UP | DOWN) || (*x == UP | DOWN && *y == LEFT | RIGHT);
                assert!(crossing, "{what} run as one line:\n{text}");
            }
        }
    }
    // Each flow keeps to rows of its own.
    let rows = |flow: &str| {
        let b: Vec<&Placed> = p.boxes.iter().filter(|b| flow_of(&b.id) == flow).collect();
        (
            b.iter().map(|b| b.y).min().unwrap(),
            b.iter().map(|b| b.y + b.h).max().unwrap(),
        )
    };
    let (mb, pl) = (rows("morning-brief"), rows("prod-logs"));
    assert!(mb.1 <= pl.0 || pl.1 <= mb.0, "{mb:?} and {pl:?} overlap:\n{text}");
}

#[test]
fn two_flows_edges_never_share_a_line() {
    apart(&two_flows());
}

#[test]
fn a_flows_way_back_keeps_to_its_own_rows() {
    let mut g = two_flows();
    let back = edge("step:morning-brief/brief", "flow:morning-brief", "run.failed", 1);
    g.edges.push(back);
    let p = plan(&g, &all(&g), &names(), now());
    assert!(p.routes.iter().any(|r| r.back.is_some()));
    apart(&g);
}
