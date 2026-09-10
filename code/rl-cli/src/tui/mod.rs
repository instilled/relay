//! The control panel: one list, one pane, one keystroke to anything.
//!
//! Left, everything ranked by attention — questions, then runs that need
//! you, then what is working and what finished — grouped by what it belongs
//! to. Right, the selected thing: its facts, or with `enter` its terminal,
//! watched, and with `enter` again, joined. Monochrome; colour only for
//! attention states; the footer is the only chrome (J8). Nothing on screen
//! is an id.

mod keys;
mod pane;
mod rows;
mod screen;
mod term;

use crate::attach::client_name;
use crate::client::Client;
use crate::render::state_label;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use jiff::Timestamp;
use pane::state_style;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use relay_core::Home;
use relay_core::time::fmt_age;
use relay_proto::decision::Decision;
use relay_proto::methods::{
    DecisionDismiss, DecisionNote, DecisionTake, DecisionTakeParams, Digest, DigestGet, DigestItem, DigestParams,
    EventFilter, EventList, PresenceClient, PresenceTouch, RunIds, RunMark, RunMarkParams, RunSeen, RunStop,
    RunStopParams, WorkspaceList,
};
use relay_proto::types::{Event as LedgerEvent, Mark, Run};
use rows::{Names, Row};
use std::time::{Duration, Instant};
use term::TermView;

const REFRESH: Duration = Duration::from_secs(1);
/// Footer messages fade after this.
const FLASH: Duration = Duration::from_secs(4);
/// Narrower than this, the list and the pane take turns instead of sharing.
const SPLIT_AT: u16 = 100;
/// The list's share of a split screen, in percent.
const LIST_SHARE: u16 = 42;

/// What the right pane shows for a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Facts,
    Term,
}

/// Which side a narrow screen shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Pane,
}

enum Mode {
    Normal,
    /// `x` pressed: `y` stops this run.
    Stop(String),
    /// `m` pressed: pick what to say about this run.
    Mark(String),
    /// `n` pressed: `y` dismisses this question.
    Dismiss(String),
    /// `/` pressed: typing a filter.
    Filter(String),
}

struct App {
    home: Home,
    rpc: Client,
    me: String,
    digest: Digest,
    names: Names,
    filter: String,
    /// The selected row's key (`r:<id>` or `q:<id>`); the cursor follows it
    /// across re-ranks.
    sel: Option<String>,
    pane: Pane,
    focus: Focus,
    /// The pane alone, full width.
    wide: bool,
    term: Option<TermView>,
    /// Size the terminal was last told about.
    term_size: (u16, u16),
    /// The selected run's recent events.
    events: Vec<LedgerEvent>,
    /// Options marked so far on a multi-choice question.
    picked: Vec<usize>,
    mode: Mode,
    flash: Option<(String, Instant)>,
    quit: bool,
}

/// Where things go on screen.
struct Areas {
    head: Rect,
    list: Option<Rect>,
    pane: Option<Rect>,
    footer: Rect,
}

