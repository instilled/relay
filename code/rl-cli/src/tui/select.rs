//! `impl App` — the selection & navigation cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    pub(super) fn rows(&self) -> Vec<Row<'_>> {
        rows::build(
            &self.digest,
            &rows::Context {
                names: &self.names,
                flows: &self.flows,
                filter: &self.filter,
                show_flows: false,
                show_agents: self.show_agents,
                show_shells: self.show_shells,
                show_hidden: self.show_hidden,
                now: Timestamp::now(),
                relay: Some(&self.relay),
            },
        )
    }

    /// The folder the selected row's work is in. The `everywhere` header has
    /// none: what is under it ran in folders of all sorts. In an overview with
    /// no row under the cursor, the project's.
    pub(super) fn selected_folder(&self) -> Option<PathBuf> {
        let Some(row) = self.selected() else {
            // In an overview with nothing under the cursor (`+ new flow`): its project's.
            return match self.view {
                View::Overview => self.overview.as_ref().and_then(|s| s.folder()).map(Path::to_path_buf),
                _ => None,
            };
        };
        match row {
            Row::Run(i) => Some(i.run.workspace.clone()),
            Row::Flow(f) => self
                .flow_of_row(&f)
                .and_then(|x| x.folder.clone())
                .or_else(|| f.last().map(|i| i.run.workspace.clone())),
            Row::Question(q) => Some(q.workspace.clone()),
            Row::Outside(c) => Some(c.cwd.clone()),
            Row::Group(h) if h.is_everywhere() => None,
            Row::Group(h) => Some(h.workspace.clone()),
        }
    }

    /// The registered folder under the cursor, when the cursor is on a project
    /// header — what `^r` acts on.
    pub(super) fn selected_project(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Group(h) if !h.is_everywhere() => Some(h.workspace.clone()),
            _ => None,
        }
    }

    /// Whether the cursor is on the `everywhere` header, which is not a
    /// project: what acts on a project's folder says so there, and does
    /// nothing.
    pub(super) fn on_everywhere(&self) -> bool {
        matches!(self.selected(), Some(Row::Group(h)) if h.is_everywhere())
    }

    /// Keys of the selectable rows of the view that is up, in order.
    pub(super) fn keys(&self) -> Vec<String> {
        match self.view {
            View::Work => self.rows().into_iter().filter_map(|r| r.key()).collect(),
            View::Events => self.event_items().iter().flat_map(|i| i.keys()).collect(),
            View::Report => Vec::new(),
            View::Overview => self
                .overview_rows()
                .into_iter()
                .filter_map(|r| r.key())
                .chain(std::iter::once(NEW_FLOW.to_string()))
                .collect(),
            View::CatchUp => self
                .lead_keys()
                .into_iter()
                .chain(std::iter::once(TOP.to_string()))
                .chain(self.catch_rows().into_iter().filter_map(|r| r.key()))
                .collect(),
        }
    }

    /// Keeps the selection on something that exists: the first row, when
    /// it has to move — in the catch-up, the first under its top and what heads it.
    pub(super) fn settle(&mut self) {
        let keys = self.keys();
        if self.sel.as_ref().is_none_or(|k| !keys.contains(k)) {
            let first = keys
                .iter()
                .find(|k| *k != TOP && heads::flow_of(k).is_none())
                .or(keys.first());
            self.select(first.cloned());
        }
    }

    /// Whether the cursor is on the catch-up's top.
    pub(super) fn on_top(&self) -> bool {
        self.view == View::CatchUp && self.sel.as_deref() == Some(TOP)
    }

    /// Keys of what the search's words name, best first; see `query::ranked`.
    /// Nothing when it has no words, only tokens.
    pub(super) fn matches(&self) -> Vec<String> {
        let words = query::Query::parse(&self.filter).words;
        if words.is_empty() {
            return vec![];
        }
        let (keys, named): (Vec<Option<String>>, Vec<query::Named>) = match self.view {
            View::Report => return vec![],
            View::Work => self
                .rows()
                .iter()
                .map(|r| (r.key(), rows::named(r, &self.digest, &self.names, &self.flows)))
                .unzip(),
            View::Events => self
                .event_items()
                .iter()
                .map(|i| (Some(i.key()), events::named(i)))
                .unzip(),
            View::CatchUp => self
                .catch_rows()
                .iter()
                .map(|r| (r.key(), rows::named(r, &self.digest, &self.names, &self.flows)))
                .unzip(),
            View::Overview => self
                .overview_rows()
                .iter()
                .map(|r| (r.key(), rows::named(r, &self.digest, &self.names, &self.flows)))
                .unzip(),
        };
        query::ranked(&words, &named)
            .into_iter()
            .filter_map(|i| keys[i].clone())
            .collect()
    }

    /// The search changed: the cursor goes to the best match. With no words
    /// to match, it stays where it is if it can.
    pub(super) fn land(&mut self) {
        match self.matches().into_iter().next() {
            Some(k) => self.select(Some(k)),
            None => self.settle(),
        }
    }

    /// `tab` with nothing to complete: on to the next match, round; shift-tab
    /// back. False when there is none to go to.
    pub(super) fn next_match(&mut self, back: bool) -> bool {
        let order = self.matches();
        let Some(k) = query::next_match(&order, self.sel.as_ref(), back).cloned() else {
            return false;
        };
        self.select(Some(k));
        true
    }

    pub(super) fn select(&mut self, key: Option<String>) {
        if key != self.sel {
            // A name being changed belongs to its row: off the row, it goes
            // unsaved.
            if matches!(self.mode, Mode::Rename(_)) {
                self.mode = Mode::Normal;
            }
            self.sel = key;
            self.picked.clear();
            // A firing picked in a flow's walk lasts while the cursor is on its row: off it, the
            // flow's latest again.
            self.flow_pane.picked = None;
            self.answer_at = 0;
            self.events.clear();
            // A watch tile stays pinned to its run while you browse the list;
            // it closes on `esc` or when another run's terminal takes its place,
            // not merely because the cursor moved.
        }
    }

    /// The row under the cursor, on the list or in the catch-up.
    pub(super) fn selected(&self) -> Option<Row<'_>> {
        let rows = match self.view {
            View::Work => self.rows(),
            View::CatchUp => self.catch_rows(),
            View::Overview => self.overview_rows(),
            View::Events | View::Report => return None,
        };
        let key = self.sel.as_ref()?;
        rows.into_iter().find(|r| r.key().as_ref() == Some(key))
    }

    pub(super) fn selected_run(&self) -> Option<&DigestItem> {
        self.selected().and_then(|r| r.item())
    }

    pub(super) fn selected_flow(&self) -> Option<rows::FlowRow<'_>> {
        match self.selected() {
            Some(Row::Flow(f)) => Some(f),
            _ => None,
        }
    }

    pub(super) fn selected_question(&self) -> Option<&Decision> {
        match self.selected() {
            Some(Row::Question(d)) => Some(d),
            _ => None,
        }
    }

    /// The question the line answers: the one under the cursor on the list
    /// or in the catch-up.
    pub(super) fn question_here(&self) -> Option<&Decision> {
        match self.view {
            View::Work | View::CatchUp | View::Overview => self.selected_question(),
            View::Events | View::Report => None,
        }
    }

    /// The flow behind a flow's row, as last loaded.
    pub(super) fn flow_of_row(&self, f: &rows::FlowRow) -> Option<&Flow> {
        self.flows.iter().find(|x| x.name == f.name)
    }

    /// The flow under the cursor: a flow's row, on the list or in the
    /// catch-up.
    pub(super) fn flow_here(&self) -> Option<String> {
        self.selected_flow().map(|f| f.name.to_string())
    }

    pub(super) fn run_by_id(&self, id: &str) -> Option<&Run> {
        self.digest.items.iter().map(|i| &i.run).find(|r| r.id == id)
    }

    /// The cursor is on it: that is looking. A flow's runs count as looked at
    /// when its row is.
    /// The cursor moved: what the new row needs, and nothing more. The list
    /// itself is not fetched again — the tick keeps it current — so holding
    /// a key moves at the speed of drawing, not of a whole digest per step.
    pub(super) async fn moved(&mut self) {
        // The catch-up is looked at as a whole, when you leave it.
        if self.view != View::CatchUp {
            self.saw_selected().await;
        }
        self.load_pane().await;
    }

    pub(super) async fn saw_selected(&mut self) {
        let mut from = None;
        let ids: Vec<String> = match self.selected() {
            Some(Row::Run(i)) if i.unseen => vec![i.run.id.clone()],
            Some(Row::Flow(f)) => {
                let unseen: Vec<_> = f.runs.iter().filter(|i| i.unseen).collect();
                from = unseen
                    .iter()
                    .map(|i| i.run.started_at)
                    .min()
                    .map(|at| (f.key.clone(), at));
                unseen.iter().map(|i| i.run.id.clone()).collect()
            }
            _ => vec![],
        };
        if let Some((key, at)) = from {
            self.unseen_from.insert(key, at);
        }
        self.seen(ids).await;
    }

    pub(super) fn step(&mut self, by: isize) {
        let keys = self.keys();
        if keys.is_empty() {
            return;
        }
        let cur = self
            .sel
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
            .unwrap_or(0) as isize;
        let next = (cur + by).clamp(0, keys.len() as isize - 1) as usize;
        self.select(Some(keys[next].clone()));
    }

    /// Whether the list still has a row once the catch-up is left and what
    /// it showed is seen: the run `run` on a row of its own,
    /// else the row `key`, whatever the toggles. A run that ended over a day
    /// ago, unpinned, is the list's only while it is not seen.
    pub(super) fn kept_on_the_list(&self, key: &str, run: Option<&str>) -> bool {
        let mut d = self.digest.clone();
        let seen = self.catch_up_unseen().ids;
        for i in &mut d.items {
            if seen.contains(&i.run.id) {
                i.unseen = false;
            }
        }
        // Everything shown at once: whatever a toggle could reveal is on.
        [(true, true, false), (true, true, true)]
            .into_iter()
            .any(|(show_agents, show_shells, show_hidden)| {
                let cx = rows::Context {
                    names: &self.names,
                    flows: &self.flows,
                    filter: "",
                    show_flows: false,
                    show_agents,
                    show_shells,
                    show_hidden,
                    now: Timestamp::now(),
                    relay: Some(&self.relay),
                };
                rows::build(&d, &cx).iter().any(|r| match (run, r) {
                    (Some(id), Row::Run(i)) => i.run.id == id,
                    (Some(_), _) => false,
                    (None, r) => r.key().as_deref() == Some(key),
                })
            })
    }

    /// The list's row for a run, as the list would stand with this search
    /// and these toggles: its own.
    pub(super) fn list_key(&self, id: &str, filter: &str, toggles: (bool, bool, bool)) -> Option<String> {
        self.row_key(filter, toggles, |r| matches!(r, Row::Run(i) if i.run.id == id))
    }

    /// The key of the first row of the list that `is` picks, as the list would
    /// stand with this search and these toggles `(agents, shells, hidden)`.
    pub(super) fn row_key(
        &self,
        filter: &str,
        (show_agents, show_shells, show_hidden): (bool, bool, bool),
        is: impl Fn(&Row) -> bool,
    ) -> Option<String> {
        let cx = rows::Context {
            names: &self.names,
            flows: &self.flows,
            filter,
            show_flows: false,
            show_agents,
            show_shells,
            show_hidden,
            now: Timestamp::now(),
            relay: Some(&self.relay),
        };
        rows::build(&self.digest, &cx)
            .iter()
            .find(|r| is(r))
            .and_then(|r| r.key())
    }
}
