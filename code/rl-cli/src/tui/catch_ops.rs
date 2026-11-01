//! `impl App` — the catch-up view cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// What happened since `since`, in the list's place until you leave it:
    /// `esc` goes to the list, `enter` to the row there. Its digest is the
    /// list's, cut to the stretch (`catchup::stretch`); the list's stays as
    /// it is. `since` is also where a project's or everything's `/summary`
    /// starts while it is up. `came_back`: it opened by itself, since you
    /// were away — only then may its top be written by itself.
    pub(super) async fn open_catch_up(&mut self, since: Timestamp, came_back: bool) {
        let d = crate::catchup::stretch(&self.digest, since);
        // Another stretch over the one up: that one is left, and what it
        // showed is seen.
        if self.view == View::CatchUp {
            self.saw_catch_up().await;
        }
        self.away_since = Some(since);
        self.catch = Some(d);
        *self.leads_shown.get_mut() = None;
        self.fetch_leads().await;
        self.write_top(since, came_back).await;
        self.open_view(View::CatchUp);
        self.leave_subpage();
        self.picked.clear();
        self.events.clear();
        self.settle();
        self.load_pane().await;
    }

    /// The catch-up as it stands, built on the list's own rows and narrowed
    /// by the search, as the list is.
    pub(super) fn catch_up(&self) -> Option<crate::catchup::CatchUp<'_>> {
        self.catch_up_by(&self.filter)
    }

    /// The catch-up as `filter` narrows it; the top is told from all of it
    /// (`""`), whatever the search.
    pub(super) fn catch_up_by(&self, filter: &str) -> Option<crate::catchup::CatchUp<'_>> {
        let d = self.catch.as_ref()?;
        // Drawing, what was built for this frame is laid back over its digest.
        if let Some(kept) = self.drawn.borrow().as_ref()
            && let Some((_, k)) = kept.iter().find(|(f, _)| f == filter)
        {
            return Some(k.over(d));
        }
        let cx = rows::Context {
            names: &self.names,
            flows: &self.flows,
            filter,
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            now: Timestamp::now(),
            relay: Some(&self.relay),
        };
        let cu = crate::catchup::CatchUp::build(d, &cx);
        if let Some(kept) = self.drawn.borrow_mut().as_mut() {
            kept.push((filter.to_string(), cu.keep(d)));
        }
        Some(cu)
    }

    /// The catch-up's rows in order: each project's line, then its rows.
    pub(super) fn catch_rows(&self) -> Vec<Row<'_>> {
        let Some(cu) = self.catch_up() else {
            return vec![];
        };
        cu.projects
            .into_iter()
            .flat_map(|p| std::iter::once(Row::Group(p.head)).chain(p.rows))
            .collect()
    }

    /// The keys of the firings heading the catch-up, newest first.
    pub(super) fn lead_keys(&self) -> Vec<String> {
        let Some(c) = &self.catch else { return vec![] };
        let shown = self.leads_shown.borrow();
        heads::leads(&self.flows, &c.firings, c.since)
            .iter()
            .map(|l| heads::key(l.flow))
            .filter(|k| shown.as_ref().is_none_or(|s| s.contains(k)))
            .collect()
    }

    /// The firing heading the catch-up that the cursor is on.
    pub(super) fn lead_here(&self) -> Option<heads::Lead<'_>> {
        let flow = heads::flow_of(self.sel.as_deref()?)?;
        let c = self.catch.as_ref().filter(|_| self.view == View::CatchUp)?;
        heads::leads(&self.flows, &c.firings, c.since)
            .into_iter()
            .find(|l| l.flow == flow)
    }

    /// What each firing heading the catch-up came to, asked for when it is new or has moved on.
    pub(super) async fn fetch_leads(&mut self) {
        let Some(c) = &self.catch else { return };
        let firings: Vec<Firing> = heads::leads(&self.flows, &c.firings, c.since)
            .iter()
            .map(|l| l.firing.clone())
            .collect();
        self.outcomes.fetch(&mut self.rpc, firings).await;
    }

    /// What leaving the catch-up marks seen: every run it shows that was
    /// not, and where each flow's runs not seen till now start, so its
    /// `/summary` still starts there (as `saw_selected` notes).
    pub(super) fn catch_up_unseen(&self) -> Shown {
        let mut shown = Shown::default();
        for r in self.catch_rows() {
            match r {
                Row::Run(i) if i.unseen => shown.ids.push(i.run.id.clone()),
                Row::Flow(f) => {
                    let unseen: Vec<&DigestItem> = f.runs.iter().copied().filter(|i| i.unseen).collect();
                    if let Some(at) = unseen.iter().map(|i| i.run.started_at).min() {
                        shown.from.push((f.key.clone(), at));
                    }
                    shown.ids.extend(unseen.iter().map(|i| i.run.id.clone()));
                }
                _ => {}
            }
        }
        shown
    }

    /// Leaving the catch-up, however it is left — for the list, or the
    /// panel closing — while it is up or there to go back to: everything it
    /// showed is seen now, and a project's `/summary` starts again from when
    /// you were last here, not from the stretch it showed.
    pub(super) async fn saw_catch_up(&mut self) {
        if self.view != View::CatchUp && !self.back.contains(&View::CatchUp) {
            return;
        }
        self.away_since = self.left;
        let shown = self.catch_up_unseen();
        self.unseen_from.extend(shown.from);
        self.seen(shown.ids).await;
    }

    /// `enter` in the catch-up: the row on the list, with the cursor on it.
    /// A row the list lets go of once it is seen — it ended over a day ago — is read here
    /// instead, in the pane, and seen only once the catch-up is left.
    pub(super) async fn catch_up_enter(&mut self) {
        // A firing heading it is read where it stands: all of it, in the pane.
        if self.lead_here().is_some() {
            self.pane = Pane::Facts;
            self.cover_proposals();
            self.show_pane = true;
            self.load_pane().await;
            return;
        }
        let Some(row) = self.selected() else {
            return self.say("nothing here to open");
        };
        // The list has no flow rows: a flow opens in its project's overview.
        if let Row::Flow(f) = &row {
            let name = f.name.clone();
            drop(row);
            return self.show_flow(&name).await;
        }
        let key = row.key();
        let run = row.item().map(|i| i.run.id.clone());
        let what = match &row {
            Row::Run(i) => i.run.title.clone(),
            Row::Flow(f) => f.name.clone(),
            Row::Outside(c) => c.title(),
            Row::Group(h) => h.name.clone(),
            Row::Question(q) => q.title.clone(),
        };
        if let Some(k) = &key
            && !self.kept_on_the_list(k, run.as_deref())
        {
            self.pane = Pane::Facts;
            self.cover_proposals();
            self.show_pane = true;
            self.load_pane().await;
            return self.say(format!(
                "'{what}' ended over a day ago: off the list once seen, so read here"
            ));
        }
        match run {
            Some(id) => self.show_run(&id).await,
            None => {
                self.back_to_list().await;
                if let Some(k) = &key
                    && !self.keys().contains(k)
                {
                    // A search or a toggle hiding it gives way, as for a run.
                    self.filter.clear();
                    self.reset_toggles();
                }
                self.select(key);
            }
        }
    }

    /// The catch-up's top for its stretch from `since`, as `plan_top` says:
    /// the one shown, if any (else the counts), and one asked for on a task
    /// of its own when it is to be written — the counts stand until it lands.
    pub(super) async fn write_top(&mut self, since: Timestamp, came_back: bool) {
        if let Some(t) = self.topping.take() {
            t.task.abort();
        }
        let has_runs = self.catch_up_by("").is_some_and(|cu| cu.has_runs());
        let plan = crate::catchup::plan_top(&mut self.rpc, since, has_runs, came_back, Timestamp::now()).await;
        self.top = plan.shown;
        let Some(params) = plan.ask else { return };
        let home = self.home.clone();
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<SummaryMake>(params).await
        });
        self.topping = Some(Summarising {
            target: SummaryTarget::All,
            title: "everything".into(),
            deep: false,
            from: Some(since),
            since: Instant::now(),
            task,
        });
    }

    /// The catch-up's top landed: it takes the bullets' place while its
    /// stretch is the one shown. Failing, the bullets stay and a word says
    /// why.
    pub(super) async fn topped(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
        let Some(t) = self.topping.take() else { return };
        match done {
            Ok(Ok(summary)) => {
                self.made.insert(t.target.key(), summary.clone());
                self.top_from(&t, &summary);
            }
            Ok(Err(e)) => self.say(format!("the top: {e:#}")),
            Err(e) if e.is_cancelled() => {}
            Err(e) => self.say(format!("the top: {e}")),
        }
    }

    /// A summary of everything from where the catch-up starts is its top —
    /// unless it only says nothing ran, which the counts say better.
    pub(super) fn top_from(&mut self, s: &Summarising, summary: &Summary) {
        let shown = self.catch.as_ref().map(|c| c.since.as_millisecond());
        if s.target == SummaryTarget::All
            && s.from.map(|f| f.as_millisecond()) == shown
            && crate::catchup::top_worthy(summary)
        {
            self.top = Some(summary.clone());
        }
    }
}
