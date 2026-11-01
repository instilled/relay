//! `impl App` — a project's overview: `enter` on its line on the list. What was said of the
//! project on top, its flows below as rows (those that need you first, then by name, each with
//! its open questions under it), and a last line, `+ new flow`. `esc` goes back to its line.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

/// What an overview is of: a project's folder, or `everywhere`, the flows with no `in`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Scope {
    Project(PathBuf),
    Everywhere,
}

impl Scope {
    /// The project's folder; `everywhere` has none.
    pub(super) fn folder(&self) -> Option<&Path> {
        match self {
            Scope::Project(p) => Some(p),
            Scope::Everywhere => None,
        }
    }

    /// Whether `h` is this scope's line on the list.
    fn is(&self, h: &rows::Head) -> bool {
        match self {
            Scope::Project(p) => !h.is_everywhere() && h.workspace == *p,
            Scope::Everywhere => h.is_everywhere(),
        }
    }

    /// What `/summary` of it is of, as its line's would be.
    fn target(&self) -> SummaryTarget {
        match self {
            Scope::Project(p) => SummaryTarget::Project { workspace: p.clone() },
            Scope::Everywhere => SummaryTarget::All,
        }
    }
}

impl App {
    /// Puts `scope`'s overview up, the list's cursor kept for when it comes back. The cursor
    /// lands on `at` when it is one of its rows, else on its first flow, else on `+ new flow`.
    pub(super) fn open_overview(&mut self, scope: Scope, at: Option<String>) {
        self.overview = Some(scope);
        self.open_view(View::Overview);
        self.leave_subpage();
        let keys = self.keys();
        let at = at
            .filter(|k| keys.contains(k))
            .or_else(|| keys.first().cloned())
            .unwrap_or_else(|| NEW_FLOW.to_string());
        self.select(Some(at));
    }

    /// The overview's rows: its project's flows, each followed by its open questions. Those
    /// that need you first, then by name. Hidden ones are there too; the search narrows them as
    /// it does the list.
    pub(super) fn overview_rows(&self) -> Vec<Row<'_>> {
        let Some(scope) = &self.overview else {
            return vec![];
        };
        let all = rows::build(
            &self.digest,
            &rows::Context {
                names: &self.names,
                flows: &self.flows,
                filter: &self.filter,
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: true,
                now: Timestamp::now(),
                relay: Some(&self.relay),
            },
        );
        // Each flow with the questions right under it, as `(needs you, name, rows)`.
        let mut blocks: Vec<(bool, String, Vec<Row<'_>>)> = Vec::new();
        let mut inside = false;
        let mut under_flow = false;
        for r in all {
            match r {
                Row::Group(h) => {
                    inside = scope.is(&h);
                    under_flow = false;
                }
                Row::Flow(f) if inside => {
                    let needs = self.flow_of_row(&f).is_some_and(|x| rows::flow_needs(x, &self.digest));
                    blocks.push((needs, f.name.clone(), vec![Row::Flow(f)]));
                    under_flow = true;
                }
                Row::Question(q) if inside && under_flow => {
                    if let Some((.., b)) = blocks.last_mut() {
                        b.push(Row::Question(q));
                    }
                }
                _ => under_flow = false,
            }
        }
        blocks.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
        blocks.into_iter().flat_map(|(.., b)| b).collect()
    }

    /// Whether the cursor is on the overview's `+ new flow`.
    pub(super) fn on_new_flow(&self) -> bool {
        self.view == View::Overview && self.sel.as_deref() == Some(NEW_FLOW)
    }

    /// The overview's headline: the project's name, and how its flows stand.
    pub(super) fn overview_headline(&self) -> String {
        let Some(scope) = &self.overview else {
            return String::new();
        };
        let name = match scope.folder() {
            Some(p) => self.names.workspace(p),
            None => rows::EVERYWHERE.to_string(),
        };
        let tally = rows::tally(&self.flows, scope.folder(), &self.digest).words();
        if tally.is_empty() {
            name
        } else {
            format!("{name} · {tally}")
        }
    }

    /// The overview's top: what was last said of the project, else that it is being said, else
    /// how to have it said.
    pub(super) fn overview_top_lines(&self, width: usize) -> Vec<Line<'static>> {
        let Some(scope) = &self.overview else {
            return vec![];
        };
        let target = scope.target();
        let key = target.key();
        let now = Timestamp::now();
        // The one loaded with the cursor's row, else the last one made here.
        let kept = self
            .summary
            .as_ref()
            .filter(|s| s.target == key)
            .or_else(|| self.made.get(&key));
        if let Some(s) = kept {
            // Its own blank line first is the overview's to place.
            return pane::summary_section(Some(s), width, now).into_iter().skip(1).collect();
        }
        if let Some(s) = self.summarising.as_ref().filter(|s| s.target == target) {
            let secs = s.since.elapsed().as_secs();
            return vec![Line::styled(format!(" summarising… {secs}s"), theme::accent())];
        }
        vec![Line::styled(" no summary yet · /summary writes it", theme::dim())]
    }

    /// The overview: its top, a blank line, its rows as the list draws them, and `+ new flow`,
    /// a window of them following the selection.
    pub(super) fn draw_overview(&self, f: &mut Frame, area: Rect) {
        self.list_h.set(area.height);
        *self.hit.borrow_mut() = ListHit::default();
        let w = area.width as usize;
        let mut lines = self.overview_top_lines(w);
        lines.push(Line::raw(""));
        let mut keys: Vec<Option<String>> = vec![None; lines.len()];
        let rows = self.overview_rows();
        let (row_lines, row_keys, row_sel) = self.row_lines(&rows, w);
        let mut sel_line = lines.len() + row_sel;
        lines.extend(row_lines);
        keys.extend(row_keys);
        let mut new = Line::from(" + new flow");
        if self.on_new_flow() {
            let short = w.saturating_sub(new.width());
            if short > 0 {
                new.spans.push(Span::raw(" ".repeat(short)));
            }
            new = new.style(theme::selected().patch(theme::accent()));
            sel_line = lines.len();
        } else {
            new = new.style(theme::dim());
        }
        lines.push(new);
        keys.push(Some(NEW_FLOW.to_string()));
        self.draw_window(f, area, lines, keys, sel_line);
    }
}
