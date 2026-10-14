//! A flow's own wiring, drawn for its pane: each step a box with its last firing's mark, laid
//! out by rank — a step with no `after` first, one with `after` one past the furthest of those
//! — its `if` over it, and a line into it from each step it comes after. Pure.

use super::canvas::{Canvas, DOWN, LEFT, RIGHT, UP};
use super::flows::glyph;
use super::theme;
use ratatui::style::Style;
use rl_flow::def::FlowDef;
use rl_proto::flow::{Firing, StepStatus};
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[cfg(test)]
#[path = "flow_graph_tests.rs"]
mod tests;

/// The most of a step's name a box shows, and of an `if` the room over it is widened for.
const NAME_MAX: usize = 20;
const LABEL_MAX: usize = 24;

/// A box's borders, its mark, a space each side of the mark and one after the name.
const BOX_ROOM: usize = 6;

/// `s`, cut to `room` columns with `…` when wider. Columns, not chars: a wide glyph takes two.
fn clip(s: &str, room: usize) -> String {
    if s.width() <= room {
        return s.to_string();
    }
    let mut t = String::new();
    let mut used = 0;
    for c in s.chars() {
        used += c.width().unwrap_or(0);
        if used + 1 > room {
            break;
        }
        t.push(c);
    }
    t.push('…');
    t
}

/// A step's place: its column's left edge and its box's top row.
#[derive(Clone, Copy)]
struct Placed {
    rank: usize,
    x: usize,
    y: usize,
}

impl Placed {
    /// The row a line leaves or enters it by.
    fn mid(&self) -> usize {
        self.y + 1
    }
}

/// Some steps of one rank-to-rank gap and the steps they come after, all joined on one lane.
struct Bundle {
    from: Vec<usize>,
    to: Vec<usize>,
}

