//! `impl App` — the key, mouse & input dispatch cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// The wheel steps the selection — the list scrolls to follow the cursor,
    /// so there is no scroll of its own to move — and the cursor travels with
    /// the pointer: moving over a row puts the cursor on it, and so does a
    /// click. All only over the list itself.
    ///
    /// Moving is not looking, though. Passing over a row on the way somewhere
    /// else must not mark it seen, and a pointer sweeps many rows a second, so
    /// a move only selects; the wheel and a click count as looking and refresh.
    pub(super) async fn mouse(&mut self, m: MouseEvent, area: Rect) -> Result<()> {
        // A name being changed: a click anywhere but its row, or the wheel,
        // lets it go and then does what it does. Passing over the list does
        // nothing, so the cursor stays on the row being typed in.
        if let Mode::Rename(r) = &self.mode {
            let on_it = hit_row(&self.hit.borrow(), m.column, m.row).as_deref() == Some(r.key.as_str());
            match m.kind {
                MouseEventKind::Down(_) if !on_it => self.mode = Mode::Normal,
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => self.mode = Mode::Normal,
                _ => return Ok(()),
            }
        }
        // A click on a question's choice in the pane takes it.
        if m.kind == MouseEventKind::Down(MouseButton::Left) && matches!(self.mode, Mode::Normal | Mode::Answer(_)) {
            let hit = {
                let h = self.pane_hit.borrow();
                h.choice(m.column, m.row).map(|c| (h.question.clone(), c))
            };
            if let Some((qid, at)) = hit
                && let Some(q) = self.digest.decisions.iter().find(|d| d.id == qid).cloned()
            {
                let all = pane::choices(&q, q.run.as_deref().and_then(|r| self.run_by_id(r)));
                if let Some(c) = all.get(at).cloned() {
                    self.mode = Mode::Normal;
                    self.answer_at = at;
                    self.take_choice(&q, c, area).await;
                }
                return Ok(());
            }
        }
        if !matches!(self.mode, Mode::Normal) || !self.hit.borrow().has(m.column, m.row) {
            return Ok(());
        }
        if self.view == View::Events {
            self.events_mouse(m).await;
            return Ok(());
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.step(1),
            MouseEventKind::ScrollUp => self.step(-1),
            MouseEventKind::Moved => {
                let key = hit_row(&self.hit.borrow(), m.column, m.row);
                if let Some(key) = key
                    && self.sel.as_deref() != Some(key.as_str())
                {
                    self.select(Some(key));
                }
                return Ok(());
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Bound first: the borrow has to be done with before `select`.
                let key = hit_row(&self.hit.borrow(), m.column, m.row);
                match key {
                    Some(key) => self.select(Some(key)),
                    None => return Ok(()),
                }
            }
            _ => return Ok(()),
        }
        self.moved().await;
        Ok(())
    }

    /// A paste, whole: into the line as it came, newlines and all, over a
    /// name being changed as one line, or a form's field a character at a
    /// time.
    pub(super) async fn paste(&mut self, s: String, area: Rect) -> Result<()> {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        if let Mode::Rename(r) = &mut self.mode {
            r.field.paste(&s);
            return Ok(());
        }
        if let Mode::Reply(r) = &mut self.mode {
            r.field.paste(&s);
            return Ok(());
        }
        if matches!(self.mode, Mode::Normal) {
            self.line.insert(&s);
            self.completing = None;
            return Ok(());
        }
        for c in s.chars().filter(|c| !c.is_control()) {
            self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), area)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn key(&mut self, k: KeyEvent, area: Rect) -> Result<()> {
        let done = self.key_in(k, area).await;
        // Leaving a subpage is not async; what it looked at is reviewed here.
        self.flush_reviews().await;
        done
    }

    async fn key_in(&mut self, mut k: KeyEvent, area: Rect) -> Result<()> {
        self.flash = None;
        // Like the flash, the list of what a reload did not load lasts one key.
        self.reload_list = None;
        self.tested = None;
        // A destructive key stays armed only until the very next key: its own
        // second press acts, anything else disarms it. (ctrl-x/ctrl-c re-check
        // and re-arm themselves in `key_list`.)
        if !matches!(
            (k.code, k.modifiers),
            (KeyCode::Char('c') | KeyCode::Char('x'), KeyModifiers::CONTROL)
        ) {
            self.arm = None;
        }
        // A form or a `y`-or-not question has its keys in the footer; the
        // shortcuts go, rather than come back when it is done. A name typed
        // in the list takes no room from them, so they stay for it.
        if !matches!(self.mode, Mode::Normal | Mode::Answer(_) | Mode::Rename(_) | Mode::Walk) {
            self.shortcuts = false;
        }
        // `ctrl-\` leaves any subpage to the list in one shot, from inside a sub-mode too, and
        // the overview, page or not.
        if (self.show_pane || self.view == View::Overview)
            && matches!(self.mode, Mode::Normal | Mode::Answer(_) | Mode::Walk | Mode::Rename(_))
            && matches!(k.code, KeyCode::Char('\\' | '4'))
            && k.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.leave_subpage();
            // From a flow's page in the overview, past the overview too.
            if self.view == View::Overview {
                self.back_to_list().await;
            }
            return Ok(());
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Stop(id) => {
                if k.code == KeyCode::Char('y') {
                    self.stop(id, false).await;
                }
                return Ok(());
            }
            Mode::Take(id, _) => {
                if k.code == KeyCode::Char('y') {
                    self.attach = Some((id, true));
                }
                return Ok(());
            }
            Mode::Dismiss(id) => {
                if k.code == KeyCode::Char('y') {
                    self.dismiss(id).await;
                }
                return Ok(());
            }
            Mode::SummaryCost { .. } => {
                if let Some(p) = self.summary_ask.take()
                    && k.code == KeyCode::Char('y')
                {
                    self.summary_start(p);
                }
                return Ok(());
            }
            Mode::Mark(id) => {
                let mark = match k.code {
                    KeyCode::Char('d') => Some(Mark::Done),
                    KeyCode::Char('b') => Some(Mark::Blocked),
                    KeyCode::Char('w') => Some(Mark::Working),
                    _ => None,
                };
                if let Some(mark) = mark {
                    self.mark(id, mark).await;
                }
                return Ok(());
            }
            Mode::Answer(id) => {
                let Some(q) = self.digest.decisions.iter().find(|d| d.id == id).cloned() else {
                    return Ok(());
                };
                let all = pane::choices(&q, q.run.as_deref().and_then(|r| self.run_by_id(r)));
                let last = all.len().saturating_sub(1);
                // Nothing is typed here, so `?` is the shortcuts' as `^h` is,
                // and `esc` puts them away before it puts the question back.
                if self.shortcut_key(&k, true) {
                    self.mode = Mode::Answer(id);
                    return Ok(());
                }
                match k.code {
                    KeyCode::Esc => self.picked.clear(),
                    KeyCode::Up => {
                        self.answer_at = self.answer_at.saturating_sub(1);
                        self.mode = Mode::Answer(id);
                    }
                    KeyCode::Down => {
                        self.answer_at = (self.answer_at + 1).min(last);
                        self.mode = Mode::Answer(id);
                    }
                    // An option by its number: the options come first.
                    KeyCode::Char(c @ '1'..='9') if ((c as u8 - b'1') as usize) < q.options.len() => {
                        self.answer_at = (c as u8 - b'1') as usize;
                        self.take_choice(&q, all[self.answer_at].clone(), area).await;
                    }
                    KeyCode::Char(' ') if q.multi => {
                        if let Some(pane::Choice::Option(i)) = all.get(self.answer_at) {
                            self.toggle_pick(*i);
                        }
                        self.mode = Mode::Answer(id);
                    }
                    KeyCode::Enter => match all.get(self.answer_at).cloned() {
                        Some(c) => self.take_choice(&q, c, area).await,
                        None => self.mode = Mode::Answer(id),
                    },
                    _ => self.mode = Mode::Answer(id),
                }
                return Ok(());
            }
            Mode::Walk => {
                if self.shortcut_key(&k, true) {
                    self.mode = Mode::Walk;
                    return Ok(());
                }
                let picked = self.flow_pane.picked.clone();
                match self.flow_pane.key(k.code) {
                    firings::Walked::On => self.mode = Mode::Walk,
                    firings::Walked::Done => {}
                    firings::Walked::Join(run) => self.join_step(run, area).await,
                    firings::Walked::NoRun(step) => {
                        self.say(format!("{step} has not run in this firing"));
                        self.mode = Mode::Walk;
                    }
                }
                // Another firing shown: its files, and what was said of it.
                if self.flow_pane.picked != picked {
                    self.load_pane().await;
                }
                return Ok(());
            }
            Mode::Form(mut f) => {
                match f.key(&k) {
                    Act::Typing => self.mode = Mode::Form(f),
                    Act::Cancel => {}
                    Act::Save => self.save_form(f, area).await,
                }
                return Ok(());
            }
            Mode::Draft(d) => {
                self.draft_key(d, k.code).await;
                return Ok(());
            }
            // `^h` and `esc` are the shortcuts' first, and the name stays; `?`
            // is a character of it.
            Mode::Rename(r) if self.shortcut_key(&k, false) => {
                self.mode = Mode::Rename(r);
                return Ok(());
            }
            Mode::Rename(mut r) => match r.field.key(&k) {
                line::Act::Typing => {
                    self.mode = Mode::Rename(r);
                    return Ok(());
                }
                line::Act::Submit => {
                    self.save_rename(r).await;
                    return Ok(());
                }
                line::Act::Cancel => return Ok(()),
                // What the name does not take lets it go, and then does what
                // it does on the list: `^j`/`^k` move, whatever is typed on
                // the line below.
                line::Act::Complete | line::Act::Nothing => {
                    k = vi_step(k, true);
                }
            },
            // Removing a project: only the exact word removes it; a key the
            // field ignores (a stray `^x` among them) keeps the prompt up, and
            // `esc` keeps the project.
            Mode::Confirm(mut c) => {
                match c.field.key(&k) {
                    line::Act::Submit => {
                        if c.field.text() == "DELETE" {
                            self.remove_project(c.workspace, c.name);
                        } else {
                            self.say(format!("type DELETE to remove {} · esc keeps it", c.name));
                            self.mode = Mode::Confirm(c);
                        }
                    }
                    line::Act::Cancel => self.say(format!("{} kept", c.name)),
                    _ => self.mode = Mode::Confirm(c),
                }
                return Ok(());
            }
            // The tag for a report row's source: `enter` saves (empty clears), `esc` lets it go.
            Mode::Tag(mut t) => {
                match t.field.key(&k) {
                    line::Act::Submit => {
                        // The source captured when the prompt opened: a refresh may have moved
                        // the cursor to another row since.
                        self.tag_source(t.source.clone(), t.field.text()).await;
                    }
                    line::Act::Cancel => {}
                    _ => self.mode = Mode::Tag(t),
                }
                return Ok(());
            }
            // A reply to a report item: `enter` sends it to the session, `esc` lets it go.
            Mode::Reply(mut r) => {
                match r.field.key(&k) {
                    line::Act::Submit => self.send_reply(r.run.clone(), r.field.text()).await,
                    line::Act::Cancel => {}
                    _ => self.mode = Mode::Reply(r),
                }
                return Ok(());
            }
            Mode::Normal => {}
        }
        if (k.code, k.modifiers) == (KeyCode::Char('f'), KeyModifiers::CONTROL) {
            match self.search.take() {
                Some(_) => {}
                None => self.open_search(),
            }
            return Ok(());
        }
        if self.search.is_some() {
            return self.key_search(k).await;
        }
        if self.shortcut_key(&k, self.line.is_empty()) {
            return Ok(());
        }
        let k = vi_step(k, self.line.is_empty());
        // On an empty line the list's own letters flip the toggles — `a`
        // agents, `s` shells, `h` hidden — taken here, before the
        // line editor would eat the character.
        if self.line.is_empty()
            && self.view == View::Work
            && matches!(self.mode, Mode::Normal)
            && !self.show_pane
            && k.modifiers.is_empty()
        {
            match k.code {
                KeyCode::Char('a') => {
                    self.toggle_agents();
                    return Ok(());
                }
                KeyCode::Char('s') => {
                    self.toggle_shells();
                    return Ok(());
                }
                KeyCode::Char('h') => {
                    self.toggle_hidden();
                    return Ok(());
                }
                _ => {}
            }
        }
        // On the report, with nothing typed, the inbox's letters: `j`/`k` move, `r` replies,
        // `x` marks reviewed, `p` reads deeply, `t` tags, and `w` flips to the written summary.
        if self.line.is_empty() && self.view == View::Report && !self.show_pane && k.modifiers.is_empty() {
            match k.code {
                KeyCode::Char('w') => {
                    self.report_written = !self.report_written;
                    return Ok(());
                }
                // The inbox is not on screen in prose: its letters leave it be.
                KeyCode::Char('j' | 'k' | 'r' | 'x' | 'p' | 't') if self.report_written => return Ok(()),
                KeyCode::Char('j') => {
                    self.report_step(1).await;
                    return Ok(());
                }
                KeyCode::Char('k') => {
                    self.report_step(-1).await;
                    return Ok(());
                }
                KeyCode::Char('r') => {
                    self.reply_prompt();
                    return Ok(());
                }
                KeyCode::Char('x') => {
                    self.review_selected().await;
                    return Ok(());
                }
                KeyCode::Char('p') => {
                    self.deep_read_selected().await;
                    return Ok(());
                }
                KeyCode::Char('t') => {
                    self.tag_prompt();
                    return Ok(());
                }
                _ => {}
            }
        }
        // On a flow's row in the overview, with nothing typed, → opens it to its latest firing's
        // steps and ← folds it again.
        if self.view == View::Overview
            && self.line.is_empty()
            && matches!(self.mode, Mode::Normal)
            && !self.show_pane
            && matches!(k.code, KeyCode::Left | KeyCode::Right)
            && let Some(key) = self.selected_flow().map(|f| f.key.clone())
        {
            if k.code == KeyCode::Right {
                self.open_flows.insert(key);
            } else {
                self.open_flows.remove(&key);
            }
            return Ok(());
        }
        // Candidates on offer: the arrows walk them, not the list.
        if self.cands().is_some() && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            self.walk_cands(if k.code == KeyCode::Down { 1 } else { -1 });
            return Ok(());
        }
        // `^e` on an empty line with a proposal up: it, as the run form.
        if k.code == KeyCode::Char('e')
            && k.modifiers == KeyModifiers::CONTROL
            && self.line.is_empty()
            && self.proposal_shown().is_some()
        {
            self.edit_proposal();
            return Ok(());
        }
        // `^e` on an empty line on a flow's row in the overview: its `flow.md`, as `/flow edit`.
        if k.code == KeyCode::Char('e')
            && k.modifiers == KeyModifiers::CONTROL
            && self.line.is_empty()
            && self.view == View::Overview
            && let Some(name) = self.flow_here()
        {
            self.edit_flow(&name);
            return Ok(());
        }
        // `shift-tab` on something typed: it is the other thing from what it reads as.
        if k.code == KeyCode::BackTab && !self.line.is_empty() {
            if self.line.text().trim_start().starts_with('/') {
                self.say("a command is a command");
            } else {
                self.flipped = !self.flipped;
            }
            return Ok(());
        }
        // The line takes the keyboard first; what it leaves, the list has.
        match self.line.key(&k) {
            line::Act::Typing => {
                self.completing = None;
                self.pick = None;
                if self.line.is_empty() {
                    self.flipped = false;
                }
                Ok(())
            }
            // With a candidate highlighted, tab and enter take it; otherwise
            // tab completes as ever and enter submits.
            line::Act::Complete => {
                if self.pick.is_some() {
                    self.accept_cand();
                } else {
                    self.tab();
                }
                Ok(())
            }
            line::Act::Submit if self.pick.is_some() => {
                self.accept_cand();
                Ok(())
            }
            line::Act::Submit => self.submit(area).await,
            line::Act::Cancel if !self.line.is_empty() => {
                self.line.clear();
                self.flipped = false;
                self.completing = None;
                self.pick = None;
                Ok(())
            }
            // A subpage up owns the keyboard: the list is not on screen, so
            // nothing here moves it.
            _ if self.show_pane => self.key_subpage(k, area).await,
            _ => self.key_list(k).await,
        }
    }

    /// A key while a subpage is up, past the line and the sub-modes (an
    /// answer, a walk), which have taken theirs. The subpage owns the keyboard: output
    /// scrolls, `esc` leaves (`ctrl-\` leaves from anywhere, `key` has it), and `^c`, `^x`,
    /// `^r`, `^o` and `^t` are the list's as ever. Everything else — the arrows, a reorder —
    /// would move a list nobody can see, so it does nothing here.
    pub(super) async fn key_subpage(&mut self, k: KeyEvent, area: Rect) -> Result<()> {
        // A page is the subpage's own height, not the screen's.
        if self.page_key(&k, self.pane_size(area).1) {
            return Ok(());
        }
        match (k.code, k.modifiers) {
            (KeyCode::Esc, _) => self.leave_subpage(),
            (KeyCode::Char('c' | 'x' | 'r' | 'o' | 't'), KeyModifiers::CONTROL) => return self.key_list(k).await,
            _ => {}
        }
        Ok(())
    }

    /// `^h`, `?` and `esc`, for the shortcuts, before anything else has them:
    /// whether they were its.
    pub(super) fn shortcut_key(&mut self, k: &KeyEvent, line_empty: bool) -> bool {
        match shortcuts::take(self.shortcuts, line_empty, k) {
            shortcuts::Take::Toggle => self.shortcuts = !self.shortcuts,
            shortcuts::Take::Close => self.shortcuts = false,
            shortcuts::Take::Pass => return false,
        }
        true
    }

    /// `^f` or `/filter`: search, starting from the filter already in force.
    pub(super) fn open_search(&mut self) {
        // What is typed now narrows the list: the shortcuts would only be in
        // the way.
        self.shortcuts = false;
        let mut l = line::Line::default();
        l.set_with_cursor(self.filter.clone(), self.filter.len());
        self.search = Some(Search {
            line: l,
            before: self.filter.clone(),
        });
        self.completing = None;
        self.pick = None;
    }

    /// A key while searching: typing narrows at once and puts the cursor on
    /// the best match, `enter` keeps the search and gives the line back, `esc`
    /// puts back the filter from before, `tab` completes a token or else goes
    /// to the next match; the arrows still move through what is left.
    pub(super) async fn key_search(&mut self, k: KeyEvent) -> Result<()> {
        let Some(s) = self.search.as_mut() else {
            return Ok(());
        };
        // A search is one line: what would add a line keeps it instead.
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Enter || (ctrl && k.code == KeyCode::Char('j')) {
            self.search = None;
            return Ok(());
        }
        if k.code == KeyCode::BackTab {
            self.next_match(true);
            return Ok(());
        }
        match s.line.key(&k) {
            line::Act::Typing => {
                let now = s.line.text().trim().to_string();
                if now != self.filter {
                    self.filter = now;
                    self.land();
                }
            }
            line::Act::Cancel => {
                if let Some(s) = self.search.take() {
                    self.filter = s.before;
                    self.settle();
                }
            }
            line::Act::Complete => self.complete_search(),
            line::Act::Submit => self.search = None,
            line::Act::Nothing => return self.key_list(k).await,
        }
        Ok(())
    }

    /// What a search token's values can be: the projects (and `global`), the
    /// flows, the branches — for `tab` in a search and after `/filter`.
    pub(super) fn search_values(&self) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        let mut projects: Vec<String> = self.names.paths().iter().map(|p| self.names.workspace(p)).collect();
        projects.push("global".into());
        let flows: Vec<String> = self.flows.iter().map(|f| f.name.clone()).collect();
        let branches: Vec<String> = (self.digest.checkouts.iter().filter_map(|c| c.branch.clone()))
            .chain(self.digest.items.iter().filter_map(|i| i.run.branch.clone()))
            .collect();
        // The events view's `kind:` is an event's; elsewhere, the list's own.
        let kinds = match self.view {
            View::Events => self.feed.kinds(),
            _ => rows::list_kinds(),
        };
        (projects, flows, branches, kinds)
    }

    /// `tab` in a search: the token names, then a token's values — states,
    /// kinds, projects, flows. One fits: taken. Several: what they share, and
    /// the rest shown in the footer. None — a plain word, or no word at all
    /// after one — and it goes to the next match instead.
    pub(super) fn complete_search(&mut self) {
        let (projects, flows, branches, kinds) = self.search_values();
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let (text, cur) = (s.line.text().to_string(), s.line.cursor());
        let start = text[..cur].rfind(' ').map_or(0, |i| i + 1);
        let word = &text[start..cur];
        let cands = query::complete(word, &projects, &flows, &branches, &kinds);
        // A plain word has nothing to complete, and nor has the gap after one:
        // tab goes on to the next match. With nothing to match yet, the gap
        // still offers the names.
        if (cands.is_empty() || word.is_empty()) && self.next_match(false) {
            return;
        }
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let take = match cands.as_slice() {
            [] => return,
            [one] if one.ends_with(':') => one.clone(),
            [one] => format!("{one} "),
            many => {
                let mut prefix = many[0].clone();
                for c in &many[1..] {
                    while !c.starts_with(&prefix) {
                        prefix.pop();
                    }
                }
                if prefix.len() <= word.len() {
                    self.flash = Some((many.join(" · "), Instant::now()));
                    return;
                }
                prefix
            }
        };
        let line = format!("{}{take}{}", &text[..start], &text[cur..]);
        s.line.set_with_cursor(line, start + take.len());
        self.filter = s.line.text().trim().to_string();
        self.land();
    }

    /// What the line does not take: moving about, and backing out.
    pub(super) async fn key_list(&mut self, k: KeyEvent) -> Result<()> {
        // A destructive key is armed by its first press and acts on its second;
        // any other key here disarms it, so `ctrl-x`/`ctrl-c` never fire on one
        // slip (the way an agent view asks before it deletes).
        let armed = self.arm.take();
        if let Some(up) = reorder_step(&k) {
            self.reorder(up).await;
            return Ok(());
        }
        if self.view == View::Report && !self.report_written {
            let page = self.list_h.get().saturating_sub(2).max(1) as isize;
            let by = match k.code {
                KeyCode::Down => Some(1),
                KeyCode::Up => Some(-1),
                KeyCode::PageDown => Some(page),
                KeyCode::PageUp => Some(-page),
                KeyCode::Home => Some(isize::MIN / 2),
                KeyCode::End => Some(isize::MAX / 2),
                _ => None,
            };
            if let Some(by) = by {
                self.report_step(by).await;
                return Ok(());
            }
        }
        if self.view == View::Events && self.events_step(k.code).await {
            return Ok(());
        }
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if armed == Some(Arm::Quit) {
                    self.quit = true;
                } else {
                    self.arm = Some(Arm::Quit);
                    self.flash = Some(("press ^c again to quit".into(), Instant::now()));
                }
            }
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                if self.flow_in_catch_up() {
                    self.say(Self::FLOW_ON_THE_LIST);
                } else if !self.on_everywhere() && matches!(self.selected(), Some(Row::Group(_))) {
                    // A project is not armed and fired by a second keypress: a
                    // stray or laggy ^x must not take a whole project down.
                    self.confirm_remove_project();
                } else if armed == Some(Arm::Remove) {
                    self.remove_selected().await;
                } else if let Some(what) = self.remove_hint() {
                    self.arm = Some(Arm::Remove);
                    self.flash = Some((format!("press ^x again to {what}"), Instant::now()));
                } else if self.on_everywhere() {
                    self.say(format!("{} · nothing to remove", Self::NOT_A_PROJECT));
                } else {
                    self.say("nothing here to remove");
                }
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => self.rename_here(),
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => self.toggle_pane(),
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => self.toggle_pin().await,
            (KeyCode::Esc, _) => {
                // A subpage's `esc` is `key_subpage`'s; here it is the list's.
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.settle();
                } else if self.view == View::Events {
                    self.toggle_events().await;
                } else if self.view == View::Report && !self.report_written && self.report_back() {
                    // A step back within the inbox.
                } else if self.view == View::CatchUp || self.view == View::Report {
                    self.back_to_list().await;
                } else if self.view == View::Overview {
                    self.leave_overview().await;
                }
            }
            (KeyCode::Down, _) => {
                self.step(1);
                self.moved().await;
            }
            (KeyCode::Up, _) => {
                self.step(-1);
                self.moved().await;
            }
            (KeyCode::Home, _) => {
                self.step(isize::MIN / 2);
                self.moved().await;
            }
            (KeyCode::End, _) => {
                self.step(isize::MAX / 2);
                self.moved().await;
            }
            (KeyCode::PageDown, _) => {
                self.step(self.list_h.get().saturating_sub(2).max(1) as isize);
                self.moved().await;
            }
            (KeyCode::PageUp, _) => {
                self.step(-(self.list_h.get().saturating_sub(2).max(1) as isize));
                self.moved().await;
            }
            _ => {}
        }
        Ok(())
    }

    /// `enter` on an empty line: whatever the row under the cursor is for.
    pub(super) async fn enter_row(&mut self, area: Rect) -> Result<()> {
        // A subpage up has the keyboard: `enter` there is the second step, acting on the row it
        // opened on, never the list's.
        if self.show_pane {
            self.act_in_subpage(area).await;
            return Ok(());
        }
        if self.view == View::Events {
            self.events_enter().await;
            return Ok(());
        }
        if self.view == View::CatchUp {
            self.catch_up_enter().await;
            return Ok(());
        }
        if self.view == View::Report {
            if !self.report_written {
                self.report_enter(area.width >= report::INBOX_SPLIT_W).await;
            }
            return Ok(());
        }
        if self.on_new_flow() {
            // A new flow is drafted from the line, as `/flow new` does.
            let draft = "/flow new ";
            self.line.set_with_cursor(draft, draft.len());
            return Ok(());
        }
        if self.selected_run().is_some() {
            self.open_run(area).await;
            return Ok(());
        }
        if self.selected_flow().is_some() {
            // A flow's row in the overview never grows into its runs: they are in its page, and
            // enter again walks its steps and its last firings there.
            self.pane = Pane::Facts;
            self.show_pane = true;
            self.load_pane().await;
        } else if let Some(q) = self.selected_question() {
            let id = q.id.clone();
            self.picked.clear();
            // The choices are in the subpage: the arrows walk them from the first.
            self.answer_at = 0;
            self.pane = Pane::Facts;
            self.show_pane = true;
            self.mode = Mode::Answer(id);
        } else if let Some(Row::Outside(s)) = self.selected() {
            // An agent's own session: enter attaches to it straight away — hands over the
            // terminal, carries it on, or goes to its run — the way `/join` does, without the
            // pane first. The pane is still a `^o`/`/pane` away.
            let s = s.clone();
            self.enter_outside(&s, area).await;
        } else if let Some(Row::Group(h)) = self.selected() {
            // A project's line: its overview, its flows under what was said of it; `everywhere`'s,
            // the flows with no `in`.
            let scope = if h.is_everywhere() {
                overview::Scope::Everywhere
            } else {
                overview::Scope::Project(h.workspace.clone())
            };
            self.open_overview(scope, None);
            self.fetch_overview_top().await;
            // Not `moved`: opening the overview alone does not mark the flow it lands on seen.
            self.load_pane().await;
        } else {
            self.say("nothing here to open");
        }
        Ok(())
    }

    /// `enter` with a subpage up: a flow's goes on to walk its steps and its last firings, a
    /// question's to its choices, an agent's own session's does what its subpage says. The rest
    /// have nothing more to do.
    pub(super) async fn act_in_subpage(&mut self, area: Rect) {
        if self.pane != Pane::Facts {
            return;
        }
        if let Some(f) = self.selected_flow() {
            if f.name == self.flow_pane.flow && !self.flow_pane.firings.is_empty() {
                self.flow_pane.at = 0;
                self.mode = Mode::Walk;
            }
        } else if let Some(q) = self.selected_question() {
            let id = q.id.clone();
            self.answer_at = 0;
            self.mode = Mode::Answer(id);
        } else if let Some(Row::Outside(s)) = self.selected() {
            let s = s.clone();
            self.enter_outside(&s, area).await;
        }
    }

    /// Where the line is being typed: the folder of the row under the cursor,
    /// what agents there are, and $PATH.
    pub(super) fn here<'a>(&'a self, folder: &'a Path) -> complete::Here<'a> {
        complete::Here {
            agents: &self.agents,
            path_dirs: &self.path_dirs,
            aliases: &self.aliases,
            ..complete::Here::at(folder)
        }
    }

    /// The folder the line at the foot will run in, for completing it: the
    /// warm shell's own folder when the line goes there and the shell has
    /// said where it is, else the folder here (`complete::line_folder`).
    pub(super) fn line_folder(&self, text: &str) -> PathBuf {
        let folder = self.folder_here();
        let shell = self.selected_shell().or_else(|| self.project_shell(&folder));
        let cwd = shell.and_then(|id| {
            self.digest
                .items
                .iter()
                .find(|i| i.run.id == id)
                .and_then(|i| i.cwd.clone())
        });
        complete::line_folder(text, &self.agents, &folder, cwd.as_deref())
    }

    /// What the line at the foot means: as it reads, or the other way round once `shift-tab` has
    /// said so (`flipped`).
    pub(super) fn line_what(&self) -> line::What {
        // Runnable where it will run: a `./build.sh` in the folder the
        // warm shell has `cd`'d into is a shell line.
        let at = self.line_folder(self.line.text());
        let here = self.here(&at);
        let mut what = line::what(self.line.text(), &self.agents, &|h| complete::runnable(h, &here));
        // With a proposal up, words change it: a first word that runs is still one of them,
        // unless a bang says otherwise.
        if self.proposal_shown().is_some()
            && !self.line.text().trim_start().starts_with('!')
            && matches!(what, line::What::Shell(_) | line::What::Agent { .. })
        {
            what = line::What::Ask(self.line.text().trim().to_string());
        }
        if self.flipped {
            line::flip(what, self.line.text())
        } else {
            what
        }
    }

    /// `enter` on the line: what it says. Nothing typed, the row instead.
    pub(super) async fn submit(&mut self, area: Rect) -> Result<()> {
        self.completing = None;
        // On a question, a line that starts with an option answers it, and
        // the rest of the line is the note that goes with the answer.
        if let Some(q) = self.question_here()
            && let Some((keys, note)) = pane::answer_of(q, self.line.text())
        {
            let id = q.id.clone();
            self.line.clear();
            self.answer(id, keys, note).await;
            return Ok(());
        }
        let folder = self.folder_here();
        let what = self.line_what();
        self.flipped = false;
        match what {
            line::What::Empty if self.proposal_shown().is_some() => self.go_proposal(area).await,
            line::What::Empty => return self.enter_row(area).await,
            line::What::Slash(rest) => return self.command(&rest, area).await,
            line::What::Shell(cmd) => {
                self.line.clear();
                // A shell line runs in the project's warm shell — one persistent
                // `$SHELL -i` per folder, rc read once — not a fresh run each time.
                if let Err(e) = self.run_in_shell(cmd, folder, area).await {
                    self.say(format!("{e:#}"));
                }
            }
            line::What::Agent { agent, prompt } => {
                self.line.clear();
                let mut argv = vec![agent];
                argv.extend(prompt);
                let d = form::RunDraft {
                    cwd: folder,
                    kind: RunKind::Agent,
                    argv,
                    title: None,
                };
                if let Err(e) = self.start_run(d, area).await {
                    self.say(format!("{e:#}"));
                }
            }
            line::What::Ask(words) => {
                if self.asking.is_some() {
                    self.say("still asking; /stop on its run");
                } else {
                    self.line.clear();
                    self.ask(words, Want::Any);
                }
            }
        }
        Ok(())
    }

    /// `tab`: as much of the word as every candidate agrees on, then round
    /// the candidates one at a time.
    /// The candidates on offer under the line, if any: the `/` commands it
    /// names, or the words `tab` has laid out. Slash and words never both show.
    pub(super) fn cands(&self) -> Option<Cands> {
        if !matches!(self.mode, Mode::Normal) || self.search.is_some() {
            return None;
        }
        if let Some(n) = self.slash_list().map(|cs| cs.len())
            && n > 0
        {
            return Some(Cands::Slash(n));
        }
        match &self.completing {
            Some(c) if !c.cands.is_empty() => Some(Cands::Words(c.cands.len())),
            _ => None,
        }
    }

    /// The `/` commands a `/` line names, for the list under it — until `tab`
    /// lays out words for a command's argument, which show instead.
    pub(super) fn slash_list(&self) -> Option<Vec<&'static palette::Command>> {
        if !matches!(self.mode, Mode::Normal) {
            return None;
        }
        let rest = self.line.text().strip_prefix('/')?;
        let args = rest.trim_start().contains(char::is_whitespace);
        if args && self.completing.as_ref().is_some_and(|c| !c.cands.is_empty()) {
            return None;
        }
        Some(palette::matching(rest))
    }

    /// Walk the highlight through the candidates, wrapping. From nothing, down
    /// lands on the first, up on the last.
    pub(super) fn walk_cands(&mut self, by: isize) {
        let Some(n) = self.cands().map(|c| c.len() as isize) else {
            return;
        };
        let next = match self.pick {
            Some(i) => (i as isize + by).rem_euclid(n),
            None if by > 0 => 0,
            None => n - 1,
        };
        self.pick = Some(next as usize);
    }

    /// Take the highlighted candidate into the line: a `/` command filled in
    /// with its name (its arguments left for you to type — it is not run), or
    /// the completion word applied where the cursor was.
    pub(super) fn accept_cand(&mut self) {
        let Some(cs) = self.cands() else {
            self.pick = None;
            return;
        };
        let i = self.pick.unwrap_or(0).min(cs.len().saturating_sub(1));
        match cs {
            Cands::Slash(_) => {
                let rest = self.line.text().strip_prefix('/').unwrap_or("").to_string();
                if let Some(c) = palette::matching(&rest).get(i) {
                    let taken = format!("/{} ", c.name);
                    let at = taken.len();
                    self.line.set_with_cursor(taken, at);
                }
            }
            Cands::Words(_) => {
                if let Some(c) = self.completing.take()
                    && let Some(cand) = c.cands.get(i)
                {
                    let (l, pos) = complete::apply(&c.line, c.cursor, &cand.text);
                    self.line.set_with_cursor(l.clone(), pos);
                    self.completing = Some(Completing { shown: l, at: i, ..c });
                }
            }
        }
        self.pick = None;
    }

    pub(super) fn tab(&mut self) {
        let text = self.line.text().to_string();
        // A `/` line still on its name completes to the command it names; past
        // the name, its arguments complete like any other word, below.
        if let Some(rest) = text.strip_prefix('/')
            && !rest.contains(char::is_whitespace)
        {
            if let Some(c) = palette::matching(rest).first() {
                let taken = format!("/{} ", c.name);
                let at = taken.len();
                self.line.set_with_cursor(taken, at);
            }
            self.completing = None;
            return;
        }
        // Tab again on what tab last wrote: the next one. With only the one,
        // there is nothing to walk: tab starts again from what it wrote, so a
        // folder it filled in (`src/`) opens on the next press.
        if let Some(c) = self.completing.take()
            && c.shown == text
            && c.cands.len() > 1
        {
            let at = (c.at + 1) % c.cands.len();
            let (l, pos) = complete::apply(&c.line, c.cursor, &c.cands[at].text);
            self.line.set_with_cursor(l.clone(), pos);
            self.completing = Some(Completing { shown: l, at, ..c });
            return;
        }
        let cur = self.line.cursor();
        let folder = self.line_folder(&text);
        let (projects, flows, branches, kinds) = self.search_values();
        let cands = {
            let here = complete::Here {
                names: complete::Names {
                    projects: &projects,
                    flows: &flows,
                    branches: &branches,
                    kinds: &kinds,
                },
                ..self.here(&folder)
            };
            complete::candidates(&text, cur, &here)
        };
        if cands.is_empty() {
            return;
        }
        // What they all share goes in first, and the next tab starts the list.
        let (take, at) = match complete::common_prefix(&cands) {
            Some(p) => (p, cands.len() - 1),
            None => (cands[0].text.clone(), 0),
        };
        let (l, pos) = complete::apply(&text, cur, &take);
        self.line.set_with_cursor(l.clone(), pos);
        self.completing = Some(Completing {
            line: text,
            cursor: cur,
            shown: l,
            cands,
            at,
        });
    }

    /// A `/` command: the panel's own doing, by name.
    pub(super) async fn command(&mut self, rest: &str, area: Rect) -> Result<()> {
        let Some((c, arg)) = palette::parse(rest) else {
            self.say(format!("no such command: /{rest} · /help lists them"));
            return Ok(());
        };
        self.line.clear();
        match c.cmd {
            palette::Cmd::Quit => self.quit = true,
            palette::Cmd::Up => self.add_dir(&arg).await?,
            palette::Cmd::Help => {
                let names: Vec<&str> = palette::ALL.iter().map(|c| c.name).collect();
                self.say(format!("/{}", names.join(" · /")));
                // The commands in the footer, and the keys under them: what
                // `^h` shows.
                self.shortcuts = true;
            }
            palette::Cmd::Stop => {
                if let Some(i) = self.selected_run()
                    && !i.run.state.is_terminal()
                {
                    self.mode = Mode::Stop(i.run.id.clone());
                } else if let Some(q) = self.selected_question() {
                    self.mode = Mode::Dismiss(q.id.clone());
                } else {
                    self.say("nothing here to stop");
                }
            }
            palette::Cmd::Archive => match arg.trim() {
                "seen" | "all" => self.archive_seen().await,
                "" => {
                    if self.flow_in_catch_up() {
                        self.say(Self::FLOW_ON_THE_LIST);
                    } else if let Some(f) = self.selected_flow() {
                        let ids: Vec<String> = f.runs.iter().map(|i| i.run.id.clone()).collect();
                        self.archive_flow(&f.name, ids).await;
                    } else if let Some(i) = self.selected_run() {
                        if i.run.state.is_terminal() {
                            let (id, title) = (i.run.id.clone(), i.run.title.clone());
                            self.archive(vec![id], &title).await;
                        } else {
                            self.say("still going: /stop first");
                        }
                    } else {
                        self.say("nothing here to archive · /archive seen for everything you have seen");
                    }
                }
                other => self.say(format!("archive {other}? on its own, or /archive seen")),
            },
            palette::Cmd::Mark => {
                let Some(i) = self.selected_run().filter(|i| !i.run.state.is_terminal()) else {
                    self.say("nothing here that is still going");
                    return Ok(());
                };
                let id = i.run.id.clone();
                match arg.trim() {
                    "" => self.mode = Mode::Mark(id),
                    "done" => self.mark(id, Mark::Done).await,
                    "blocked" => self.mark(id, Mark::Blocked).await,
                    "working" => self.mark(id, Mark::Working).await,
                    other => self.say(format!("mark {other}? done, blocked or working")),
                }
            }
            palette::Cmd::Resume => match arg.trim() {
                "" => match self.selected_run() {
                    Some(i) if i.run.state.is_terminal() => {
                        let ids = vec![i.run.id.clone()];
                        self.resume(
                            RunResumeParams {
                                ids,
                                ..Default::default()
                            },
                            area,
                        )
                        .await;
                    }
                    Some(_) => self.say("still going: enter joins it"),
                    None => self.say("nothing here to resume"),
                },
                "all" => {
                    self.resume(
                        RunResumeParams {
                            lost: true,
                            ..Default::default()
                        },
                        area,
                    )
                    .await
                }
                "all commands" => {
                    let p = RunResumeParams {
                        lost: true,
                        commands: true,
                        ..Default::default()
                    };
                    self.resume(p, area).await
                }
                other => self.say(format!("resume {other}? on its own, all, or all commands")),
            },
            palette::Cmd::Join if matches!(self.selected(), Some(Row::Outside(_))) => {
                // An agent's own session: as enter does, without the pane first.
                if let Some(Row::Outside(s)) = self.selected() {
                    let s = s.clone();
                    self.enter_outside(&s, area).await;
                }
            }
            palette::Cmd::Join => match self.selected_run() {
                Some(i) if !i.run.state.is_terminal() => {
                    // Someone at the keyboard already: asked first, by name.
                    self.attach = Some((i.run.id.clone(), false));
                }
                Some(_) => self.say("ended: nothing to join"),
                None => self.say("nothing here to join"),
            },
            palette::Cmd::Import => self.importing = true,
            palette::Cmd::Pane => match arg.trim() {
                "" => self.toggle_pane(),
                other => self.say(format!("pane {other}? on its own")),
            },
            palette::Cmd::Rename => self.rename_here(),
            palette::Cmd::Pin => self.toggle_pin().await,
            palette::Cmd::Hide => self.hide().await,
            palette::Cmd::Sort => self.sort_project().await,
            palette::Cmd::Events => self.toggle_events().await,
            palette::Cmd::Report => self.open_report().await,
            palette::Cmd::Shell => self.new_shell(area).await,
            palette::Cmd::Reload => match arg.trim() {
                "flows" => self.reload_flows().await,
                "shell" => self.reload_shell().await,
                "" => {
                    // The shell first: what the flows say is the flash that stays.
                    self.reload_shell().await;
                    self.reload_flows().await;
                }
                other => self.say(format!("reload {other}? flows or shell, or both on its own")),
            },
            palette::Cmd::Flow => self.flow_command(&arg).await,
            palette::Cmd::Edit => {
                // The flow named, else the one under the cursor (the catch-up's
                // flow row) — `/flow edit`, opening its `flow.md` in `$EDITOR`.
                let named = arg.trim();
                if !named.is_empty() {
                    self.edit_flow(named);
                } else if let Some(f) = self.selected_flow() {
                    let name = f.name.clone();
                    self.edit_flow(&name);
                } else {
                    self.say("nothing here to edit · name a flow: /edit <flow>");
                }
            }
            palette::Cmd::New => self.new_run_form(),
            palette::Cmd::Summary => self.summary(&arg).await,
            palette::Cmd::Since => self.since(&arg).await,
            palette::Cmd::Ask => {
                if arg.trim().is_empty() {
                    if !self.show_proposal() {
                        self.say("/ask what? plain words, and the assistant drafts it");
                    }
                } else if self.asking.is_some() {
                    self.say("still asking; /stop on its run");
                } else {
                    self.ask(arg, Want::Any);
                }
            }
            palette::Cmd::Filter => {
                if arg.trim().is_empty() {
                    self.open_search();
                } else {
                    self.filter = arg.trim().to_string();
                    self.land();
                }
            }
            palette::Cmd::Agents => self.toggle_agents(),
            palette::Cmd::Shells => self.toggle_shells(),
            palette::Cmd::Hidden => self.toggle_hidden(),
            palette::Cmd::Prune => match self.rpc.call::<GcRun>(GcParams::default()).await {
                Ok(r) if r.runs == 0 => self.say(format!("nothing with a journal ended over {} ago", r.older_than)),
                Ok(r) => self.say(format!(
                    "dropped {} journal{} · {} · the runs and their events stay",
                    r.runs,
                    if r.runs == 1 { "" } else { "s" },
                    bytes_words(r.bytes)
                )),
                Err(e) => self.say(format!("prune: {e:#}")),
            },
        }
        Ok(())
    }

    /// Reading an ended run's page: the few keys the page keeps for itself.
    /// Everything else has already gone to the line.
    pub(super) fn page_key(&mut self, k: &KeyEvent, page: u16) -> bool {
        if self.term.is_none() {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let back = matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) && ctrl;
        if back || k.code == KeyCode::Esc {
            self.leave_subpage();
            return true;
        }
        let by = match k.code {
            KeyCode::PageUp => page as isize,
            KeyCode::PageDown => -(page as isize),
            KeyCode::Up => 1,
            KeyCode::Down => -1,
            KeyCode::Home => isize::MAX / 2,
            KeyCode::End => isize::MIN / 2,
            _ => return false,
        };
        if let Some(t) = &mut self.term {
            t.scroll_by(by);
        }
        true
    }
}
