//! `impl App` — a project's overview: `enter` on its line on the list. What was said of the
//! project on top, its flows below as rows (those that need you first, then by name, each with
//! its open questions under it), and a line, `+ new flow`; last, when any of its flows are parked,
//! `parked: N`, which `enter` folds them out under. `esc` goes back to its line.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::flow_proposal::Aim;
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

    /// The scope a flow is in: its project, or `everywhere` with no `in`.
    pub(super) fn of(flow: &Flow) -> Self {
        match &flow.folder {
            Some(p) => Scope::Project(p.clone()),
            None => Scope::Everywhere,
        }
    }

    /// The key of its line on the list.
    pub(super) fn line_key(&self) -> String {
        match self {
            Scope::Project(p) => format!("g:{}", p.display()),
            Scope::Everywhere => format!("g:{}", rows::EVERYWHERE),
        }
    }

    /// Whether a parked flow is this scope's: its `in` the project, or none for `everywhere`.
    fn holds(&self, p: &Parked) -> bool {
        p.in_.as_deref() == self.folder()
    }

    /// Whether `h` is this scope's line on the list.
    fn is(&self, h: &rows::Head) -> bool {
        match self {
            Scope::Project(p) => !h.is_everywhere() && h.workspace == *p,
            Scope::Everywhere => h.is_everywhere(),
        }
    }

    /// What `/summary` of it is of, as its line's would be.
    pub(super) fn target(&self) -> SummaryTarget {
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
        if self.overview.as_ref() != Some(&scope) {
            self.overview_said = None;
            self.parked_open = false;
        }
        if self.view != View::Overview {
            self.list_search = Some(std::mem::take(&mut self.filter));
            self.search = None;
        }
        self.overview = Some(scope);
        self.open_view(View::Overview);
        self.leave_subpage();
        let keys = self.keys();
        let at = at
            .filter(|k| keys.contains(k))
            .or_else(|| keys.first().cloned())
            .unwrap_or_else(|| NEW_FLOW.to_string());
        self.select(Some(at));
        let _ = self.note_unseen_from();
    }

    /// What was last said of the overview's project, fetched for its top whichever row the
    /// cursor is on, and the flows parked, for its last line; and a new one asked for when it is
    /// to be written (`write_overview_top`). `everywhere`'s top has none.
    pub(super) async fn fetch_overview_top(&mut self) {
        self.fetch_parked().await;
        self.write_overview_top().await;
    }

    /// The overview's top as `plan_project_top` says: the one shown, and with none kept that is
    /// current, a new one asked for on a task of its own. One at a time: one already being
    /// written of the project, by this or by `/summary`, is left to land; while another
    /// project's is written, this one is next, asked once that one lands. One that failed here
    /// is not asked again until a `/summary` of the project.
    pub(super) async fn write_overview_top(&mut self) {
        self.overview_next = None;
        let Some(folder) = self.overview.as_ref().and_then(Scope::folder).map(Path::to_path_buf) else {
            return;
        };
        let target = SummaryTarget::Project {
            workspace: folder.clone(),
        };
        let from = self.overview_stretch(&target);
        let plan = crate::catchup::plan_project_top(&mut self.rpc, &folder, from).await;
        if let Some(s) = plan.shown {
            self.overview_said = Some(s);
        }
        let Some(params) = plan.ask else { return };
        let writing = |s: &Option<Summarising>| s.as_ref().is_some_and(|s| s.target == target);
        if self.overview_failed.contains(&target.key()) || writing(&self.overviewing) || writing(&self.summarising) {
            return;
        }
        if self.overviewing.is_some() {
            self.overview_next = Some(target);
            return;
        }
        let home = self.home.clone();
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<SummaryMake>(params).await
        });
        self.overviewing = Some(Summarising {
            title: self.names.workspace(&folder),
            target,
            deep: false,
            from: Some(from),
            since: Instant::now(),
            task,
        });
    }

    /// Where the overview's summary of `target` starts: where the last one made of it here did,
    /// so the one it made is found again; else since you were last here; else the last day.
    fn overview_stretch(&self, target: &SummaryTarget) -> Timestamp {
        self.overview_from
            .get(&target.key())
            .copied()
            .or(self.away_since)
            .unwrap_or_else(|| Timestamp::now() - crate::catchup::OVERVIEW_STRETCH)
    }

    /// The overview's summary landed: kept as the last made of its project, its top while the
    /// overview is still on it, and the subpage's unless that shows a deep one or another
    /// stretch. Failing, a word says why, and it is not asked again by itself. Then the project
    /// the overview is on, when it was next, is asked for.
    pub(super) async fn overviewed(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
        let Some(o) = self.overviewing.take() else { return };
        match done {
            Ok(Ok(summary)) => {
                let key = o.target.key();
                if let Some(f) = summary.from {
                    self.overview_from.insert(key.clone(), f);
                }
                // A deep one made of the project by `/summary` stays its `made`: the brief shows
                // in the top, not over it.
                if !self.made.get(&key).is_some_and(|m| m.deep) {
                    self.made.insert(key.clone(), summary.clone());
                }
                if self.overview.as_ref().map(Scope::target).as_ref() == Some(&o.target) {
                    self.overview_said = Some(summary.clone());
                }
                let other = self
                    .summary
                    .as_ref()
                    .is_some_and(|s| s.target == key && (s.deep || s.from != summary.from));
                if self.summary_target().as_ref() == Some(&o.target) && !other {
                    self.summary = Some(summary);
                }
            }
            Ok(Err(e)) => {
                self.overview_failed.insert(o.target.key());
                self.say(format!("the summary: {e:#}"));
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => {
                self.overview_failed.insert(o.target.key());
                self.say(format!("the summary: {e}"));
            }
        }
        let next = self.overview_next.take();
        if next.is_some() && next == self.overview.as_ref().map(Scope::target) {
            self.write_overview_top().await;
        }
    }

    /// The parked flows, every scope's, as the daemon finds them in `~/.relay/flows`. Ones that
    /// can't be had leave the last ones standing.
    pub(super) async fn fetch_parked(&mut self) {
        if let Ok(p) = self.rpc.call::<FlowParkedList>(()).await {
            self.parked = p;
        }
        if self.parked_in_scope().is_empty() {
            self.parked_open = false;
        }
    }

    /// The overview's parked flows: those whose `in` is its project, or, `everywhere`'s, none.
    pub(super) fn parked_in_scope(&self) -> Vec<&Parked> {
        let Some(scope) = &self.overview else {
            return vec![];
        };
        self.parked.iter().filter(|p| scope.holds(p)).collect()
    }

    /// The keys after `+ new flow`: `parked: N` when there are any, and its flows under it
    /// while it is open.
    pub(super) fn parked_keys(&self) -> Vec<String> {
        let parked = self.parked_in_scope();
        if parked.is_empty() {
            return vec![];
        }
        let mut keys = vec![PARKED.to_string()];
        if self.parked_open {
            keys.extend(parked.iter().map(|p| parked_key(&p.folder)));
        }
        keys
    }

    /// Whether the cursor is on the overview's `parked: N`.
    pub(super) fn on_parked_line(&self) -> bool {
        self.view == View::Overview && self.sel.as_deref() == Some(PARKED)
    }

    /// The folder of the parked flow under the cursor, in the overview.
    pub(super) fn parked_here(&self) -> Option<String> {
        if self.view != View::Overview {
            return None;
        }
        let folder = self.sel.as_deref()?.strip_prefix(PARKED_ROW)?;
        Some(folder.to_string())
    }

    /// Back from the overview to the list, on the line it was opened from.
    pub(super) async fn leave_overview(&mut self) {
        self.overview_closed();
        self.go_back();
        self.settle();
        self.load_pane().await;
    }

    /// The overview is let go of: the list's search, set aside while it was up, given back.
    pub(super) fn overview_closed(&mut self) {
        self.overview = None;
        if let Some(f) = self.list_search.take() {
            self.filter = f;
        }
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
                parked: &self.parked,
            },
        );
        // Each flow with the questions right under it, as `(needs you, name, rows)`.
        let mut blocks: Vec<(bool, String, Vec<Row<'_>>)> = Vec::new();
        // A live run's question is filed under the run, which is not a row here: it goes under
        // the run's flow, once every flow has its block.
        let mut stray: Vec<(&str, Row<'_>)> = Vec::new();
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
                Row::Question(q) => {
                    under_flow = false;
                    let asker = q.run.as_deref().and_then(|id| self.run_by_id(id));
                    if let Some(Cause::Flow { flow, .. }) = asker.map(|r| &r.cause) {
                        stray.push((flow.as_str(), Row::Question(q)));
                    }
                }
                _ => under_flow = false,
            }
        }
        for (flow, q) in stray {
            if let Some((.., b)) = blocks.iter_mut().find(|(_, name, _)| name == flow) {
                b.push(q);
            }
        }
        blocks.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
        blocks.into_iter().flat_map(|(.., b)| b).collect()
    }

    /// Whether the cursor is on the overview's `+ new flow`.
    pub(super) fn on_new_flow(&self) -> bool {
        self.view == View::Overview && self.sel.as_deref() == Some(NEW_FLOW)
    }

    /// Words typed in the overview, taken for a flow when the cursor is on one: on a flow, a
    /// change to it, asked of its `flow.md` as it is now; on `+ new flow`, a new one in the
    /// project. Either way proposed in the pane, and the line cleared for it; a `flow.md` that
    /// can't be read is said, and the words stay on the line. Whether they were taken; elsewhere,
    /// or with a proposal up, they are not, and go on as words anywhere do.
    pub(super) fn overview_words(&mut self, words: String) -> bool {
        if self.flow_proposal_shown().is_some() || self.proposal_shown().is_some() {
            return false;
        }
        let Some(name) = self.overview_words_for() else {
            return false;
        };
        let Some(name) = name else {
            let folder = self.overview.as_ref().and_then(Scope::folder).map(Path::to_path_buf);
            self.line.clear();
            self.propose_flow(Aim::New { named: None, folder }, words);
            return true;
        };
        match std::fs::read_to_string(self.home.flow_dir(&name).join("flow.md")) {
            Ok(base) => {
                self.line.clear();
                self.propose_flow(Aim::Change { name, base }, words);
            }
            Err(e) => self.say(format!("{name}'s flow.md can't be read: {e}")),
        }
        true
    }

    /// What words typed in the overview would be taken for: a change to the flow under the
    /// cursor (its name), or a new flow on `+ new flow` (none). Outside the overview, or on
    /// another row, nothing: they are words as anywhere.
    pub(super) fn overview_words_for(&self) -> Option<Option<String>> {
        if self.view != View::Overview {
            return None;
        }
        if self.on_new_flow() {
            return Some(None);
        }
        self.selected_flow().map(|f| Some(f.name.to_string()))
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
        let Some(folder) = scope.folder() else {
            return vec![Line::styled(
                " flows with no in · runs outside every project",
                theme::dim(),
            )];
        };
        let target = SummaryTarget::Project {
            workspace: folder.to_path_buf(),
        };
        let key = target.key();
        let now = Timestamp::now();
        // A new one being written, asked with `/summary` or as the overview opened: it says so
        // above the one it will replace.
        let mut lines = Vec::new();
        let writing = [&self.summarising, &self.overviewing]
            .into_iter()
            .find_map(|s| s.as_ref().filter(|s| s.target == target));
        if let Some(s) = writing {
            let secs = s.since.elapsed().as_secs();
            lines.push(Line::styled(format!(" summarising… {secs}s"), theme::accent()));
        }
        // Another project's being written first: this one is next.
        if self.overview_next.as_ref() == Some(&target)
            && let Some(o) = &self.overviewing
        {
            lines.push(Line::styled(
                format!(" summarising {}… · this one next", o.title),
                theme::dim(),
            ));
        }
        // The one loaded with the cursor's row, else the last one made here, else the one
        // loaded when the cursor was on nothing here.
        let kept = self
            .summary
            .as_ref()
            .filter(|s| s.target == key)
            .or_else(|| self.made.get(&key))
            .or_else(|| self.overview_said.as_ref().filter(|s| s.target == key));
        match kept {
            // Its own blank line first is the overview's to place.
            Some(s) => lines.extend(pane::summary_section(Some(s), width, now).into_iter().skip(1)),
            None if lines.is_empty() => {
                lines.push(Line::styled(" no summary yet · /summary writes it", theme::dim()));
            }
            None => {}
        }
        lines
    }

    /// The key of what the overview's top says, the project's; `everywhere`'s has none.
    pub(super) fn overview_target_key(&self) -> Option<String> {
        let folder = self.overview.as_ref()?.folder()?;
        Some(
            SummaryTarget::Project {
                workspace: folder.to_path_buf(),
            }
            .key(),
        )
    }

    /// The overview: its top, a blank line, its rows as the list draws them, `+ new flow`, and
    /// `parked: N` with its flows under it while it is open, a window of them following the
    /// selection.
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
        if self.on_new_flow() {
            sel_line = lines.len();
        }
        lines.push(self.overview_line(" + new flow".into(), self.on_new_flow(), w));
        keys.push(Some(NEW_FLOW.to_string()));
        let parked = self.parked_in_scope();
        if !parked.is_empty() {
            let fold = if self.parked_open { "▾" } else { "▸" };
            let line = format!(" parked: {} {fold}", parked.len());
            if self.on_parked_line() {
                sel_line = lines.len();
            }
            lines.push(self.overview_line(line, self.on_parked_line(), w));
            keys.push(Some(PARKED.to_string()));
        }
        if self.parked_open {
            let now = Timestamp::now();
            let here = self.parked_here();
            for p in parked {
                let line = format!("   {}  parked {} ago · {}", p.name, fmt_age(p.at, now), p.folder);
                let on = here.as_deref() == Some(p.folder.as_str());
                if on {
                    sel_line = lines.len();
                }
                lines.push(self.overview_line(line, on, w));
                keys.push(Some(parked_key(&p.folder)));
            }
        }
        self.draw_window(f, area, lines, keys, sel_line);
    }

    /// One of the overview's own lines, past its rows: dim, or lit to the edge under the cursor.
    fn overview_line(&self, text: String, on: bool, w: usize) -> Line<'static> {
        let mut line = Line::from(text);
        if !on {
            return line.style(theme::dim());
        }
        let short = w.saturating_sub(line.width());
        if short > 0 {
            line.spans.push(Span::raw(" ".repeat(short)));
        }
        line.style(theme::selected().patch(theme::accent()))
    }
}

/// The start of a parked flow's key under `parked: N`; its folder follows.
const PARKED_ROW: &str = "parked:";

/// A parked flow's key under `parked: N`: `parked:_nightly`.
fn parked_key(folder: &str) -> String {
    format!("{PARKED_ROW}{folder}")
}
