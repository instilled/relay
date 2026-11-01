//! `impl App` — the terminal join, attach & resume cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// Back from a join: to the list, with the cursor on the run and no page
    /// over it, and a word on how it went.
    pub(super) fn back_from_join(&mut self, id: &str, ended: crate::attach::Ended) {
        use crate::attach::Ended;
        let title = self
            .run_by_id(id)
            .map(|r| r.title.clone())
            .unwrap_or_else(|| id.to_string());
        match ended {
            Ended::Closed { .. } => {
                self.say(self.gone_words(id, &title));
            }
            Ended::Exit { how, .. } => self.say(format!("{title} ended ({how})")),
            Ended::Detached | Ended::Left => self.say(format!("let go of {title} · it keeps going")),
        }
        self.select(Some(format!("r:{id}")));
        self.leave_subpage();
    }

    /// A join that did not happen: someone has the keyboard — asked whether
    /// to take it from them — or it failed.
    pub(super) fn join_refused(&mut self, id: String, e: &anyhow::Error) {
        match held_by(e) {
            Some(who) => self.mode = Mode::Take(id, who),
            None => self.say(format!("join: {e:#}")),
        }
    }

    /// What became of run `id`, known as `title` if the list no longer has it.
    pub(super) fn gone_words(&self, id: &str, title: &str) -> String {
        let title = self
            .run_by_id(id)
            .map(|r| r.title.clone())
            .unwrap_or_else(|| title.to_string());
        match self.run_by_id(id).map(|r| r.state) {
            Some(RunState::Failed) => format!("{title} failed · the session holder went away"),
            Some(RunState::Finished) => format!("{title} finished"),
            Some(RunState::Stopped) => format!("{title} was stopped"),
            _ => format!("lost the connection to {title} · it may still be going"),
        }
    }

    /// An ended run's terminal, re-rendered in the pane to read. A live run
    /// is never shown here: `enter` on one hands the real terminal over
    /// (`raw::attach`).
    pub(super) async fn open_term(&mut self, area: Rect) {
        let Some(id) = self.selected_run().map(|i| i.run.id.clone()) else {
            self.say("nothing here to open");
            return;
        };
        self.open_term_on(id, area).await;
    }

    /// What run `id` printed, in a page of its own.
    pub(super) async fn open_term_on(&mut self, id: String, area: Rect) {
        // Still reading this one: back to it, where it was left.
        if self.term.as_ref().is_some_and(|t| t.id == id) {
            self.pane = Pane::Term;
            self.cover_proposals();
            self.show_pane = true;
            self.term_size = self.pane_size(area);
            return;
        }
        self.seen(vec![id.clone()]).await;
        self.pane = Pane::Term;
        self.cover_proposals();
        self.show_pane = true;
        let size = self.pane_size(area);
        match Output::open(&self.home, &id, size).await {
            Ok(t) => {
                // The one read before is left: looked at.
                if let Some(was) = self.term.replace(t) {
                    self.read_terms.push(was.id);
                }
                self.term_size = size;
            }
            Err(e) => {
                self.pane = Pane::Facts;
                self.say(format!("attach: {e:#}"));
            }
        }
    }

    /// A step's run, from its flow's pane: one still going joined, an ended one read in the
    /// tile — as `enter` on its own row would.
    pub(super) async fn join_step(&mut self, run: String, area: Rect) {
        let live = match self.run_by_id(&run) {
            Some(r) => !r.state.is_terminal(),
            None => self
                .rpc
                .call::<RunGet>(RunId { id: run.clone() })
                .await
                .is_ok_and(|r| !r.state.is_terminal()),
        };
        if live {
            self.attach = Some((run, false));
        } else {
            self.open_term_on(run, area).await;
        }
    }

    /// The run that asked, in its terminal: selected on the list and joined,
    /// as `enter` on it would.
    pub(super) async fn attach_to(&mut self, run: String, area: Rect) {
        self.back_to_list().await;
        let key = format!("r:{run}");
        if !self.keys().contains(&key) {
            self.say("the run that asked is no longer on the list");
            return;
        }
        self.select(Some(key));
        self.open_run(area).await;
    }

    /// Enter (or `/join`) on an agent's own session (`carry`): one in
    /// Claude's background gets the real terminal, handed over by
    /// `main_loop` once ratatui has stepped aside; one in another terminal
    /// is carried on here as a run, in the background (`carried` says how it
    /// went); one carried on already goes to its run.
    pub(super) async fn enter_outside(&mut self, s: &rl_proto::methods::Outside, area: Rect) {
        if let Some(run) = &s.run {
            self.select(Some(format!("r:{run}")));
            return self.say(format!("{} is carried on here already: this run", s.title()));
        }
        match carry::enter(s, (self.agent_bin)(&s.agent).as_deref(), &carry::attach_args(&s.agent)) {
            Err(why) => self.say(why),
            Ok(carry::Enter::Attach(argv)) => self.attach_outside = Some(argv),
            Ok(carry::Enter::CarryOn) => {
                if let Some(c) = &self.carrying {
                    return self.say(format!("carrying {} on already", c.session.title()));
                }
                let env = match crate::env::child_env(&[]) {
                    Ok(e) => e,
                    Err(e) => return self.say(format!("carry on: {e:#}")),
                };
                let p = carry::move_params(s, env, spawn_size(area));
                let home = self.home.clone();
                let task =
                    tokio::spawn(
                        async move { crate::client::Client::connect(&home).await?.call::<ImportMove>(p).await },
                    );
                self.carrying = Some(Carrying {
                    session: s.clone(),
                    task,
                });
            }
        }
    }

    /// `rld`'s word on a session being carried on: the run it now is
    /// becomes the selection, so enter joins it — while the cursor is still
    /// on the session; moved on meanwhile, it stays where it is, and the run
    /// is named; or why not.
    pub(super) async fn carried(&mut self, done: Result<Result<Run>, tokio::task::JoinError>) {
        let Some(c) = self.carrying.take() else { return };
        match done {
            Ok(Ok(run)) => {
                let here = self.sel.as_deref() == Some(c.session.key().as_str());
                self.refresh().await;
                if here {
                    self.select(Some(format!("r:{}", run.id)));
                    self.say(carry::carried_on(&c.session));
                } else {
                    self.say(carry::carried_on_there(&c.session, &run.title));
                }
            }
            Ok(Err(e)) => self.say(format!("{e:#}")),
            Err(e) => self.say(format!("carry on: {e}")),
        }
    }

    /// The run under the cursor, itself: one still going is joined — its
    /// real terminal and its keyboard, handed over by `main_loop` once
    /// ratatui has stepped aside — and an ended one is read in the pane.
    pub(super) async fn open_run(&mut self, area: Rect) {
        let Some((id, ended)) = self
            .selected_run()
            .map(|i| (i.run.id.clone(), i.run.state.is_terminal()))
        else {
            return;
        };
        if ended {
            self.open_term(area).await;
        } else {
            self.attach = Some((id, false));
        }
    }

    /// `/resume`: runs picked back up, and in a word what came of it. One run
    /// becomes the selection, so `enter` joins it.
    pub(super) async fn resume(&mut self, mut p: RunResumeParams, area: Rect) {
        p.env = match crate::env::child_env(&[]) {
            Ok(e) => e,
            Err(e) => return self.say(format!("resume: {e:#}")),
        };
        let (cols, rows) = spawn_size(area);
        p.cols = Some(cols);
        p.rows = Some(rows);
        let lost = p.lost;
        let out = match self.rpc.call::<RunResume>(p).await {
            Ok(o) => o,
            Err(e) => return self.say(format!("resume: {e:#}")),
        };
        self.refresh().await;
        let done: Vec<_> = out.iter().filter_map(|r| r.run.as_ref().map(|n| (r, n))).collect();
        let left_out = out
            .iter()
            .filter(|r| r.why_not.as_deref() == Some(rl_proto::methods::RESUME_LEFT_OUT))
            .count();
        let failed: Vec<_> = out
            .iter()
            .filter(|r| r.run.is_none() && r.why_not.as_deref() != Some(rl_proto::methods::RESUME_LEFT_OUT))
            .collect();
        if let [(_, new)] = done.as_slice() {
            self.select(Some(format!("r:{}", new.id)));
        }
        let mut words = Vec::new();
        match done.as_slice() {
            [] if out.is_empty() && lost => words.push("nothing lost to resume".to_string()),
            [] => {}
            [(r, _)] => words.push(format!("resumed {} · enter joins it", r.title)),
            many => words.push(format!("resumed {}", many.len())),
        }
        match failed.as_slice() {
            [] => {}
            [one] => words.push(format!(
                "{}: {}",
                one.title,
                one.why_not.as_deref().unwrap_or("not resumed")
            )),
            many => words.push(format!("{} could not be · rl resume says why", many.len())),
        }
        if left_out > 0 {
            let s = if left_out == 1 { "" } else { "s" };
            words.push(format!("left {left_out} command{s} out · /resume all commands"));
        }
        if !words.is_empty() {
            self.say(words.join(" · "));
        }
    }
}
