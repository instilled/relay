//! `impl App` — the events feed view cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// `/events`: the list gives way to every event, newest first, and what
    /// each set off; `/events` again or `esc` goes back.
    pub(super) async fn toggle_events(&mut self) {
        if self.view == View::Events {
            self.go_back();
        } else {
            self.open_view(View::Events);
            self.leave_subpage();
            // Asked again when the view opens: something may be back.
            self.feed.gone.clear();
            self.fetch_events().await;
        }
        self.picked.clear();
        self.events.clear();
        self.refresh().await;
    }

    /// `/report`: the board in the list's place; `/report` again or `esc` goes back.
    pub(super) async fn open_report(&mut self) {
        if self.view == View::Report {
            self.go_back();
        } else {
            self.open_view(View::Report);
            self.leave_subpage();
            self.fetch_report().await;
        }
        self.picked.clear();
        self.events.clear();
    }

    /// The report as the daemon has it now. When the view opens and on the tick.
    pub(super) async fn fetch_report(&mut self) {
        match self.rpc.call::<ReportGet>(ReportParams::default()).await {
            Ok(r) => {
                self.report_failed = false;
                self.report_sel = self.report_sel.min(report::flat_leads(&r).len().saturating_sub(1));
                self.report = Some(r);
            }
            // The last good board stays; the trouble is said once, not every tick.
            Err(e) => {
                if !self.report_failed {
                    self.say(format!("daemon: {e:#}"));
                }
                self.report_failed = true;
            }
        }
    }

    /// The lead under the report's cursor.
    fn report_lead(&self) -> Option<rl_proto::lead::Lead> {
        let rep = self.report.as_ref()?;
        report::flat_leads(rep).get(self.report_sel).map(|l| (*l).clone())
    }

    /// Move the report's cursor by `by` rows, held to the board.
    pub(super) fn report_step(&mut self, by: isize) {
        let n = self.report.as_ref().map_or(0, |r| report::flat_leads(r).len());
        if n > 0 {
            self.report_sel = (self.report_sel as isize + by).clamp(0, n as isize - 1) as usize;
        }
    }

    /// `enter` on a report row: open what it is about. A run opens on the list; a question
    /// is selected there. Other leads have nothing to open here.
    pub(super) async fn act_selected(&mut self) {
        let Some(lead) = self.report_lead() else {
            return self.say("nothing here to open");
        };
        match lead.key.split_once(':') {
            Some(("run", id)) => {
                self.structure_run(id, lead.facts).await;
                self.show_run(id).await;
            }
            Some(("decision", id)) => {
                self.back_to_list().await;
                let key = format!("q:{id}");
                if self.keys().contains(&key) {
                    self.select(Some(key));
                } else {
                    self.say("that question is not on the list");
                }
            }
            _ => self.say(format!("'{}' has nothing to open", lead.title)),
        }
    }

    /// Opening a run from the report: its facts are kept to show in the run's pane, and, if it
    /// has a recap not yet split, the daemon is asked to split it (it asks an assistant only
    /// when `assistants.summary` is set, and only once per run). The one call of the report
    /// that can reach an assistant; it is made here, on the open, never on the board.
    async fn structure_run(&mut self, id: &str, facts: Option<rl_proto::RunFacts>) {
        let Some(mut facts) = facts else { return };
        if self.feed.facts.get(id).is_some_and(|kept| kept.worked_on.is_some()) {
            return;
        }
        if facts.recap.as_deref().is_some_and(|r| !r.trim().is_empty()) && facts.worked_on.is_none() {
            let p = rl_proto::methods::FactsStructureParams { run: id.to_string() };
            match self.rpc.call::<rl_proto::methods::FactsStructure>(p).await {
                Ok(split) => facts = split,
                Err(e) => self.say(format!("split: {e:#}")),
            }
        }
        self.feed.facts.insert(id.to_string(), facts);
    }

    /// `d` on a report row: dismissed (built-in or stored alike), and the board asked again.
    pub(super) async fn dismiss_selected(&mut self) {
        let Some(lead) = self.report_lead() else {
            return self.say("nothing here to dismiss");
        };
        let p = LeadDismissParams {
            source: lead.source,
            key: lead.key,
        };
        match self.rpc.call::<LeadDismiss>(p).await {
            Ok(()) => {
                self.say("dismissed");
                self.fetch_report().await;
            }
            Err(e) => self.say(format!("dismiss: {e:#}")),
        }
    }

    /// `t` on a report row: ask for the tag to give the row's source.
    pub(super) fn tag_prompt(&mut self) {
        let Some(lead) = self.report_lead() else {
            return self.say("nothing here to tag");
        };
        let was = self
            .report
            .as_ref()
            .and_then(|r| r.tags.iter().find(|t| t.source == lead.source))
            .map(|t| t.tag.clone())
            .unwrap_or_default();
        self.mode = Mode::Tag(Box::new(Tagging {
            source: lead.source,
            field: line::Field::selected(was),
        }));
    }

    /// Give `source` the tag `name`; an empty name clears it.
    pub(super) async fn tag_source(&mut self, source: String, name: &str) {
        let name = name.trim();
        let done = if name.is_empty() {
            self.rpc.call::<TagClear>(TagClearParams { source }).await
        } else {
            self.rpc
                .call::<TagSet>(SourceTag {
                    source,
                    tag: name.to_string(),
                })
                .await
        };
        match done {
            Ok(()) => self.fetch_report().await,
            Err(e) => self.say(format!("tag: {e:#}")),
        }
    }

    pub(super) fn event_items(&self) -> Vec<events::Item<'_>> {
        events::build(&self.feed, &self.names, &self.filter)
    }

    /// What is new in the ledger since the newest event read — the newest
    /// page when nothing is — and what is known of what it names. When the
    /// view opens and on the tick; never because the cursor moved.
    pub(super) async fn fetch_events(&mut self) {
        let f = EventFilter {
            newest_first: true,
            after: self.feed.newest(),
            limit: Some(events::PAGE),
            ..Default::default()
        };
        match self.rpc.call::<EventList>(f).await {
            Ok(page) => self.feed.newer(page, events::PAGE as usize),
            Err(e) => return self.say(format!("daemon: {e:#}")),
        }
        self.fill_feed().await;
    }

    /// The page before the oldest event read: the cursor reached the end.
    pub(super) async fn fetch_older(&mut self) {
        let f = EventFilter {
            newest_first: true,
            before: self.feed.oldest(),
            limit: Some(events::PAGE),
            ..Default::default()
        };
        match self.rpc.call::<EventList>(f).await {
            Ok(page) => self.feed.older(page, events::PAGE as usize),
            Err(e) => return self.say(format!("daemon: {e:#}")),
        }
        self.fill_feed().await;
    }

    /// The runs and questions the events name: the list's own as they
    /// stand, the rest — archived runs, answered questions — asked for once.
    pub(super) async fn fill_feed(&mut self) {
        for i in &self.digest.items {
            self.feed.runs.insert(i.run.id.clone(), i.run.clone());
        }
        for d in &self.digest.decisions {
            self.feed.questions.insert(d.id.clone(), d.clone());
        }
        let mut missing = self.feed.unknown_runs();
        if missing.len() > EVENT_ASKS {
            // A page's worth at once: the newest runs, archived too, in one go.
            let f = RunFilter {
                archived: true,
                limit: Some(events::PAGE),
                ..Default::default()
            };
            if let Ok(runs) = self.rpc.call::<RunList>(f).await {
                self.feed.runs.extend(runs.into_iter().map(|r| (r.id.clone(), r)));
            }
            missing = self.feed.unknown_runs();
        }
        // The rest a few at a time, so a tick stays a tick.
        for id in missing.into_iter().take(EVENT_ASKS) {
            match self.rpc.call::<RunGet>(RunId { id: id.clone() }).await {
                Ok(r) => {
                    self.feed.runs.insert(id, r);
                }
                Err(_) => {
                    self.feed.gone.insert(id);
                }
            }
        }
        for id in self.feed.unknown_questions().into_iter().take(EVENT_ASKS) {
            match self.rpc.call::<DecisionGet>(DecisionId { id: id.clone() }).await {
                Ok(d) => {
                    self.feed.questions.insert(id, d);
                }
                Err(_) => {
                    self.feed.gone.insert(id);
                }
            }
        }
    }

    /// A key that moves the cursor in the events view: along the lines, and
    /// nothing fetched for it — only the older page, once it reaches the
    /// end. False when the key is not one of those.
    pub(super) async fn events_step(&mut self, code: KeyCode) -> bool {
        let page = self.list_h.get().saturating_sub(2).max(1) as isize;
        let by = match code {
            KeyCode::Down => 1,
            KeyCode::Up => -1,
            KeyCode::Home => isize::MIN / 2,
            KeyCode::End => isize::MAX / 2,
            KeyCode::PageDown => page,
            KeyCode::PageUp => -page,
            _ => return false,
        };
        self.step(by);
        if self.feed.more && self.sel.is_some() && self.keys().last() == self.sel.as_ref() {
            self.fetch_older().await;
        }
        true
    }

    /// The mouse over the events view: the wheel steps, a move or a click
    /// puts the cursor on the line — and nothing is fetched for it.
    pub(super) async fn events_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => {
                self.events_step(KeyCode::Down).await;
            }
            MouseEventKind::ScrollUp => {
                self.events_step(KeyCode::Up).await;
            }
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                let key = hit_row(&self.hit.borrow(), m.column, m.row);
                if key.is_some() && key != self.sel {
                    self.select(key);
                }
            }
            _ => {}
        }
    }

    /// Where `enter` on the line under the cursor goes.
    pub(super) fn event_target(&self) -> Option<events::Target> {
        let key = self.sel.as_deref()?;
        events::target(&self.event_items(), key)
    }

    /// `enter` in the events view: an event goes to its run on the list, a
    /// line a flow set off to that flow's row.
    pub(super) async fn events_enter(&mut self) {
        match self.event_target() {
            Some(events::Target::Run(id)) => self.show_run(&id).await,
            Some(events::Target::Flow(name)) => self.show_flow(&name).await,
            None => self.say("nothing here to open"),
        }
    }
}
