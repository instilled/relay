//! `impl App` — the summaries & assistant Q&A cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// What `/summary` on the selected row is of: everything on the
    /// catch-up's top; a question's, the run that asked it.
    pub(super) fn summary_target(&self) -> Option<SummaryTarget> {
        if let Some(l) = self.lead_here() {
            return Some(SummaryTarget::Firing {
                id: l.firing.id.clone(),
            });
        }
        if self.on_top() {
            return Some(SummaryTarget::All);
        }
        if self.view == View::Overview {
            // On `+ new flow`, or on nothing: the project's. Everywhere's is not a summary of
            // its own (`All` is the catch-up's), so it has none.
            let Some(row) = self.selected() else {
                return match self.overview.as_ref()? {
                    overview::Scope::Project(p) => Some(SummaryTarget::Project { workspace: p.clone() }),
                    overview::Scope::Everywhere => None,
                };
            };
            return self.target_of(&row);
        }
        self.target_of(&self.selected()?)
    }

    /// What a summary of `row` is of: a run; a flow's runs, wherever they
    /// ran; a project; everything, on the `everywhere` header.
    pub(super) fn target_of(&self, row: &Row) -> Option<SummaryTarget> {
        Some(match row {
            Row::Run(i) => SummaryTarget::Run { id: i.run.id.clone() },
            // An older firing picked in its pane is what is told; else the flow.
            Row::Flow(f) => match self.flow_pane.picked_firing().filter(|x| x.flow == f.name) {
                Some(x) => SummaryTarget::Firing { id: x.id.clone() },
                None => SummaryTarget::Flow { name: f.name.clone() },
            },
            Row::Group(h) if h.is_everywhere() => SummaryTarget::All,
            Row::Group(h) => SummaryTarget::Project {
                workspace: h.workspace.clone(),
            },
            // A question is told by the run that asked it.
            Row::Question(q) => SummaryTarget::Run { id: q.run.clone()? },
            // An agent's own session is not Relay's to tell.
            Row::Outside(_) => return None,
        })
    }

    /// The summary kept of `row`, when the one loaded is of it.
    pub(super) fn summary_of_row(&self, row: &Row) -> Option<&Summary> {
        let s = self.summary.as_ref()?;
        match self.target_of(row)? {
            SummaryTarget::Run { id } => (s.run == id).then_some(s),
            t => (s.target == t.key()).then_some(s),
        }
    }

    /// `enter` on the ask line: the words go to the assistant — outside a flow, the run proposal's
    /// pane open on them at once; for a flow, the flow proposal's. The panel stays live and the
    /// answer comes back through the main loop.
    pub(super) fn ask(&mut self, words: String, want: crate::context::Want) {
        if words.trim().is_empty() {
            return;
        }
        match want {
            Want::Any => {
                // Words with a proposal up change it.
                let sent = self.propose(words.clone());
                let folder = match &self.proposal {
                    Some(p) => p.folder.clone(),
                    None => self.folder_here(),
                };
                self.spawn_ask(sent, folder, Want::Any, words);
            }
            Want::Flow { name } => self.propose_flow(
                flow_proposal::Aim::New {
                    named: Some(name),
                    folder: None,
                },
                words,
            ),
            Want::NewFlow { folder } => self.propose_flow(flow_proposal::Aim::New { named: None, folder }, words),
        }
    }

    pub(super) async fn answered(
        &mut self,
        answer: std::result::Result<
            Result<(crate::interpret::Answer, rl_proto::methods::AssistantOutput)>,
            tokio::task::JoinError,
        >,
    ) {
        let Some(asking) = self.asking.take() else { return };
        match answer {
            // Asked for a flow: whatever came, into its pane.
            answer if !matches!(asking.want, Want::Any) => self.flow_answered(asking.want, asking.words, answer),
            // A flow where a run was asked: the run proposal becomes the flow's.
            Ok(Ok((crate::interpret::Answer::Flow(d), out))) => {
                self.run_proposal_to_flow(&asking.words);
                let answer = Ok(Ok((crate::interpret::Answer::Flow(d), out)));
                self.flow_answered(Want::NewFlow { folder: None }, asking.words, answer);
            }
            Ok(Ok((crate::interpret::Answer::Run(draft), out))) => self.proposed(draft, out.agent),
            Ok(Err(e)) => self.not_proposed(format!("{e:#}")),
            Err(e) => self.not_proposed(format!("asking: {e}")),
        }
        self.refresh().await;
    }

    /// `/summary [deep] [stretch]`: what happened in the row under the
    /// cursor — a run, a flow's runs, a project's, or everything on the
    /// `everywhere` header — brief from the recaps or deep from the logs,
    /// since you last looked unless a stretch is typed. Deep over more than
    /// one assistant call says how many first and waits for `y`.
    pub(super) async fn summary(&mut self, arg: &str) {
        if let Some(s) = &self.summarising {
            self.say(format!("still summarising `{}`; ^x on its run stops it", s.title));
            return;
        }
        if self.on_top() && self.topping.is_some() {
            self.say("the top is still being written; /summary on a row meanwhile");
            return;
        }
        let Some(target) = self.summary_target() else {
            self.say(if self.selected_question().is_some() {
                "this question has no run to summarise"
            } else if self.view == View::Overview {
                "/summary works on a flow here"
            } else {
                "/summary works on a run, a flow, a project, everything or a question"
            });
            return;
        };
        if let SummaryTarget::Run { id } = &target
            && self
                .run_by_id(id)
                .is_some_and(|r| matches!(r.cause, Cause::Assistant(_)))
        {
            self.say("an assistant's own run; nothing to summarise");
            return;
        }
        let now = Timestamp::now();
        let SummaryArgs {
            deep,
            again,
            from: typed,
        } = match summary_args(arg, now) {
            Ok(a) => a,
            Err(e) => {
                self.say(format!("{e:#}"));
                return;
            }
        };
        // A run's own stretch is the daemon's to say (since you looked at
        // it); a flow's, from the firing you had not seen when the cursor
        // landed on it, else the daemon's; a project's and everything's,
        // since you were here.
        let from = match target {
            SummaryTarget::Project { .. } | SummaryTarget::All => Some(
                typed
                    .or(self.away_since)
                    .unwrap_or(now - SignedDuration::from_hours(24)),
            ),
            SummaryTarget::Flow { .. } if typed.is_none() => {
                self.sel.as_ref().and_then(|k| self.unseen_from.get(k)).copied()
            }
            _ => typed,
        };
        let params = SummaryParams {
            target,
            deep,
            from,
            again,
            dry: false,
            timeout: None,
        };
        // What it would cost, first, when it could fan out: a deep read (in
        // parts), or a brief over many runs (one each, and the join).
        let over = !matches!(params.target, SummaryTarget::Run { .. });
        if deep || over {
            let dry = SummaryParams {
                dry: true,
                ..params.clone()
            };
            let ask_over = if deep { 1 } else { crate::cost::ASK_OVER };
            match self.rpc.call::<SummaryMake>(dry).await {
                Ok(d) if d.calls > ask_over => {
                    self.summary_ask = Some(params);
                    self.mode = Mode::SummaryCost {
                        runs: d.runs,
                        calls: d.calls,
                    };
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    self.say(format!("{e:#}"));
                    return;
                }
            }
        }
        self.summary_start(params);
    }

    /// Hands `params` to the summariser; the answer lands in the pane through
    /// the main loop.
    pub(super) fn summary_start(&mut self, params: SummaryParams) {
        // A flow's note of where its unseen firings began is used once asked.
        if let Some(k) = &self.sel
            && params.from.is_some()
            && self.unseen_from.get(k) == params.from.as_ref()
        {
            self.unseen_from.remove(k);
        }
        let title = match &params.target {
            SummaryTarget::Run { id } => self
                .run_by_id(id)
                .map(|r| r.title.clone())
                .unwrap_or_else(|| id.clone()),
            SummaryTarget::Flow { name } => format!("flow {name}"),
            SummaryTarget::Firing { id } => match self.digest.firings.iter().find(|f| f.id == *id) {
                Some(f) => format!("a firing of {}", f.flow),
                None => "a firing".into(),
            },
            SummaryTarget::Project { workspace } => self.names.workspace(workspace),
            SummaryTarget::All => "everything".into(),
        };
        let (target, deep, from) = (params.target.clone(), params.deep, params.from);
        let home = self.home.clone();
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<SummaryMake>(params).await
        });
        // The summary is in the details, not over a terminal being watched:
        // that stays open, a step away. The top's is in the top — when it is
        // over the stretch the catch-up shows; over another, it is the pane's.
        // The report's is in its reading pane.
        let shown = self.catch.as_ref().map(|c| c.since.as_millisecond());
        let tops = self.on_top() && shown == from.map(|f| f.as_millisecond());
        // The overview's own is in its top.
        let in_top = self.view == View::Overview && self.overview.as_ref().is_some_and(|o| o.target() == target);
        if !tops && !in_top && self.view != View::Report {
            self.pane = Pane::Facts;
            self.show_pane = true;
        }
        self.summarising = Some(Summarising {
            target,
            title,
            deep,
            from,
            since: Instant::now(),
            task,
        });
    }

    pub(super) async fn summarised(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
        let Some(s) = self.summarising.take() else { return };
        match done {
            Ok(Ok(summary)) => {
                let what = match s.target {
                    SummaryTarget::Run { .. } => "recap",
                    _ => "summary",
                };
                self.say(format!(
                    "{what} of `{}` · {}",
                    s.title,
                    pane::recap_label(&summary, Timestamp::now())
                ));
                self.made.insert(s.target.key(), summary.clone());
                // A run's deep read is the report's too: its reading pane shows it.
                if let SummaryTarget::Run { id } = &s.target
                    && summary.deep
                {
                    self.briefs.insert(id.clone(), summary.clone());
                }
                self.top_from(&s, &summary);
                if self.summary_target().as_ref() == Some(&s.target) {
                    self.summary = Some(summary);
                }
            }
            Ok(Err(e)) => self.say(format!("{e:#}")),
            Err(e) => self.say(format!("summarising: {e}")),
        }
        self.refresh().await;
    }

    /// `/since [stretch]`: the catch-up over the stretch typed, else since
    /// you were last here, else the last day — from any view; `esc` goes to
    /// the list.
    pub(super) async fn since(&mut self, arg: &str) {
        let now = Timestamp::now();
        let since = match arg.trim() {
            "" => self.left.unwrap_or(now - SignedDuration::from_hours(24)),
            typed => match rl_core::time::parse_since(typed, now) {
                Ok(t) => t,
                Err(e) => return self.say(format!("{e:#}")),
            },
        };
        self.open_catch_up(since, false).await;
    }
}
