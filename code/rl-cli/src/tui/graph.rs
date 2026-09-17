//! The wiring graph, laid out and drawn on a grid of characters. Pure: a
//! `Graph` from `graph.get` in, a canvas of styled cells out.
//!
//! Laid out in layers, the way `dot` does it, because that is what reads on
//! a terminal: a node's column comes from what feeds it (one to the right of
//! its furthest source), not from what kind of node it is. Edges that close a
//! loop are set aside for ranking and drawn back along the bottom. Within a
//! column, nodes are ordered by where their neighbours sit, a few sweeps each
//! way, to untangle crossings; each is placed level with what feeds it where
//! there is room. Edges run right-angled: out of the source's side, down or
//! up a lane of their own in the gap before the target, and into the target
//! on a row of their own, where the event kinds they carry are written.

use super::query::{Query, Subject};
use super::rows::Names;
use super::theme;
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rl_core::time::fmt_age;
use rl_proto::graph::{Graph, Node, NodeKind};
use rl_proto::rule::RuleStatus;
use rl_proto::types::RunState;
use std::collections::{HashMap, HashSet, VecDeque};

/// Widest a node's box grows, and narrowest.
const BOX_MIN: usize = 16;
const BOX_MAX: usize = 30;
/// Longest an edge label is written before it is cut.
const LABEL_MAX: usize = 22;

pub const AGENT: Color = Color::Rgb(0xB0, 0x5C, 0xC8); // magenta
pub const REACTION: Color = Color::Rgb(0xC9, 0xB2, 0x2E); // yellow

/// One column wide each, every one: the grid is a cell per character, and an
/// emoji-width glyph (`⚡`, `⏱`) would push a box's border out of line.
pub fn glyph(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Schedule => "◷",
        NodeKind::Reaction => "↯",
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
        NodeKind::Schedule => Style::default().fg(theme::LIVE),
        NodeKind::Reaction => Style::default().fg(REACTION),
        NodeKind::Ask => Style::default().fg(theme::NEEDS),
        NodeKind::Shell => Style::default().fg(theme::ACCENT),
        NodeKind::Agent => Style::default().fg(AGENT),
        NodeKind::Event | NodeKind::Outside => theme::dim(),
    }
}