/// Each step's rank: 0 with no `after`, else one past the furthest step it comes after. An
/// `after` that names no step adds nothing.
fn ranks(def: &FlowDef) -> Vec<usize> {
    let index: HashMap<&str, usize> = def
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut rank = vec![0; def.steps.len()];
    // A checked flow has no cycle; the passes are bounded all the same.
    for _ in 0..def.steps.len() {
        let mut moved = false;
        for (i, s) in def.steps.iter().enumerate() {
            let r = s
                .after
                .iter()
                .filter_map(|a| index.get(a.as_str()))
                .map(|&a| rank[a] + 1)
                .max()
                .unwrap_or(0);
            if r != rank[i] {
                rank[i] = r;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    rank
}

/// The edges into one rank's steps as lanes: steps that all go to all of the same steps share
/// one, otherwise each step taken in gets its own.
fn bundles(edges: &[(usize, usize)]) -> Vec<Bundle> {
    let mut groups: Vec<(BTreeSet<usize>, BTreeSet<usize>, usize)> = Vec::new();
    for &(s, t) in edges {
        let touching: Vec<usize> = (0..groups.len())
            .filter(|&g| groups[g].0.contains(&s) || groups[g].1.contains(&t))
            .collect();
        let mut merged = (BTreeSet::from([s]), BTreeSet::from([t]), 1);
        for g in touching.into_iter().rev() {
            let (from, to, n) = groups.remove(g);
            merged.0.extend(from);
            merged.1.extend(to);
            merged.2 += n;
        }
        groups.push(merged);
    }
    let mut out = Vec::new();
    for (from, to, n) in groups {
        if n == from.len() * to.len() {
            out.push(Bundle {
                from: from.into_iter().collect(),
                to: to.into_iter().collect(),
            });
        } else {
            for t in to {
                out.push(Bundle {
                    from: edges.iter().filter(|e| e.1 == t).map(|e| e.0).collect(),
                    to: vec![t],
                });
            }
        }
    }
    out
}

/// The sides of the cells a line from `(x0, y0)` to `(x1, y1)`, straight across or down,
/// leaves by.
fn line(marks: &mut HashMap<(usize, usize), u8>, (x0, y0): (usize, usize), (x1, y1): (usize, usize)) {
    if y0 == y1 {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            *marks.entry((x, y0)).or_default() |= (u8::from(x > a) * LEFT) | (u8::from(x < b) * RIGHT);
        }
    } else {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            *marks.entry((x0, y)).or_default() |= (u8::from(y > a) * UP) | (u8::from(y < b) * DOWN);
        }
    }
}

/// A flow's steps drawn: boxes in their ranks, left to right, those of one rank top to bottom
/// as written, each with its mark from `firing` (`○` without one) and its `if` over it; the
/// step at `at` among the firing's steps lit. `None` when its lines would meet in a way that
/// reads as joined where they are not — the pane then lists the steps instead.
pub fn draw(def: &FlowDef, firing: Option<&Firing>, at: Option<usize>) -> Option<Canvas> {
    let steps = &def.steps;
    if steps.is_empty() {
        return None;
    }
    let rank = ranks(def);
    let columns = rank.iter().max().map_or(0, |r| r + 1);
    let names: Vec<String> = steps.iter().map(|s| clip(&s.name, NAME_MAX)).collect();
    let labels: Vec<Option<String>> = steps
        .iter()
        .map(|s| s.if_.as_ref().map(|c| format!("if {c}")))
        .collect();
    let mut col_w = vec![0; columns];
    for i in 0..steps.len() {
        let label = labels[i].as_ref().map_or(0, |l| l.width().min(LABEL_MAX));
        col_w[rank[i]] = col_w[rank[i]].max(names[i].width() + BOX_ROOM).max(label);
    }

    // Rows: each column's steps stacked as written, the n-th of every column level with the
    // others so that a chain runs straight, and a row over them where one has an `if`.
    let mut slot = vec![0; steps.len()];
    let mut filled = vec![0; columns];
    for (i, &r) in rank.iter().enumerate() {
        slot[i] = filled[r];
        filled[r] += 1;
    }
    let slots = filled.iter().max().copied().unwrap_or(0);
    let mut top = vec![0; slots];
    let mut y = 0;
    for (k, t) in top.iter_mut().enumerate() {
        y += usize::from((0..steps.len()).any(|i| slot[i] == k && labels[i].is_some()));
        *t = y;
        y += 4;
    }
    let height = y - 1;
    let mut placed: Vec<Placed> = (0..steps.len())
        .map(|i| Placed {
            rank: rank[i],
            x: 0,
            y: top[slot[i]],
        })
        .collect();

    // The lines into each rank, from the steps its steps come after.
    let index: HashMap<&str, usize> = steps.iter().enumerate().map(|(i, s)| (s.name.as_str(), i)).collect();
    let mut into: Vec<Vec<(usize, usize)>> = vec![Vec::new(); columns];
    for (t, s) in steps.iter().enumerate() {
        let mut seen = HashSet::new();
        for a in s.after.iter().filter_map(|a| index.get(a.as_str())) {
            if seen.insert(*a) {
                into[rank[t]].push((*a, t));
            }
        }
    }
    let mut lanes: Vec<Vec<Bundle>> = into.iter().map(|e| bundles(e)).collect();
    for b in &mut lanes {
        let highest = |b: &Bundle| b.from.iter().chain(&b.to).map(|&i| placed[i].mid()).min();
        b.sort_by_key(|b| highest(b));
    }

    // Columns: one space in, then each column, a gap of its lanes and an arrow before the next.
    let mut col_x = vec![0; columns];
    let mut gap_x = vec![0; columns];
    let mut x = 1;
    for r in 0..columns {
        if r > 0 {
            gap_x[r] = x;
            x += lanes[r].len().max(1) + 3;
        }
        col_x[r] = x;
        x += col_w[r];
    }
    let width = x;
    for p in &mut placed {
        p.x = col_x[p.rank];
    }

    // What the boxes and labels take, which no line may cross.
    let mut taken: HashSet<(usize, usize)> = HashSet::new();
    for (i, p) in placed.iter().enumerate() {
        for xx in p.x..p.x + col_w[p.rank] {
            for yy in p.y..p.y + 3 {
                taken.insert((xx, yy));
            }
        }
        if let Some(l) = &labels[i] {
            for xx in p.x..p.x + clip(l, col_w[p.rank]).width() {
                taken.insert((xx, p.y - 1));
            }
        }
    }

    // Each lane's lines on its own, so that where two lanes meet can be told.
    let mut drawn: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    let mut arrows = Vec::new();
    for (r, bundles) in lanes.iter().enumerate() {
        for (n, b) in bundles.iter().enumerate() {
            let lane = gap_x[r] + 1 + n;
            let mut marks = HashMap::new();
            let ys = b.from.iter().chain(&b.to).map(|&i| placed[i].mid());
            let (top, bottom) = (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0));
            for &s in &b.from {
                let p = placed[s];
                line(&mut marks, (p.x + col_w[p.rank], p.mid()), (lane, p.mid()));
            }
            line(&mut marks, (lane, top), (lane, bottom));
            for &t in &b.to {
                let p = placed[t];
                line(&mut marks, (lane, p.mid()), (p.x - 1, p.mid()));
                arrows.push((p.x - 1, p.mid()));
            }
            for (cell, m) in marks {
                drawn.entry(cell).or_default().push(m);
            }
        }
    }
    let crossing = |ms: &[u8]| {
        let mut ms = ms.to_vec();
        ms.sort_unstable();
        ms == [UP | DOWN, LEFT | RIGHT]
    };
    if drawn
        .iter()
        .any(|(cell, ms)| taken.contains(cell) || (ms.len() > 1 && !crossing(ms)))
    {
        return None;
    }

    let mut c = Canvas::new(width, height);
    for ((x, y), ms) in &drawn {
        c.mark(*x, *y, ms.iter().fold(0, |a, m| a | m), theme::dim());
    }
    c.join();
    for (x, y) in arrows {
        c.put(x, y, '▶', theme::dim());
    }
    let status = |name: &str| {
        firing
            .and_then(|f| f.steps.iter().find(|s| s.name == name))
            .map_or(StepStatus::Pending, |s| s.status)
    };
    let lit = at.and_then(|n| firing?.steps.get(n)).map(|s| s.name.as_str());
    for (i, p) in placed.iter().enumerate() {
        let w = col_w[p.rank];
        if let Some(l) = &labels[i] {
            c.text(p.x, p.y - 1, &clip(l, w), theme::dim());
        }
        let border = if lit == Some(steps[i].name.as_str()) {
            theme::selected()
        } else {
            Style::default()
        };
        let (x, y) = (p.x, p.y);
        c.put(x, y, '╭', border);
        c.put(x + w - 1, y, '╮', border);
        c.put(x, y + 2, '╰', border);
        c.put(x + w - 1, y + 2, '╯', border);
        for xx in x + 1..x + w - 1 {
            c.put(xx, y, '─', border);
            c.put(xx, y + 2, '─', border);
        }
        c.put(x, y + 1, '│', border);
        c.put(x + w - 1, y + 1, '│', border);
        let s = status(&steps[i].name);
        let mark = if s == StepStatus::Failed {
            theme::bad()
        } else {
            Style::default()
        };
        c.text(x + 2, y + 1, glyph(s), mark);
        c.text(x + 4, y + 1, &names[i], Style::default());
    }
    Some(c)
}