fn areas(area: Rect, wide: bool, focus: Focus) -> Areas {
    let [head, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (list, pane) = if wide {
        (None, Some(body))
    } else if area.width < SPLIT_AT {
        match focus {
            Focus::List => (Some(body), None),
            Focus::Pane => (None, Some(body)),
        }
    } else {
        let [l, _, r] = Layout::horizontal([
            Constraint::Percentage(LIST_SHARE),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        (Some(l), Some(r))
    };
    Areas {
        head,
        list,
        pane,
        footer,
    }
}

pub async fn run(home: &Home) -> Result<()> {
    let me = client_name();
    let mut rpc = Client::connect(home).await?;
    let _ = rpc.call::<PresenceTouch>(PresenceClient { client: me.clone() }).await;
    let mut app = App {
        home: home.clone(),
        rpc,
        me,
        digest: Digest {
            since: Timestamp::now(),
            items: Vec::new(),
            decisions: Vec::new(),
        },
        names: Names::default(),
        filter: String::new(),
        sel: None,
        pane: Pane::Facts,
        focus: Focus::List,
        wide: false,
        term: None,
        term_size: (0, 0),
        events: Vec::new(),
        picked: Vec::new(),
        mode: Mode::Normal,
        flash: None,
        quit: false,
    };
    app.refresh().await;

    let mut terminal = ratatui::init();
    let result = app.main_loop(&mut terminal).await;
    ratatui::restore();
    // Leaving is looking: `rl since` starts from here next time.
    let _ = app
        .rpc
        .call::<PresenceTouch>(PresenceClient { client: app.me.clone() })
        .await;
    result
}

impl App {
    async fn main_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(REFRESH);
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            let size = self.pane_size(terminal.size()?.into());
            if self.term.is_some() && size != self.term_size {
                self.term_size = size;
                if let Some(t) = &mut self.term {
                    t.resized(size).await?;
                }
            }
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => self.key(k, size).await?,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => self.quit = true,
                },
                _ = tick.tick() => self.refresh().await,
                more = async {
                    match &mut self.term {
                        Some(t) => t.pump().await,
                        None => std::future::pending().await,
                    }
                } => match more {
                    Ok(true) => {}
                    Ok(false) => { self.say("connection closed"); self.close_term(); }
                    Err(e) => { self.say(format!("stream: {e}")); self.close_term(); }
                },
            }
        }
        Ok(())
    }

    fn say(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    /// The area the terminal draws into.
    fn pane_size(&self, size: Rect) -> (u16, u16) {
        let a = areas(size, self.wide, Focus::Pane);
        let r = a.pane.unwrap_or(size);
        (r.width.max(1), r.height.max(1))
    }

    async fn refresh(&mut self) {
        if let Ok(ws) = self.rpc.call::<WorkspaceList>(()).await {
            self.names = Names::new(ws.into_iter().map(|w| (w.path, w.name)));
        }
        match self.rpc.call::<DigestGet>(DigestParams::default()).await {
            Ok(d) => self.digest = d,
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        self.settle();
        if self.pane == Pane::Facts
            && let Some(Row::Run(i)) = self.selected()
        {
            let filter = EventFilter {
                run: Some(i.run.id.clone()),
                limit: Some(200),
                ..Default::default()
            };
            if let Ok(evs) = self.rpc.call::<EventList>(filter).await {
                self.events = evs;
            }
        }
    }

    // ---- the list ----------------------------------------------------------

    fn rows(&self) -> Vec<Row<'_>> {
        rows::build(&self.digest, &self.names, &self.filter, Timestamp::now())
    }

    /// Keys of the selectable rows, in order.
    fn keys(&self) -> Vec<String> {
        self.rows().into_iter().filter_map(|r| r.key()).collect()
    }

    /// Keeps the selection on something that exists.
    fn settle(&mut self) {
        let keys = self.keys();
        if self.sel.as_ref().is_none_or(|k| !keys.contains(k)) {
            self.select(keys.first().cloned());
        }
    }

    fn select(&mut self, key: Option<String>) {
        if key != self.sel {
            self.sel = key;
            self.picked.clear();
            self.events.clear();
            self.close_term();
        }
    }

    fn selected(&self) -> Option<Row<'_>> {
        let key = self.sel.as_ref()?;
        self.rows().into_iter().find(|r| r.key().as_ref() == Some(key))
    }

    fn selected_run(&self) -> Option<&DigestItem> {
        match self.selected() {
            Some(Row::Run(i)) => Some(i),
            _ => None,
        }
    }

    fn selected_question(&self) -> Option<&Decision> {
        match self.selected() {
            Some(Row::Question(d)) => Some(d),
            _ => None,
        }
    }

    fn run_by_id(&self, id: &str) -> Option<&Run> {
        self.digest.items.iter().map(|i| &i.run).find(|r| r.id == id)
    }

    fn step(&mut self, by: isize) {
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

    fn close_term(&mut self) {
        self.term = None;
        self.pane = Pane::Facts;
    }

    async fn seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let _ = self.rpc.call::<RunSeen>(RunIds { ids }).await;
    }

    // ---- doing things ------------------------------------------------------

    async fn open_term(&mut self, size: (u16, u16)) {
        let Some(id) = self.selected_run().map(|i| i.run.id.clone()) else {
            return;
        };
        self.seen(vec![id.clone()]).await;
        match TermView::open(&self.home, &id, size).await {
            Ok(t) => {
                self.term = Some(t);
                self.term_size = size;
                self.pane = Pane::Term;
                self.focus = Focus::Pane;
            }
            Err(e) => self.say(format!("attach: {e:#}")),
        }
    }

    async fn answer(&mut self, id: String, options: Vec<String>) {
        let what = options.join(", ");
        match self
            .rpc
            .call::<DecisionTake>(DecisionTakeParams {
                id,
                options,
                note: None,
            })
            .await
        {
            Ok(d) => {
                self.picked.clear();
                self.refresh().await;
                match d.outcome_runs.first() {
                    Some(r) => {
                        let title = self.run_by_id(r).map(|r| r.title.clone()).unwrap_or_default();
                        self.say(format!("{what} → started {title}"));
                        self.select(Some(format!("r:{r}")));
                    }
                    None => self.say(format!("answered {what}")),
                }
            }
            Err(e) => self.say(format!("answer: {e:#}")),
        }
    }

    async fn dismiss(&mut self, id: String) {
        match self.rpc.call::<DecisionDismiss>(DecisionNote { id, note: None }).await {
            Ok(_) => {
                self.say("dismissed");
                self.refresh().await;
            }
            Err(e) => self.say(format!("dismiss: {e:#}")),
        }
    }

    async fn stop(&mut self, id: String) {
        match self
            .rpc
            .call::<RunStop>(RunStopParams {
                id: id.clone(),
                force: false,
            })
            .await
        {
            Ok(_) => self.say("stop requested"),
            Err(e) => self.say(format!("stop: {e:#}")),
        }
    }

    async fn mark(&mut self, id: String, mark: Mark) {
        match self.rpc.call::<RunMark>(RunMarkParams { id, mark, note: None }).await {
            Ok(r) => {
                self.say(format!("marked {}", state_label(r.state, r.confidence)));
                self.refresh().await;
            }
            Err(e) => self.say(format!("mark: {e:#}")),
        }
    }

    /// Everything on the list has been looked at.
    async fn clear_news(&mut self) {
        let ids: Vec<String> = self
            .digest
            .items
            .iter()
            .filter(|i| i.unseen)
            .map(|i| i.run.id.clone())
            .collect();
        let n = ids.len();
        self.seen(ids).await;
        self.refresh().await;
        self.say(format!("{n} cleared"));
    }

    // ---- keys --------------------------------------------------------------

    async fn key(&mut self, k: KeyEvent, size: (u16, u16)) -> Result<()> {
        self.flash = None;
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Stop(id) => {
                if k.code == KeyCode::Char('y') {
                    self.stop(id).await;
                }
                return Ok(());
            }
            Mode::Dismiss(id) => {
                if k.code == KeyCode::Char('y') {
                    self.dismiss(id).await;
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
            Mode::Filter(mut buf) => {
                match k.code {
                    KeyCode::Esc => self.filter.clear(),
                    KeyCode::Enter => self.filter = buf,
                    KeyCode::Backspace => {
                        buf.pop();
                        self.filter = buf.clone();
                        self.mode = Mode::Filter(buf);
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                        self.filter = buf.clone();
                        self.mode = Mode::Filter(buf);
                    }
                    _ => self.mode = Mode::Filter(buf),
                }
                self.settle();
                return Ok(());
            }
            Mode::Normal => {}
        }
        if self.pane == Pane::Term && self.term.is_some() {
            return self.key_term(k).await;
        }
        self.key_list(k, size).await
    }

    async fn key_list(&mut self, k: KeyEvent, size: (u16, u16)) -> Result<()> {
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Esc, _) => {
                if self.wide {
                    self.wide = false;
                } else if self.focus == Focus::Pane {
                    self.focus = Focus::List;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.settle();
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.step(1);
                self.refresh().await;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.step(-1);
                self.refresh().await;
            }
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) => self.step(isize::MIN / 2),
            (KeyCode::End, _) | (KeyCode::Char('G'), _) => self.step(isize::MAX / 2),
            (KeyCode::Enter, _) => {
                if let Some(q) = self.selected_question() {
                    if q.multi && !self.picked.is_empty() {
                        let id = q.id.clone();
                        let opts: Vec<String> = self.picked.iter().map(|&i| q.options[i].key.clone()).collect();
                        self.answer(id, opts).await;
                    } else {
                        let n = q.options.len().min(pane::MAX_KEYS);
                        self.say(format!("press 1-{n} to answer"));
                    }
                } else {
                    self.open_term(size).await;
                }
            }
            (KeyCode::Char(c @ '1'..='9'), _) => {
                if let Some(q) = self.selected_question() {
                    let i = (c as u8 - b'1') as usize;
                    if i < q.options.len() {
                        if q.multi {
                            match self.picked.iter().position(|&p| p == i) {
                                Some(at) => {
                                    self.picked.remove(at);
                                }
                                None => self.picked.push(i),
                            }
                        } else {
                            let (id, key) = (q.id.clone(), q.options[i].key.clone());
                            self.answer(id, vec![key]).await;
                        }
                    }
                }
            }
            (KeyCode::Char('n'), _) => {
                if let Some(q) = self.selected_question() {
                    self.mode = Mode::Dismiss(q.id.clone());
                }
            }
            (KeyCode::Char('i'), _) | (KeyCode::Char(' '), _) => {
                if self.sel.is_some() {
                    self.focus = Focus::Pane;
                }
            }
            (KeyCode::Char('f'), _) => self.wide = !self.wide,
            (KeyCode::Char('x'), _) => {
                if let Some(i) = self.selected_run()
                    && !i.run.state.is_terminal()
                {
                    self.mode = Mode::Stop(i.run.id.clone());
                }
            }
            (KeyCode::Char('m'), _) => {
                if let Some(i) = self.selected_run()
                    && !i.run.state.is_terminal()
                {
                    self.mode = Mode::Mark(i.run.id.clone());
                }
            }
            (KeyCode::Char('/'), _) => self.mode = Mode::Filter(self.filter.clone()),
            (KeyCode::Char('d'), _) => self.clear_news().await,
            _ => {}
        }
        Ok(())
    }

    async fn key_term(&mut self, k: KeyEvent) -> Result<()> {
        let Some(t) = &mut self.term else {
            self.close_term();
            return Ok(());
        };
        // ctrl-\ is byte 0x1c, which a plain terminal reports as ctrl-4.
        let back =
            matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) && k.modifiers.contains(KeyModifiers::CONTROL);
        if back || t.ended.is_some() || (!t.joined() && k.code == KeyCode::Esc) {
            self.close_term();
            return Ok(());
        }
        if !t.joined() {
            // Watching: `enter` joins (or takes over, once refused); `f` widens.
            match k.code {
                KeyCode::Enter => {
                    let take = t.busy_with.is_some();
                    let home = self.home.clone();
                    if let Err(e) = t.join(&home, take).await {
                        self.say(format!("join: {e:#}"));
                    }
                }
                KeyCode::Char('f') => self.wide = !self.wide,
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            }
            return Ok(());
        }
        if let Some(bytes) = keys::encode(&k, t.app_cursor()) {
            t.input(bytes).await?;
        }
        Ok(())
    }

    // ---- drawing -----------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let a = areas(f.area(), self.wide, self.focus);
        let rows = self.rows();
        let news = self.digest.items.iter().filter(|i| i.unseen).count();
        let title = rows::headline(&rows, news, &self.filter);
        let [l, r] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(self.me.len() as u16 + 1)]).areas(a.head);
        f.render_widget(Paragraph::new(title).bold(), l);
        f.render_widget(Paragraph::new(self.me.clone()).dim().right_aligned(), r);
        if let Some(list) = a.list {
            self.draw_list(f, list, &rows);
            if a.pane.is_some() {
                let x = list.x + list.width;
                let bar = Rect::new(x, list.y, 1, list.height);
                f.render_widget(Paragraph::new("│\n".repeat(list.height as usize)).dim(), bar);
            }
        }
        if let Some(pane) = a.pane {
            self.draw_pane(f, pane);
        }
        f.render_widget(Paragraph::new(format!(" {}", self.footer())).dim(), a.footer);
    }

    fn draw_list(&self, f: &mut Frame, area: Rect, rows: &[Row]) {
        if rows.is_empty() {
            let msg = if self.filter.is_empty() {
                " nothing running · rl run -- <command>"
            } else {
                " nothing matches"
            };
            f.render_widget(Paragraph::new(msg).dim(), area);
            return;
        }
        let now = Timestamp::now();
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        let mut sel_line = 0usize;
        for (n, row) in rows.iter().enumerate() {
            let selected = row.key().is_some() && row.key() == self.sel;
            let mut line = match row {
                Row::Group(g) => {
                    let mut l = vec![];
                    if n > 0 {
                        lines.push(Line::raw(""));
                    }
                    l.push(Span::raw(format!(" {g}")).bold());
                    Line::from(l)
                }
                Row::Question(q) => Line::from(vec![
                    Span::raw(" ? ").yellow().bold(),
                    Span::raw(format!("{:<8}", "to decide")).yellow(),
                    Span::raw(format!(" {:>4}  ", fmt_age(q.created_at, now))),
                    Span::raw(q.title.clone()),
                ]),
                Row::Run(i) => {
                    let r = &i.run;
                    let mark = if i.unseen { "•" } else { " " };
                    let mut spans = vec![
                        Span::raw(format!(" {mark} ")),
                        Span::styled(
                            format!("{:<8}", state_label(r.state, r.confidence)),
                            state_style(r.state),
                        ),
                        Span::raw(format!(" {:>4}  ", fmt_age(i.at, now))),
                        Span::raw(r.title.clone()),
                    ];
                    if let Some(j) = &i.joined_by {
                        spans.push(Span::raw(format!("  ● {j}")).dim());
                    }
                    spans.push(Span::raw(format!("  {}", i.summary)).dim());
                    Line::from(spans)
                }
            };
            if selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
                sel_line = lines.len();
            }
            lines.push(line);
        }
        let offset = sel_line.saturating_sub(area.height.saturating_sub(1) as usize);
        f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), area);
    }

    fn draw_pane(&mut self, f: &mut Frame, area: Rect) {
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            screen::draw(t.screen(), area, f.buffer_mut());
            return;
        }
        let now = Timestamp::now();
        let lines = match self.selected() {
            Some(Row::Run(i)) => pane::run_lines(i, &self.events, &self.names, now),
            Some(Row::Question(q)) => {
                let asker = q.run.as_deref().and_then(|id| self.run_by_id(id));
                pane::question_lines(q, asker, &self.picked, now)
            }
            _ => vec![Line::from(Span::raw(" nothing selected").dim())],
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn footer(&self) -> String {
        let title_of = |id: &str| self.run_by_id(id).map(|r| r.title.clone()).unwrap_or_default();
        match &self.mode {
            Mode::Stop(id) => return format!("stop {}? y / any key", title_of(id)),
            Mode::Dismiss(id) => {
                let t = self
                    .digest
                    .decisions
                    .iter()
                    .find(|d| &d.id == id)
                    .map(|d| d.title.clone())
                    .unwrap_or_default();
                return format!("dismiss {t}? y / any key");
            }
            Mode::Mark(id) => return format!("mark {}: d done · b blocked · w working · esc", title_of(id)),
            Mode::Filter(buf) => return format!("/{buf}▏  enter apply · esc clear"),
            Mode::Normal => {}
        }
        if let Some((m, at)) = &self.flash
            && at.elapsed() < FLASH
        {
            return m.clone();
        }
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            let mut s = String::new();
            if t.ended.is_some() {
                s.push_str("ended · any key back");
            } else if t.joined() {
                s.push_str("joined · ctrl-\\ back");
            } else if let Some(who) = &t.busy_with {
                s.push_str(&format!("watching · {who} is joined · enter to take over · esc back"));
            } else if let Some(who) = &t.info.joiner {
                s.push_str(&format!("watching · {who} is joined · enter to join · esc back"));
            } else {
                s.push_str("watching · enter to join · f wide · esc back");
            }
            if let Some((n, at)) = &t.note
                && at.elapsed() < FLASH
            {
                s.push_str(&format!(" · {n}"));
            }
            return s;
        }
        match self.selected() {
            Some(Row::Question(q)) => {
                let n = q.options.len().min(pane::MAX_KEYS);
                let how = if q.multi { "pick · enter answer" } else { "answer" };
                format!("1-{n} {how} · n not this · ↑↓ · / filter · q quit")
            }
            Some(Row::Run(i)) if i.run.state.is_terminal() => {
                "enter output · f wide · / filter · d clear news · q quit".into()
            }
            Some(Row::Run(_)) => "enter terminal · x stop · m mark · f wide · / filter · d clear news · q quit".into(),
            _ => "/ filter · q quit".into(),
        }
    }
}