/// The nodes the filter — a search, see `query` — keeps: those that match,
/// and everything upstream and downstream of them, so a folder's filter still
/// shows what reaches it from elsewhere and where it leads.
pub fn keep(g: &Graph, filter: &str, names: &Names) -> HashSet<String> {
    let all = || g.nodes.iter().map(|n| n.id.clone()).collect();
    let query = Query::parse(filter);
    if query.is_empty() {
        return all();
    }
    let hit = |n: &Node| {
        let folder = folder_word(n, names);
        query.hits(&node_subject(n, &folder))
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
/// runs went, what kind of node it is, and the rule it belongs to.
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
    if !n.waiting.is_empty() || n.status == Some(RuleStatus::NeedsApproval) {
        is.push("needs");
    }
    let kind = match n.kind {
        NodeKind::Schedule => vec!["job", "schedule"],
        NodeKind::Reaction => vec!["reaction"],
        NodeKind::Ask => vec!["question"],
        NodeKind::Shell => vec!["shell", "command"],
        NodeKind::Agent => vec!["agent"],
        NodeKind::Event => vec!["event"],
        NodeKind::Outside => vec!["outside"],
    };
    Subject {
        text: vec![&n.name, &n.detail],
        project: vec![folder, n.folder.as_deref().and_then(|p| p.to_str()).unwrap_or("")],
        is,
        kind,
        by: n.rule.as_deref().map(|r| r.rsplit('/').next().unwrap_or(r)),
    }
}

/// Where a node is, in words: its folder's name, `global` for a rule of the
/// Relay home, nothing for an event.
fn folder_word(n: &Node, names: &Names) -> String {
    match (&n.folder, n.kind) {
        (Some(p), _) => names.workspace(p),
        (None, NodeKind::Event) => String::new(),
        (None, _) => "global".into(),
    }
}

/// The two lines inside a node's box.
pub fn texts(n: &Node, names: &Names, now: Timestamp) -> (String, String) {
    let mark = if n.status == Some(RuleStatus::NeedsApproval) {
        "! "
    } else {
        ""
    };
    let top = format!("{} {mark}{}", glyph(n.kind), n.name);
    let what = match n.kind {
        NodeKind::Schedule => match (n.status, n.next_at) {
            (Some(RuleStatus::On), Some(t)) if t > now => format!("in {}", fmt_age(now, t)),
            (Some(RuleStatus::On), _) => n.detail.clone(),
            (Some(s), _) => s.to_string(),
            (None, _) => String::new(),
        },
        NodeKind::Reaction => match n.status {
            Some(RuleStatus::On) | None => n.detail.clone(),
            Some(s) => s.to_string(),
        },
        NodeKind::Ask => match n.waiting.len() {
            0 => String::new(),
            k => format!("{k} waiting"),
        },
        NodeKind::Shell | NodeKind::Agent => tally(n),
        NodeKind::Event => "nothing raised it yet".into(),
        NodeKind::Outside => "raised outside a run".into(),
    };
    let place = folder_word(n, names);
    let bottom = match (place.is_empty(), what.is_empty()) {
        (true, _) => what,
        (false, true) => place,
        (false, false) => format!("{place} · {what}"),
    };
    (top, bottom)
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

/// A node's box on the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: String,
    pub rank: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// An edge's path: out of `from` at `(sx, sy)`, along `lane`, into `to` at
/// `(tx, ty)`. A back edge (`back`: the bottom row, and the lane it drops
/// down just after its source) runs along the bottom and climbs `lane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub from: String,
    pub to: String,
    pub label: String,
    pub observed: bool,
    pub sx: usize,
    pub sy: usize,
    pub lane: usize,
    pub tx: usize,
    pub ty: usize,
    /// Where its label starts: past every lane in the gap, so no label runs
    /// into another edge's turn.
    pub label_x: usize,
    pub back: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub boxes: Vec<Placed>,
    pub routes: Vec<Route>,
    pub width: usize,
    pub height: usize,
}

impl Plan {
    pub fn get(&self, id: &str) -> Option<&Placed> {
        self.boxes.iter().find(|b| b.id == id)
    }

    /// Node ids in reading order: column by column, top to bottom.
    pub fn order(&self) -> Vec<String> {
        let mut b: Vec<&Placed> = self.boxes.iter().collect();
        b.sort_by_key(|p| (p.rank, p.y));
        b.into_iter().map(|p| p.id.clone()).collect()
    }
}

/// Where everything goes.
pub fn plan(g: &Graph, kept: &HashSet<String>, names: &Names, now: Timestamp) -> Plan {
    let nodes: Vec<&Node> = g.nodes.iter().filter(|n| kept.contains(&n.id)).collect();
    if nodes.is_empty() {
        return Plan::default();
    }
    let at: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let edges: Vec<(usize, usize, &rl_proto::graph::Edge)> = g
        .edges
        .iter()
        .filter_map(|e| Some((*at.get(e.from.as_str())?, *at.get(e.to.as_str())?, e)))
        .filter(|(a, b, _)| a != b)
        .collect();
    let n = nodes.len();
    let mut outs: Vec<Vec<usize>> = vec![vec![]; n];
    let mut ins_n = vec![0usize; n];
    for (k, (a, b, _)) in edges.iter().enumerate() {
        outs[*a].push(k);
        ins_n[*b] += 1;
    }

    // Loops: an edge back to a node still on the walk is set aside.
    let mut back = vec![false; edges.len()];
    let mut state = vec![0u8; n]; // 0 new, 1 on the walk, 2 done
    let mut starts: Vec<usize> = (0..n).filter(|&i| ins_n[i] == 0).collect();
    starts.extend(0..n);
    for s in starts {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < outs[v].len() {
                stack.push((v, i + 1));
                let k = outs[v][i];
                let w = edges[k].1;
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => back[k] = true,
                    _ => {}
                }
            } else {
                state[v] = 2;
            }
        }
    }
    let forward: Vec<usize> = (0..edges.len()).filter(|&k| !back[k]).collect();

    // Ranks: one past the furthest thing that feeds a node.
    let mut rank = vec![0usize; n];
    let mut indeg = vec![0usize; n];
    for &k in &forward {
        indeg[edges[k].1] += 1;
    }
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    while let Some(v) = queue.pop_front() {
        for &k in &outs[v] {
            if back[k] {
                continue;
            }
            let w = edges[k].1;
            rank[w] = rank[w].max(rank[v] + 1);
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push_back(w);
            }
        }
    }
    let ranks = rank.iter().max().map_or(0, |m| m + 1);
    let mut cols: Vec<Vec<usize>> = vec![vec![]; ranks];
    for v in 0..n {
        cols[rank[v]].push(v);
    }

    // Order within columns by where neighbours sit, sweeping both ways.
    let pos = |cols: &Vec<Vec<usize>>| {
        let mut p = vec![0f64; n];
        for c in cols {
            for (i, &v) in c.iter().enumerate() {
                p[v] = i as f64;
            }
        }
        p
    };
    for sweep in 0..6 {
        let down = sweep % 2 == 0;
        let p = pos(&cols);
        let range: Vec<usize> = if down {
            (1..ranks).collect()
        } else {
            (0..ranks.saturating_sub(1)).rev().collect()
        };
        for r in range {
            let mut keyed: Vec<(f64, usize)> = cols[r]
                .iter()
                .map(|&v| {
                    let near: Vec<f64> = forward
                        .iter()
                        .filter_map(|&k| {
                            let (a, b, _) = edges[k];
                            match down {
                                true if b == v && rank[a] < r => Some(p[a]),
                                false if a == v && rank[b] > r => Some(p[b]),
                                _ => None,
                            }
                        })
                        .collect();
                    let key = if near.is_empty() {
                        p[v]
                    } else {
                        near.iter().sum::<f64>() / near.len() as f64
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|x, y| x.0.total_cmp(&y.0).then(nodes[x.1].id.cmp(&nodes[y.1].id)));
            cols[r] = keyed.into_iter().map(|(_, v)| v).collect();
        }
    }

    // Sizes.
    let mut ins = vec![0usize; n];
    let mut outn = vec![0usize; n];
    for (a, b, _) in &edges {
        outn[*a] += 1;
        ins[*b] += 1;
    }
    let w: Vec<usize> = nodes
        .iter()
        .map(|nd| {
            let (t, b) = texts(nd, names, now);
            (t.chars().count().max(b.chars().count()) + 4).clamp(BOX_MIN, BOX_MAX)
        })
        .collect();
    let h: Vec<usize> = (0..n).map(|v| 4usize.max(ins[v] + 2).max(outn[v] + 2)).collect();
    let colw: Vec<usize> = cols
        .iter()
        .map(|c| c.iter().map(|&v| w[v]).max().unwrap_or(0))
        .collect();

    // Gaps: a lane per edge that turns in it, and room for the longest label.
    let label = |e: &rl_proto::graph::Edge| -> String {
        let mut s = e.label.clone();
        if e.count > 0 {
            s = if s.is_empty() {
                format!("×{}", e.count)
            } else {
                format!("{s} ×{}", e.count)
            };
        }
        s.chars().take(LABEL_MAX).collect()
    };
    // Gap `r` sits before column `r`.
    let mut lanes: Vec<Vec<usize>> = vec![vec![]; ranks + 1];
    let mut label_w = vec![0usize; ranks + 1];
    let mut down_lanes: Vec<Vec<usize>> = vec![vec![]; ranks + 1];
    for (k, (a, b, e)) in edges.iter().enumerate() {
        lanes[rank[*b]].push(k);
        label_w[rank[*b]] = label_w[rank[*b]].max(label(e).chars().count());
        if back[k] {
            down_lanes[rank[*a] + 1].push(k);
        }
    }
    let gap = |r: usize| -> usize {
        let l = lanes[r].len() + down_lanes[r].len();
        if l == 0 { 2 } else { l + label_w[r] + 4 }
    };
    let mut colx = vec![0usize; ranks];
    let mut x = gap(0);
    for r in 0..ranks {
        colx[r] = x;
        x += colw[r] + gap(r + 1);
    }
    let width = x;

    // Rows: level with what feeds a node, where there is room.
    let mut y = vec![0usize; n];
    for (r, col) in cols.iter().enumerate() {
        let mut next = 0usize;
        for &v in col {
            let feeds: Vec<usize> = forward
                .iter()
                .filter(|&&k| edges[k].1 == v && rank[edges[k].0] < r)
                .map(|&k| y[edges[k].0] + h[edges[k].0] / 2)
                .collect();
            let want = if feeds.is_empty() {
                0
            } else {
                (feeds.iter().sum::<usize>() / feeds.len()).saturating_sub(h[v] / 2)
            };
            y[v] = want.max(next);
            next = y[v] + h[v] + 1;
        }
    }
    let body = (0..n).map(|v| y[v] + h[v]).max().unwrap_or(0);

    // Ports: a row each, in the order the other ends sit, so edges into and
    // out of one box do not cross at its side.
    let centre = |v: usize| y[v] + h[v] / 2;
    let mut in_row = vec![0usize; edges.len()];
    let mut out_row = vec![0usize; edges.len()];
    for (v, &top) in y.iter().enumerate() {
        let mut i: Vec<usize> = (0..edges.len()).filter(|&k| edges[k].1 == v).collect();
        i.sort_by_key(|&k| (centre(edges[k].0), k));
        for (j, k) in i.into_iter().enumerate() {
            in_row[k] = top + 1 + j;
        }
        let mut o: Vec<usize> = (0..edges.len()).filter(|&k| edges[k].0 == v).collect();
        o.sort_by_key(|&k| (centre(edges[k].1), k));
        for (j, k) in o.into_iter().enumerate() {
            out_row[k] = top + 1 + j;
        }
    }

    // Lanes: in the gap before the target, ordered so they cross least.
    let mut lane_x = vec![0usize; edges.len()];
    let mut up_x = vec![0usize; edges.len()];
    let mut down_x = vec![0usize; edges.len()];
    let mut label_at = vec![0usize; ranks];
    for r in 0..ranks {
        let start = if r == 0 { 0 } else { colx[r - 1] + colw[r - 1] };
        let mut here: Vec<usize> = down_lanes[r].clone();
        here.sort();
        let mut next = start + 1;
        for k in here {
            down_x[k] = next;
            next += 1;
        }
        let mut into: Vec<usize> = lanes[r].clone();
        into.sort_by_key(|&k| (in_row[k] as isize - out_row[k] as isize, in_row[k], k));
        for k in into {
            lane_x[k] = next;
            up_x[k] = next;
            next += 1;
        }
        label_at[r] = next + 1;
    }
    let mut bottom = body + 1;
    let routes: Vec<Route> = edges
        .iter()
        .enumerate()
        .map(|(k, (a, b, e))| {
            let back_path = back[k].then(|| {
                let yb = bottom;
                bottom += 1;
                (yb, down_x[k])
            });
            Route {
                from: nodes[*a].id.clone(),
                to: nodes[*b].id.clone(),
                label: label(e),
                observed: e.count > 0,
                sx: colx[rank[*a]] + w[*a],
                sy: out_row[k],
                lane: if back[k] { up_x[k] } else { lane_x[k] },
                tx: colx[rank[*b]] - 1,
                ty: in_row[k],
                label_x: label_at[rank[*b]],
                back: back_path,
            }
        })
        .collect();

    let boxes = (0..n)
        .map(|v| Placed {
            id: nodes[v].id.clone(),
            rank: rank[v],
            x: colx[rank[v]],
            y: y[v],
            w: w[v],
            h: h[v],
        })
        .collect();
    Plan {
        boxes,
        routes,
        width,
        height: bottom,
    }
}

/// Which sides of a cell a line leaves by.
const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Clone)]
struct Cell {
    ch: char,
    lines: u8,
    style: Style,
}

