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
            self.report_opens().await;
        }
        self.picked.clear();
        self.events.clear();
    }

    /// How long you were away before this panel opened: fixed for the
    /// session, however long it stays up. None: the first open.
    pub(super) fn away(&self) -> Option<SignedDuration> {
        self.left.map(|l| self.opened_at.duration_since(l))
    }

    /// The report's header: how long you were away and since when, what waits for your
    /// review and what is working. Time away is left out while it is not known.
    pub(super) fn report_head(&self) -> String {
        let Some(rep) = &self.report else {
            return "report".into();
        };
        let i = inbox::inbox(rep);
        let mut parts = Vec::new();
        if let Some(left) = self.left {
            parts.push(format!("away {}", fmt_age(left, self.opened_at)));
            parts.push(format!("since {}", rl_core::time::fmt_when(left, Timestamp::now())));
        }
        parts.push(format!("{} to review", i.to_review));
        parts.push(format!("{} working", i.working));
        parts.join(" · ")
    }

    /// As the report opens: the inbox fetched, and the selection where time
    /// away puts it (`inbox::opening`). Nothing is written.
    pub(super) async fn report_opens(&mut self) {
        self.inbox_inside = None;
        self.inbox_sel = inbox::Sel::All;
        self.report_open = false;
        self.item_view = None;
        self.fetch_report().await;
        if let Some(rep) = &self.report {
            let after = match self.rpc.call::<ConfigGet>(()).await {
                Ok(cfg) => cfg.config.report.brief_after(),
                Err(_) => rl_proto::config::ReportConfig::default().brief_after(),
            };
            self.inbox_sel = inbox::opening(&inbox::inbox(rep), self.away(), after);
            self.load_item().await;
        }
    }

    /// The report as the daemon has it now.
    /// When the view opens and on the tick. The selection stays where it was while it is
    /// still there; gone, it moves to what came after it.
    pub(super) async fn fetch_report(&mut self) {
        match self.rpc.call::<ReportGet>(ReportParams::default()).await {
            Ok(r) => {
                self.report_failed = false;
                let was = self.report.replace(r);
                self.keep_selection(was.as_ref());
                self.load_item().await;
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

    /// The selection, held over a new report: kept while it is still there; gone, the next
    /// in the order it was in that still is, else its team, else all the teams.
    fn keep_selection(&mut self, was: Option<&rl_proto::methods::Report>) {
        let Some(rep) = &self.report else { return };
        let now = inbox::inbox(rep);
        let inside = self.inbox_inside.as_deref();
        if inbox::order(&now, inside).contains(&self.inbox_sel) {
            return;
        }
        let then = was.map(inbox::inbox);
        let old = then.as_ref().map(|i| inbox::order(i, inside)).unwrap_or_default();
        let after = old.iter().skip_while(|s| **s != self.inbox_sel).skip(1);
        let here = inbox::order(&now, inside);
        if let Some(next) = after.into_iter().find(|s| here.contains(s)) {
            self.inbox_sel = next.clone();
            return;
        }
        // Its team, from the list of teams; or, failing that, all of them.
        let team = match (&self.inbox_sel, &then) {
            (inbox::Sel::Item(key), Some(i)) => i
                .teams
                .iter()
                .find(|t| t.items.iter().any(|l| l.key == *key))
                .map(|t| t.name.clone()),
            _ => None,
        };
        self.inbox_inside = None;
        self.inbox_sel = match team {
            Some(name) if now.teams.iter().any(|t| t.name == name) => inbox::Sel::Team(name),
            _ => inbox::Sel::All,
        };
    }

    /// The selected item read whole, once a selection: asked when it changes, never again
    /// for the same one. Reading it reviews nothing.
    pub(super) async fn load_item(&mut self) {
        let inbox::Sel::Item(key) = &self.inbox_sel else {
            return;
        };
        if self.item_view.as_ref().is_some_and(|(k, _)| k == key) {
            return;
        }
        let key = key.clone();
        // Only a run has a session to read; anything else has no log to show.
        let view = match key.strip_prefix("run:") {
            Some(id) => match self
                .rpc
                .call::<QueueItemGet>(QueueItemParams { run: id.to_string() })
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    self.say(format!("read: {e:#}"));
                    Default::default()
                }
            },
            None => Default::default(),
        };
        self.item_view = Some((key, view));
    }

    /// The lead the inbox has selected: the item, or a team's first; none on all the teams.
    fn inbox_lead(&self) -> Option<rl_proto::lead::Lead> {
        let rep = self.report.as_ref()?;
        let all = || rep.needs_you.iter().chain(&rep.live).chain(&rep.happened);
        match &self.inbox_sel {
            inbox::Sel::Item(key) => all().find(|l| l.key == *key).cloned(),
            inbox::Sel::Team(name) => {
                let i = inbox::inbox(rep);
                let team = i.teams.iter().find(|t| t.name == *name)?;
                team.items.first().map(|l| (*l).clone())
            }
            inbox::Sel::All => None,
        }
    }

    /// `j`/`k` and the arrows: `by` steps along the inbox's order, held to its ends.
    pub(super) async fn report_step(&mut self, by: isize) {
        let Some(rep) = &self.report else { return };
        let order = inbox::order(&inbox::inbox(rep), self.inbox_inside.as_deref());
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|s| *s == self.inbox_sel).unwrap_or(0);
        let to = (at as isize).saturating_add(by).clamp(0, order.len() as isize - 1) as usize;
        self.inbox_sel = order[to].clone();
        self.load_item().await;
    }

    /// `enter` on the inbox: into a team, its first item selected; on an item, its session
    /// taken over the terminal (a question goes to its row on the list). Below
    /// `INBOX_SPLIT_W` the reading pane is one pane of its own: the first `enter` opens it,
    /// the next does the rest.
    pub(super) async fn report_enter(&mut self, wide: bool) {
        if !wide && !self.report_open {
            self.report_open = true;
            return;
        }
        match self.inbox_sel.clone() {
            inbox::Sel::All => {}
            inbox::Sel::Team(name) => {
                self.report_open = false;
                let first = self
                    .report
                    .as_ref()
                    .and_then(|r| inbox::order(&inbox::inbox(r), Some(&name)).into_iter().next());
                self.inbox_inside = Some(name);
                if let Some(first) = first {
                    self.inbox_sel = first;
                }
                self.load_item().await;
            }
            inbox::Sel::Item(key) => match key.strip_prefix("run:") {
                Some(id) => self.attach = Some((id.to_string(), false)),
                None => self.act_selected().await,
            },
        }
    }

    /// `esc` on the inbox: the reading pane closed, below `INBOX_SPLIT_W`; out of a team,
    /// onto its row; at the top, back to the list. False when it went back to the list.
    pub(super) fn report_back(&mut self) -> bool {
        if self.report_open {
            self.report_open = false;
        } else if let Some(name) = self.inbox_inside.take() {
            self.inbox_sel = inbox::Sel::Team(name);
        } else {
            return false;
        }
        true
    }

    /// `enter` on a lead with no session to take over: a question is selected on the list.
    /// Other leads have nothing to open here.
    pub(super) async fn act_selected(&mut self) {
        let Some(lead) = self.inbox_lead() else {
            return self.say("nothing here to open");
        };
        match lead.key.split_once(':') {
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

    /// `x`: the item reviewed (a lead with no item, dismissed), and the next one selected;
    /// on a team, every queued item of it. The board is asked again.
    pub(super) async fn review_selected(&mut self) {
        let Some(rep) = &self.report else { return };
        let i = inbox::inbox(rep);
        let next = inbox::order(&i, self.inbox_inside.as_deref())
            .into_iter()
            .skip_while(|s| *s != self.inbox_sel)
            .nth(1);
        let (runs, lead): (Vec<String>, _) = match &self.inbox_sel {
            inbox::Sel::All => (Vec::new(), None),
            inbox::Sel::Team(name) => {
                let items = i.teams.iter().filter(|t| t.name == *name).flat_map(|t| &t.items);
                let queued = items.filter(|l| l.rest.is_some());
                (queued.filter_map(|l| run_of(l)).collect(), None)
            }
            inbox::Sel::Item(key) => match i.teams.iter().flat_map(|t| &t.items).find(|l| l.key == *key) {
                Some(l) if l.rest.is_some() => (run_of(l).into_iter().collect(), None),
                Some(l) => (Vec::new(), Some((*l).clone())),
                None => (Vec::new(), None),
            },
        };
        let done = match lead {
            Some(lead) => {
                let p = LeadDismissParams {
                    source: lead.source,
                    key: lead.key,
                };
                self.rpc.call::<LeadDismiss>(p).await.map(|()| "dismissed".to_string())
            }
            None if runs.is_empty() => return self.say("nothing here waits for review"),
            None => {
                let n = runs.len();
                let said = if n == 1 {
                    "reviewed".into()
                } else {
                    format!("{n} reviewed")
                };
                self.rpc
                    .call::<QueueReview>(QueueReviewParams { runs })
                    .await
                    .map(|_| said)
            }
        };
        match done {
            Ok(said) => {
                self.say(said);
                if let Some(next) = next {
                    self.inbox_sel = next;
                }
                self.fetch_report().await;
            }
            Err(e) => self.say(format!("review: {e:#}")),
        }
    }

    /// `r` on an item: a prompt for its session, typed on the line.
    pub(super) fn reply_prompt(&mut self) {
        let Some(lead) = self
            .inbox_lead()
            .filter(|_| matches!(self.inbox_sel, inbox::Sel::Item(_)))
        else {
            return self.say("r replies to an item");
        };
        let Some(id) = lead.key.strip_prefix("run:") else {
            return self.say(format!("'{}' has no session to reply to", lead.title));
        };
        self.mode = Mode::Reply(Box::new(Replying {
            run: id.to_string(),
            title: lead.title,
            field: line::Field::selected(""),
        }));
    }

    /// The reply sent, as if typed and `enter` pressed. Typing to a run reviews it, the
    /// daemon's side.
    pub(super) async fn send_reply(&mut self, run: String, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\r');
        match self.rpc.call::<RunInput>(RunInputParams { id: run, bytes }).await {
            Ok(()) => {
                self.say("sent");
                self.fetch_report().await;
            }
            Err(e) => self.say(format!("reply: {e:#}")),
        }
    }

    /// `p` on an item: a deep read of its session since you last looked, from the log; what
    /// it would cost is asked first over one call. It lands in the reading pane.
    pub(super) async fn deep_read_selected(&mut self) {
        if !matches!(self.inbox_sel, inbox::Sel::Item(_)) {
            return self.say("open an item to read it deeply");
        }
        if let Some(s) = &self.summarising {
            return self.say(format!("still summarising `{}`; ^x on its run stops it", s.title));
        }
        let Some(lead) = self.inbox_lead() else { return };
        let Some(id) = lead.key.strip_prefix("run:") else {
            return self.say(format!("'{}' has no session to read", lead.title));
        };
        let params = SummaryParams {
            target: SummaryTarget::Run { id: id.to_string() },
            deep: true,
            from: lead.rest.as_ref().map(|r| r.item.from),
            again: false,
            dry: false,
            timeout: None,
        };
        let dry = SummaryParams {
            dry: true,
            ..params.clone()
        };
        match self.rpc.call::<SummaryMake>(dry).await {
            Ok(d) if d.calls > 1 => {
                self.summary_ask = Some(params);
                self.mode = Mode::SummaryCost {
                    runs: d.runs,
                    calls: d.calls,
                };
            }
            Ok(_) => {
                self.say(format!("reading {} deeply…", lead.title));
                self.summary_start(params);
            }
            Err(e) => self.say(format!("{e:#}")),
        }
    }

    /// `t` on the inbox: ask for the tag to give the selected item's source, or its team's.
    pub(super) fn tag_prompt(&mut self) {
        let Some(lead) = self.inbox_lead() else {
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

/// The run a lead is of, by its key.
fn run_of(lead: &rl_proto::lead::Lead) -> Option<String> {
    lead.key.strip_prefix("run:").map(str::to_string)
}