/// The whole graph, drawn. Read out a window of it with [`Canvas::lines`].
pub struct Canvas {
    w: usize,
    h: usize,
    cells: Vec<Cell>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        let blank = Cell {
            ch: ' ',
            lines: 0,
            style: Style::default(),
        };
        Self {
            w,
            h,
            cells: vec![blank; w * h],
        }
    }

    fn at(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        (x < self.w && y < self.h).then(|| &mut self.cells[y * self.w + x])
    }

    fn put(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if let Some(c) = self.at(x, y) {
            c.ch = ch;
            c.lines = 0;
            c.style = style;
        }
    }

    fn text(&mut self, x: usize, y: usize, s: &str, style: Style) {
        for (i, ch) in s.chars().enumerate() {
            self.put(x + i, y, ch, style);
        }
    }

    fn hline(&mut self, x0: usize, x1: usize, y: usize, style: Style) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            let mut m = 0;
            if x > a {
                m |= LEFT;
            }
            if x < b {
                m |= RIGHT;
            }
            self.mark(x, y, m, style);
        }
    }

    fn vline(&mut self, x: usize, y0: usize, y1: usize, style: Style) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            let mut m = 0;
            if y > a {
                m |= UP;
            }
            if y < b {
                m |= DOWN;
            }
            self.mark(x, y, m, style);
        }
    }

    fn mark(&mut self, x: usize, y: usize, m: u8, style: Style) {
        if let Some(c) = self.at(x, y) {
            c.lines |= m;
            // A counted edge shows through a merely declared one.
            if c.style == Style::default() || style != theme::dim() {
                c.style = style;
            }
        }
    }

    /// Joins every line mark into the box-drawing character it makes.
    fn join(&mut self) {
        for c in &mut self.cells {
            if c.lines == 0 {
                continue;
            }
            c.ch = match c.lines {
                x if x == LEFT | RIGHT || x == LEFT || x == RIGHT => '─',
                x if x == UP | DOWN || x == UP || x == DOWN => '│',
                x if x == DOWN | RIGHT => '╭',
                x if x == DOWN | LEFT => '╮',
                x if x == UP | RIGHT => '╰',
                x if x == UP | LEFT => '╯',
                x if x == UP | DOWN | RIGHT => '├',
                x if x == UP | DOWN | LEFT => '┤',
                x if x == LEFT | RIGHT | DOWN => '┬',
                x if x == LEFT | RIGHT | UP => '┴',
                _ => '┼',
            };
        }
    }

    /// Rows `y0..y0+h`, columns `x0..x0+w`, as ratatui lines.
    pub fn lines(&self, x0: usize, y0: usize, w: usize, h: usize) -> Vec<Line<'static>> {
        (y0..y0 + h)
            .map(|y| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut style = Style::default();
                for x in x0..x0 + w {
                    let (ch, st) = match (x < self.w && y < self.h).then(|| &self.cells[y * self.w + x]) {
                        Some(c) => (c.ch, c.style),
                        None => (' ', Style::default()),
                    };
                    if st != style && !run.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut run), style));
                    }
                    style = st;
                    run.push(ch);
                }
                if !run.is_empty() {
                    spans.push(Span::styled(run, style));
                }
                Line::from(spans)
            })
            .collect()
    }

    /// The whole canvas as plain text, trailing space trimmed; for tests.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        (0..self.h)
            .map(|y| {
                let row: String = (0..self.w).map(|x| self.cells[y * self.w + x].ch).collect();
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Draws a plan: edges, their labels and arrows, then the boxes over them.
pub fn draw(g: &Graph, p: &Plan, names: &Names, sel: Option<&str>, now: Timestamp) -> Canvas {
    let mut c = Canvas::new(p.width, p.height);
    let edge_style = |r: &Route| {
        let lit = sel.is_some_and(|s| s == r.from || s == r.to);
        match (lit, r.observed) {
            (true, _) => theme::accent(),
            (false, true) => Style::default(),
            (false, false) => theme::dim(),
        }
    };
    for r in &p.routes {
        let st = edge_style(r);
        match r.back {
            None => {
                c.hline(r.sx, r.lane, r.sy, st);
                c.vline(r.lane, r.sy, r.ty, st);
                c.hline(r.lane, r.tx, r.ty, st);
            }
            Some((yb, down)) => {
                c.hline(r.sx, down, r.sy, st);
                c.vline(down, r.sy, yb, st);
                c.hline(down, r.lane, yb, st);
                c.vline(r.lane, yb, r.ty, st);
                c.hline(r.lane, r.tx, r.ty, st);
            }
        }
    }
    c.join();
    for r in &p.routes {
        let st = edge_style(r);
        c.put(r.tx, r.ty, '▶', st);
        if r.label.is_empty() {
            continue;
        }
        let (x, y, room) = match r.back {
            None => (r.label_x, r.ty, r.tx.saturating_sub(r.label_x + 1)),
            Some((yb, down)) => (r.lane + 2, yb, down.saturating_sub(r.lane + 3)),
        };
        let text: String = r.label.chars().take(room).collect();
        if !text.is_empty() {
            let st = if st == theme::dim() {
                st
            } else {
                st.add_modifier(Modifier::DIM)
            };
            c.text(
                x,
                y,
                &text,
                if sel.is_some_and(|s| s == r.from || s == r.to) {
                    theme::accent()
                } else {
                    st
                },
            );
        }
    }
    for b in &p.boxes {
        let Some(n) = g.node(&b.id) else { continue };
        let selected = sel == Some(b.id.as_str());
        let mut border = colour(n.kind);
        if n.runs.last == Some(RunState::Failed) {
            border = Style::default().fg(theme::BAD);
        }
        if n.status == Some(RuleStatus::Paused) {
            border = theme::dim();
        }
        if selected {
            border = border.add_modifier(Modifier::BOLD);
        }
        let fill = if selected { theme::selected() } else { Style::default() };
        let (x, y, w, h) = (b.x, b.y, b.w, b.h);
        for yy in y..y + h {
            for xx in x..x + w {
                c.put(xx, yy, ' ', fill);
            }
        }
        c.put(x, y, '╭', border);
        c.put(x + w - 1, y, '╮', border);
        c.put(x, y + h - 1, '╰', border);
        c.put(x + w - 1, y + h - 1, '╯', border);
        for xx in x + 1..x + w - 1 {
            c.put(xx, y, '─', border);
            c.put(xx, y + h - 1, '─', border);
        }
        for yy in y + 1..y + h - 1 {
            c.put(x, yy, '│', border);
            c.put(x + w - 1, yy, '│', border);
        }
        let (top, bottom) = texts(n, names, now);
        let fit = |s: &str| -> String {
            let room = w - 4;
            if s.chars().count() <= room {
                s.to_string()
            } else {
                let mut t: String = s.chars().take(room.saturating_sub(1)).collect();
                t.push('…');
                t
            }
        };
        let name_style = match (n.status, n.waiting.is_empty()) {
            (Some(RuleStatus::NeedsApproval), _) | (_, false) => theme::needs(),
            _ => colour(n.kind).add_modifier(Modifier::BOLD),
        };
        c.text(
            x + 2,
            y + 1,
            &fit(&top),
            if selected {
                fill.add_modifier(Modifier::BOLD)
            } else {
                name_style
            },
        );
        c.text(x + 2, y + 2, &fit(&bottom), if selected { fill } else { theme::dim() });
    }
    c
}

/// Which way an arrow moves the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Where an arrow goes from the selected node. Left and right follow the
/// wiring — to what feeds it, or what it feeds — picking the one nearest in
/// height; up and down stay in the column, and reach the nearest box above
/// or below elsewhere when the column has none. Nothing that way: stay.
pub fn step(p: &Plan, sel: Option<&str>, dir: Dir) -> Option<String> {
    let Some(cur) = sel.and_then(|s| p.get(s)) else {
        return p.order().into_iter().next();
    };
    let mid = |b: &Placed| b.y as isize + b.h as isize / 2;
    let centre = |b: &Placed| b.x as isize + b.w as isize / 2;
    let nearest = |cands: Vec<&Placed>| {
        cands
            .into_iter()
            .min_by_key(|b| ((mid(b) - mid(cur)).abs(), (centre(b) - centre(cur)).abs(), b.id.clone()))
            .map(|b| b.id.clone())
    };
    match dir {
        Dir::Left | Dir::Right => {
            let ids: Vec<&str> = p
                .routes
                .iter()
                .filter_map(|r| match dir {
                    Dir::Right if r.from == cur.id => Some(r.to.as_str()),
                    Dir::Left if r.to == cur.id => Some(r.from.as_str()),
                    _ => None,
                })
                .collect();
            nearest(p.boxes.iter().filter(|b| ids.contains(&b.id.as_str())).collect())
        }
        Dir::Up | Dir::Down => {
            let past = |b: &Placed| match dir {
                Dir::Up => mid(b) < mid(cur),
                _ => mid(b) > mid(cur),
            };
            let column: Vec<&Placed> = p.boxes.iter().filter(|b| b.rank == cur.rank && past(b)).collect();
            if column.is_empty() {
                nearest(p.boxes.iter().filter(|b| past(b)).collect())
            } else {
                nearest(column)
            }
        }
    }
}

/// A kind of node, in a word.
fn kind_word(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Schedule => "schedule",
        NodeKind::Reaction => "reaction",
        NodeKind::Ask => "question",
        NodeKind::Shell => "command",
        NodeKind::Agent => "agent",
        NodeKind::Event => "event",
        NodeKind::Outside => "outside",
    }
}

/// What the pane says about the selected node: what it is, its rule as the
/// rules list would show it, what it is wired to either side, and — on a
/// question — the first one waiting, numbered to answer.
#[allow(clippy::too_many_arguments)]
pub fn pane_lines(
    g: &Graph,
    n: &Node,
    rule: Option<&rl_proto::rule::Rule>,
    last_run: Option<&str>,
    waiting: Option<&rl_proto::decision::Decision>,
    picked: &[usize],
    names: &Names,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", glyph(n.kind)), colour(n.kind)),
        Span::styled(n.name.clone(), theme::name()),
    ])];
    let place = folder_word(n, names);
    let mut what = kind_word(n.kind).to_string();
    if !place.is_empty() {
        what.push_str(&format!(" · {place}"));
    }
    lines.push(Line::from(Span::styled(format!(" {what}"), theme::dim())));
    if matches!(n.kind, NodeKind::Shell | NodeKind::Agent) {
        let mut spans = vec![Span::raw(format!(" {}", tally(n)))];
        if let Some(at) = n.runs.last_at {
            spans.push(Span::raw(format!(" · latest {} ago", fmt_age(at, now))));
        }
        spans.push(Span::styled(format!(" · past {}", fmt_age(g.since, now)), theme::dim()));
        lines.push(Line::from(spans));
    }
    if let Some(q) = waiting {
        lines.push(Line::raw(""));
        let more = n.waiting.len().saturating_sub(1);
        lines.extend(super::pane::question_lines(q, None, picked, now));
        if more > 0 {
            lines.push(Line::from(Span::styled(
                format!(" and {more} more waiting"),
                theme::dim(),
            )));
        }
    }
    if let Some(r) = rule {
        lines.push(Line::raw(""));
        if matches!(n.kind, NodeKind::Schedule | NodeKind::Reaction) {
            lines.extend(super::rules::lines(r, names, now, last_run));
        } else {
            lines.push(Line::from(vec![
                Span::styled(" from ", theme::dim()),
                Span::raw(format!("{} {}", r.kind(), r.name())),
            ]));
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
                    Span::raw(other.name.clone()),
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
    let rules = g
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Schedule | NodeKind::Reaction))
        .count();
    let waiting: usize = g.nodes.iter().map(|n| n.waiting.len()).sum();
    let mut s = format!("wiring · {rules} rules");
    if waiting > 0 {
        s.push_str(&format!(" · {waiting} waiting"));
    }
    if !filter.is_empty() {
        s.push_str(&format!(" · {kept} of {} · ⌕ {filter}", g.nodes.len()));
    }
    s
}

/// Where the window over the canvas should start so the selected box is in
/// it, moving as little as it can from where it was.
pub fn follow(p: &Plan, sel: Option<&str>, was: (usize, usize), w: usize, h: usize) -> (usize, usize) {
    let clamp = |at: usize, len: usize, view: usize| at.min(len.saturating_sub(view));
    let Some(b) = sel.and_then(|s| p.get(s)) else {
        return (clamp(was.0, p.width, w), clamp(was.1, p.height, h));
    };
    let axis = |start: usize, pos: usize, size: usize, view: usize, len: usize| {
        let mut s = start;
        if pos + size > s + view {
            s = (pos + size + 2).saturating_sub(view);
        }
        if pos < s {
            s = pos.saturating_sub(2);
        }
        clamp(s, len, view)
    };
    (axis(was.0, b.x, b.w, w, p.width), axis(was.1, b.y, b.h, h, p.height))
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
            rule: None,
            status: Some(RuleStatus::On),
            detail: String::new(),
            next_at: None,
            runs: Tally::default(),
            waiting: vec![],
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
                node("s", NodeKind::Schedule, "check-weather", Some("/w/relay")),
                node("s.run", NodeKind::Shell, "check-weather", Some("/w/relay")),
                node("r", NodeKind::Reaction, "advisory", Some("/w/relay")),
                node("a", NodeKind::Ask, "add a note?", Some("/w/relay")),
                node("a.note", NodeKind::Shell, "echo >> notes", Some("/w/relay")),
                node("shells", NodeKind::Shell, "shells", Some("/w/api")),
                node("t", NodeKind::Reaction, "tell-me", None),
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

    fn all(g: &Graph) -> HashSet<String> {
        keep(g, "", &names())
    }

    #[test]
    fn columns_come_from_what_feeds_a_node_not_its_kind() {
        let g = weather();
        let p = plan(&g, &all(&g), &names(), now());
        let rank = |id: &str| p.get(id).unwrap().rank;
        assert_eq!(
            (rank("s"), rank("s.run"), rank("r"), rank("a"), rank("a.note")),
            (0, 1, 2, 3, 4)
        );
        // A folder's shells feed a reaction directly: they start at the left
        // too, and the reaction they share with a schedule's command sits one
        // past the further of the two.
        assert_eq!((rank("shells"), rank("t")), (0, 2));
        // Nothing overlaps.
        for a in &p.boxes {
            for b in &p.boxes {
                if a.id < b.id {
                    let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                    assert!(apart, "{} overlaps {}", a.id, b.id);
                }
            }
        }
        assert_eq!(p.order()[..2], ["s", "shells"]);
    }

    #[test]
    fn a_chain_draws_as_boxes_joined_by_labelled_arrows() {
        let g = Graph {
            since: now(),
            nodes: vec![
                node("s", NodeKind::Schedule, "nightly", Some("/w/relay")),
                Node {
                    runs: Tally {
                        total: 2,
                        ..Default::default()
                    },
                    ..node("run", NodeKind::Shell, "make test", Some("/w/relay"))
                },
                node("r", NodeKind::Reaction, "tell-me", None),
            ],
            edges: vec![edge("s", "run", "", 2), edge("run", "r", "run.failed", 1)],
        };
        let p = plan(&g, &all(&g), &names(), now());
        let text = draw(&g, &p, &names(), None, now()).plain();
        assert!(text.contains("◷ nightly"), "{text}");
        assert!(text.contains("❯ make test"), "{text}");
        assert!(text.contains("↯ tell-me"), "{text}");
        assert!(text.contains("relay · ok ×2"), "{text}");
        assert!(text.contains("run.failed ×1"), "{text}");
        assert!(text.contains("▶"), "{text}");
        // Level with what feeds it, a chain is one straight line.
        let row = text.lines().find(|l| l.contains("◷ nightly")).unwrap();
        assert!(row.contains("❯ make test") && row.contains("↯ tell-me"), "{text}");
    }

    #[test]
    fn a_loop_goes_back_along_the_bottom() {
        let g = Graph {
            since: now(),
            nodes: vec![
                node("r", NodeKind::Reaction, "fixer", None),
                node("run", NodeKind::Agent, "claude · fix", None),
            ],
            edges: vec![edge("r", "run", "", 1), edge("run", "r", "run.failed", 1)],
        };
        let p = plan(&g, &all(&g), &names(), now());
        let back: Vec<&Route> = p.routes.iter().filter(|r| r.back.is_some()).collect();
        assert_eq!(back.len(), 1);
        let body = p.boxes.iter().map(|b| b.y + b.h).max().unwrap();
        assert!(back[0].back.unwrap().0 > body, "below every box");
        let text = draw(&g, &p, &names(), None, now()).plain();
        assert_eq!(text.matches('▶').count(), 2, "{text}");
    }

    #[test]
    fn a_filter_keeps_what_reaches_and_leaves_the_match() {
        let g = weather();
        let kept = keep(&g, "api", &names());
        let mut ids: Vec<&str> = kept.iter().map(String::as_str).collect();
        ids.sort();
        assert_eq!(ids, ["shells", "t"], "api's shells, and the global rule they reach");
        let kept = keep(&g, "advisory", &names());
        assert!(["s", "s.run", "r", "a", "a.note"].iter().all(|id| kept.contains(*id)));
        assert!(!kept.contains("shells"));
        // Tokens pick nodes too, and bring their wiring with them.
        let kept = keep(&g, "kind:question", &names());
        assert!(kept.contains("a") && kept.contains("r"), "{kept:?}");
        assert!(!kept.contains("shells"));
    }

    #[test]
    fn arrows_follow_the_wiring_and_the_columns() {
        let g = weather();
        let p = plan(&g, &all(&g), &names(), now());
        let go = |from: &str, dir| step(&p, Some(from), dir);
        // Along the wiring, both ways.
        assert_eq!(go("s", Dir::Right).as_deref(), Some("s.run"));
        assert_eq!(go("r", Dir::Right).as_deref(), Some("a"));
        assert_eq!(go("a", Dir::Left).as_deref(), Some("r"));
        // Two feed tell-me: left goes to the nearer in height.
        let near = go("t", Dir::Left).unwrap();
        let (t, n) = (p.get("t").unwrap(), p.get(&near).unwrap());
        for other in ["s.run", "shells"] {
            let o = p.get(other).unwrap();
            let d = |b: &Placed| (b.y as isize + b.h as isize / 2 - (t.y as isize + t.h as isize / 2)).abs();
            assert!(d(n) <= d(o), "{near} is not the nearest feeder");
        }
        // Nothing further: stay put.
        assert_eq!(go("a.note", Dir::Right), None);
        assert_eq!(go("s", Dir::Left), None);
        // Up and down keep to the column.
        assert_eq!(go("s", Dir::Down).as_deref(), Some("shells"));
        assert_eq!(go("shells", Dir::Up).as_deref(), Some("s"));
        // No selection yet: the first node.
        assert_eq!(step(&p, None, Dir::Down).as_deref(), Some("s"));
    }

    #[test]
    fn the_pane_says_what_a_node_is_and_what_it_is_wired_to() {
        let g = weather();
        let r = g.node("r").unwrap();
        let text: Vec<String> = pane_lines(&g, r, None, None, None, &[], &names(), now())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], " ↯ advisory");
        assert_eq!(text[1], " reaction · relay");
        assert!(
            text.iter().any(|l| l == "  ← ❯ check-weather  weather.alert ×3"),
            "{text:#?}"
        );
        assert!(text.iter().any(|l| l == "  → ◇ add a note?  ×3"), "{text:#?}");
    }

    #[test]
    fn the_window_follows_the_selection() {
        let g = weather();
        let p = plan(&g, &all(&g), &names(), now());
        let far = p.get("a.note").unwrap();
        let (x, _) = follow(&p, Some("a.note"), (0, 0), 40, 10);
        assert!(x <= far.x && far.x + far.w <= x + 40);
        assert_eq!(follow(&p, Some("s"), (x, 0), 40, 10).0, 0);
    }
}
