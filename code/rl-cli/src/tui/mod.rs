//! The control panel: since-you-left → fleet → detail → terminal.
//! Monochrome; colour only for attention states; the footer is the only
//! chrome (J8). Everything on screen comes from one `digest.get`.

mod keys;
mod screen;
mod term;

use crate::attach::client_name;
use crate::client::Client;
use crate::digest;
use crate::rank;
use crate::render::{fmt_clock, state_label};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use jiff::{SignedDuration, Timestamp};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, TableState, Wrap};
use relay_core::Home;
use relay_core::time::{fmt_age, fmt_duration};
use relay_proto::methods::{
    Digest, DigestGet, DigestItem, DigestParams, EventFilter, EventList, PresenceClient, PresenceTouch, RunIds,
    RunMark, RunMarkParams, RunSeen, RunStop, RunStopParams,
};
use relay_proto::types::{Event as LedgerEvent, Mark, RunState};
use std::time::{Duration, Instant};
use term::TermView;

const REFRESH: Duration = Duration::from_secs(1);
/// Ended runs stay on the fleet this long.
const RECENT: SignedDuration = SignedDuration::from_hours(24);
/// Footer messages fade after this.
const FLASH: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Since,
    Fleet,
    Detail,
    Term,
}

enum Mode {
    Normal,
    /// `x` pressed: `y` stops this run.
    Stop(String),
    /// `m` pressed: pick what to say about this run.
    Mark(String),
    /// `/` pressed: typing a fleet filter.
    Filter(String),
}

struct App {
    home: Home,
    rpc: Client,
    me: String,
    /// The since-you-left boundary: when this client last looked.
    since: Timestamp,
    digest: Digest,
    filter: String,
    screen: Screen,
    /// Where detail/terminal return to.
    from: Screen,
    mode: Mode,
    since_sel: usize,
    fleet_sel: usize,
    events: Vec<LedgerEvent>,
    term: Option<TermView>,
    flash: Option<(String, Instant)>,
    quit: bool,
}

pub async fn run(home: &Home) -> Result<()> {
    let me = client_name();
    let mut rpc = Client::connect(home).await?;
    let now = Timestamp::now();
    let since = rpc
        .call::<PresenceTouch>(PresenceClient { client: me.clone() })
        .await
        .ok()
        .and_then(|p| p.last_seen_at)
        .unwrap_or(now - RECENT);
    let mut app = App {
        home: home.clone(),
        rpc,
        me,
        since,
        digest: Digest {
            since,
            items: Vec::new(),
        },
        filter: String::new(),
        screen: Screen::Fleet,
        from: Screen::Fleet,
        mode: Mode::Normal,
        since_sel: 0,
        fleet_sel: 0,
        events: Vec::new(),
        term: None,
        flash: None,
        quit: false,
    };
    app.refresh().await;
    // The digest comes first when there is news; otherwise the fleet.
    if digest::since_items(&app.digest).iter().any(|i| i.unseen) {
        app.screen = Screen::Since;
    }

    let mut terminal = ratatui::init();
    let result = app.main_loop(&mut terminal).await;
    ratatui::restore();
    result
}

impl App {
    async fn main_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(REFRESH);
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            let pane = term_pane(terminal.size()?.into());
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => self.key(k, pane).await?,
                    Some(Ok(Event::Resize(_, _))) => {
                        if let Some(t) = &mut self.term {
                            t.resized(pane).await?;
                        }
                    }
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
                    Ok(false) => { self.say("connection closed"); self.leave_term(); }
                    Err(e) => { self.say(format!("stream: {e}")); self.leave_term(); }
                },
            }
        }
        Ok(())
    }

    fn say(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    async fn refresh(&mut self) {
        match self.rpc.call::<DigestGet>(DigestParams::default()).await {
            Ok(mut d) => {
                d.since = self.since;
                self.digest = d;
                self.clamp_selection();
            }
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        if self.screen == Screen::Detail
            && let Some(id) = self.selected_id()
        {
            let filter = EventFilter {
                run: Some(id),
                limit: Some(200),
                ..Default::default()
            };
            if let Ok(evs) = self.rpc.call::<EventList>(filter).await {
                self.events = evs;
            }
        }
    }

    // ---- lists -------------------------------------------------------------

    /// The fleet: alive, or ended within `RECENT`, matching the filter, ranked.
    fn fleet_items(&self) -> Vec<&DigestItem> {
        let now = Timestamp::now();
        let f = self.filter.to_lowercase();
        let mut items: Vec<&DigestItem> = self
            .digest
            .items
            .iter()
            .filter(|i| {
                let r = &i.run;
                !r.state.is_terminal() || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT)
            })
            .filter(|i| {
                f.is_empty()
                    || [
                        i.run.id.as_str(),
                        i.run.title.as_str(),
                        &i.run.group_key(),
                        i.run.cli.as_deref().unwrap_or(""),
                    ]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&f))
            })
            .collect();
        items.sort_by(|a, b| {
            rank::score(&a.run)
                .cmp(&rank::score(&b.run))
                .then_with(|| a.run.group_key().cmp(&b.run.group_key()))
                .then_with(|| b.run.state_at.cmp(&a.run.state_at))
        });
        items
    }

    /// The since screen's items, in group order (what the rows show).
    fn since_items(&self) -> Vec<&DigestItem> {
        digest::group(digest::since_items(&self.digest))
            .into_iter()
            .flat_map(|g| g.items)
            .collect()
    }

    fn list(&self) -> Vec<&DigestItem> {
        match self.list_screen() {
            Screen::Since => self.since_items(),
            _ => self.fleet_items(),
        }
    }

    /// Which list the selection belongs to.
    fn list_screen(&self) -> Screen {
        match self.screen {
            Screen::Since => Screen::Since,
            Screen::Fleet => Screen::Fleet,
            _ => self.from,
        }
    }

    fn sel(&self) -> usize {
        match self.list_screen() {
            Screen::Since => self.since_sel,
            _ => self.fleet_sel,
        }
    }

    fn set_sel(&mut self, i: usize) {
        match self.list_screen() {
            Screen::Since => self.since_sel = i,
            _ => self.fleet_sel = i,
        }
    }

    fn clamp_selection(&mut self) {
        let n = self.list().len();
        let s = self.sel().min(n.saturating_sub(1));
        self.set_sel(s);
    }

    fn selected(&self) -> Option<&DigestItem> {
        self.list().get(self.sel()).copied()
    }

    fn selected_id(&self) -> Option<String> {
        self.selected().map(|i| i.run.id.clone())
    }

    fn leave_term(&mut self) {
        self.term = None;
        self.screen = self.from;
    }

    async fn seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let _ = self.rpc.call::<RunSeen>(RunIds { ids }).await;
        self.refresh().await;
    }

    // ---- keys --------------------------------------------------------------

    async fn key(&mut self, k: KeyEvent, pane: (u16, u16)) -> Result<()> {
        self.flash = None;
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Stop(id) => {
                if k.code == KeyCode::Char('y') {
                    match self
                        .rpc
                        .call::<RunStop>(RunStopParams {
                            id: id.clone(),
                            force: false,
                        })
                        .await
                    {
                        Ok(_) => self.say(format!("{id}: stop requested")),
                        Err(e) => self.say(format!("stop: {e:#}")),
                    }
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
                    match self
                        .rpc
                        .call::<RunMark>(RunMarkParams {
                            id: id.clone(),
                            mark,
                            note: None,
                        })
                        .await
                    {
                        Ok(r) => {
                            self.say(format!("{id}: {}", state_label(r.state, r.confidence)));
                            self.refresh().await;
                        }
                        Err(e) => self.say(format!("mark: {e:#}")),
                    }
                }
                return Ok(());
            }
            Mode::Filter(mut buf) => {
                match k.code {
                    KeyCode::Esc => {
                        self.filter.clear();
                    }
                    KeyCode::Enter => {
                        self.filter = buf;
                    }
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
                self.clamp_selection();
                return Ok(());
            }
            Mode::Normal => {}
        }
        match self.screen {
            Screen::Term => self.key_term(k).await,
            _ => self.key_list(k, pane).await,
        }
    }

    async fn key_list(&mut self, k: KeyEvent, pane: (u16, u16)) -> Result<()> {
        let n = self.list().len();
        let cur = self.sel();
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Esc, _) => {
                if self.screen == Screen::Detail {
                    self.screen = self.from;
                }
            }
            (KeyCode::Char('a'), _) => {
                self.screen = if self.list_screen() == Screen::Since {
                    Screen::Fleet
                } else {
                    Screen::Since
                };
                self.clamp_selection();
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) if n > 0 => {
                self.set_sel((cur + 1).min(n - 1));
                self.refresh_detail().await;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) if n > 0 => {
                self.set_sel(cur.saturating_sub(1));
                self.refresh_detail().await;
            }
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) if n > 0 => self.set_sel(0),
            (KeyCode::End, _) | (KeyCode::Char('G'), _) if n > 0 => self.set_sel(n - 1),
            (KeyCode::Enter, _) => {
                if let Some(id) = self.selected_id() {
                    self.seen(vec![id.clone()]).await;
                    match TermView::open(&self.home, &id, pane).await {
                        Ok(t) => {
                            self.term = Some(t);
                            self.from = self.list_screen();
                            self.screen = Screen::Term;
                        }
                        Err(e) => self.say(format!("attach: {e:#}")),
                    }
                }
            }
            (KeyCode::Char('i'), _) | (KeyCode::Char(' '), _) => {
                if let Some(id) = self.selected_id() {
                    if self.screen != Screen::Detail {
                        self.from = self.list_screen();
                    }
                    self.screen = Screen::Detail;
                    self.seen(vec![id]).await;
                }
            }
            (KeyCode::Char('x'), _) => {
                if let Some(i) = self.selected()
                    && !i.run.state.is_terminal()
                {
                    self.mode = Mode::Stop(i.run.id.clone());
                }
            }
            (KeyCode::Char('m'), _) => {
                if let Some(i) = self.selected()
                    && !i.run.state.is_terminal()
                {
                    self.mode = Mode::Mark(i.run.id.clone());
                }
            }
            (KeyCode::Char('/'), _) if self.list_screen() == Screen::Fleet => {
                self.mode = Mode::Filter(self.filter.clone());
            }
            (KeyCode::Char('d'), _) if self.screen == Screen::Since => {
                let ids: Vec<String> = self.since_items().iter().map(|i| i.run.id.clone()).collect();
                let n = ids.len();
                self.seen(ids).await;
                let _ = self
                    .rpc
                    .call::<PresenceTouch>(PresenceClient {
                        client: self.me.clone(),
                    })
                    .await;
                self.since = Timestamp::now();
                self.digest.since = self.since;
                self.say(format!("{n} dismissed"));
                self.screen = Screen::Fleet;
            }
            (KeyCode::Char('r'), _) => self.refresh().await,
            _ => {}
        }
        Ok(())
    }

    async fn refresh_detail(&mut self) {
        if self.screen == Screen::Detail {
            self.refresh().await;
        }
    }

    async fn key_term(&mut self, k: KeyEvent) -> Result<()> {
        let Some(t) = &mut self.term else {
            self.screen = self.from;
            return Ok(());
        };
        // ctrl-\ is byte 0x1c, which a plain terminal reports as ctrl-4.
        let back =
            matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) && k.modifiers.contains(KeyModifiers::CONTROL);
        if back || t.ended.is_some() || (!t.joined() && k.code == KeyCode::Esc) {
            self.leave_term();
            return Ok(());
        }
        if !t.joined() {
            // Watching: only `enter` joins (or takes over, once refused).
            if k.code != KeyCode::Enter {
                return Ok(());
            }
            let take = t.busy_with.is_some();
            let home = self.home.clone();
            match t.join(&home, take).await {
                Ok(_) => {}
                Err(e) => self.say(format!("join: {e:#}")),
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
        let area = f.area();
        let [body, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        match self.screen {
            Screen::Since => self.draw_since(f, body),
            Screen::Fleet => self.draw_fleet(f, body),
            Screen::Detail => self.draw_detail(f, body),
            Screen::Term => self.draw_term(f, body),
        }
        f.render_widget(Paragraph::new(format!(" {}", self.footer())).dim(), footer);
    }

    fn header(&self, f: &mut Frame, area: Rect, left: String) {
        let [l, r] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(self.me.len() as u16 + 1)]).areas(area);
        f.render_widget(Paragraph::new(left).bold(), l);
        f.render_widget(Paragraph::new(self.me.clone()).dim().right_aligned(), r);
    }

    fn draw_since(&mut self, f: &mut Frame, area: Rect) {
        let now = Timestamp::now();
        let [head, rule, list, legend] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let items = digest::since_items(&self.digest);
        let (need, fin) = digest::counts(items.iter().copied());
        self.header(
            f,
            head,
            format!("relay · {}", digest::headline(self.since, need, fin, now)),
        );
        f.render_widget(Paragraph::new("─".repeat(area.width as usize)).dim(), rule);
        if items.is_empty() {
            f.render_widget(Paragraph::new(" nothing since then").dim(), list);
            return;
        }
        let mut lines: Vec<Line> = Vec::new();
        let mut idx = 0usize;
        let sel = self.since_sel;
        let mut sel_line = 0usize;
        for g in digest::group(items) {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(vec![Span::raw(" "), Span::raw(g.key).bold()]));
            for i in g.items {
                let mark = if i.unseen { "▸" } else { " " };
                let mut spans = vec![
                    Span::raw(format!(" {mark} {}  ", i.run.id)),
                    Span::styled(
                        format!("{:<12}", state_label(i.run.state, i.run.confidence)),
                        state_style(i.run.state),
                    ),
                    Span::raw(format!(" {:>4}  ", fmt_age(i.at, now))),
                    Span::raw(i.summary.clone()),
                ];
                if let Some(j) = &i.joined_by {
                    spans.push(Span::raw(format!("  ● {j}")).dim());
                }
                let mut line = Line::from(spans);
                if idx == sel {
                    line = line.style(Style::default().add_modifier(Modifier::REVERSED));
                    sel_line = lines.len();
                }
                lines.push(line);
                idx += 1;
            }
        }
        let offset = sel_line.saturating_sub(list.height.saturating_sub(1) as usize);
        f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), list);
        f.render_widget(
            Paragraph::new(" ▸ = unseen · opening a run marks it seen").dim(),
            legend,
        );
    }

    fn draw_fleet(&mut self, f: &mut Frame, area: Rect) {
        let now = Timestamp::now();
        let [head, list] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let items = self.fleet_items();
        let need = items.iter().filter(|i| i.run.state.needs_attention()).count();
        let mut title = format!("relay · {} run{}", items.len(), if items.len() == 1 { "" } else { "s" });
        if need > 0 {
            title.push_str(&format!(" · {need} need{} you", if need == 1 { "s" } else { "" }));
        }
        if !self.filter.is_empty() {
            title.push_str(&format!(" · /{}", self.filter));
        }
        self.header(f, head, title);
        if items.is_empty() {
            let msg = if self.filter.is_empty() {
                " no runs · start one with `rl run -- <command>`"
            } else {
                " nothing matches the filter"
            };
            f.render_widget(Paragraph::new(msg).dim(), list);
            return;
        }
        let header = Row::new(["ID", "STATE", "AGE", "CLI", "GROUP", "TITLE", "LAST", ""]).dim();
        let rows = items.iter().map(|i| {
            let r = &i.run;
            Row::new(vec![
                Span::raw(r.id.clone()),
                Span::styled(state_label(r.state, r.confidence), state_style(r.state)),
                Span::raw(fmt_age(r.state_at, now)),
                Span::raw(r.cli.clone().unwrap_or_else(|| r.kind.to_string())),
                Span::raw(short_group(&r.group_key())),
                Span::raw(r.title.clone()),
                Span::raw(i.summary.clone()).dim(),
                Span::raw(i.joined_by.as_ref().map(|j| format!("● {j}")).unwrap_or_default()).dim(),
            ])
        });
        let joined_w = items
            .iter()
            .filter_map(|i| i.joined_by.as_ref().map(|j| j.len() as u16 + 2))
            .max()
            .unwrap_or(0);
        let table = Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Length(4),
                Constraint::Length(7),
                Constraint::Max(16),
                Constraint::Fill(3),
                Constraint::Fill(3),
                Constraint::Length(joined_w),
            ],
        )
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .column_spacing(2);
        let mut state = TableState::default().with_selected(self.fleet_sel);
        f.render_stateful_widget(table, list, &mut state);
    }

    fn draw_detail(&mut self, f: &mut Frame, area: Rect) {
        let Some(item) = self.selected() else {
            f.render_widget(Paragraph::new(" nothing selected").dim(), area);
            return;
        };
        let r = &item.run;
        let now = Timestamp::now();
        let fact = |k: &str, v: String| Line::from(vec![Span::raw(format!(" {k:<10} ")).dim(), Span::raw(v)]);
        let mut lines: Vec<Line> = vec![Line::from(vec![
            Span::raw(r.id.clone()).bold(),
            Span::raw("  "),
            Span::styled(state_label(r.state, r.confidence), state_style(r.state)),
            Span::raw(format!("  via {} · {} in state", r.source, fmt_age(r.state_at, now))).dim(),
            Span::raw(format!(
                "    {} · {}",
                r.cli.clone().unwrap_or_else(|| "-".into()),
                r.kind
            ))
            .dim(),
        ])];
        lines.push(fact("title", r.title.clone()));
        let mut group = r.group_key();
        if r.ref_.is_some() {
            group.push_str("  (ref)");
        }
        if let Some(b) = &r.branch {
            group.push_str(&format!("      branch  {b}"));
        }
        lines.push(fact("group", group));
        let mut cwd = r.cwd.display().to_string();
        if !r.tags.is_empty() {
            cwd.push_str(&format!("      tags  {}", r.tags.join(", ")));
        }
        lines.push(fact("cwd", cwd));
        lines.push(fact("argv", r.argv.join(" ")));
        let mut started = format!("{} ({} ago)", fmt_clock(r.started_at, now), fmt_age(r.started_at, now));
        if let Some(j) = &item.joined_by {
            started.push_str(&format!("      joined  {j}"));
        }
        lines.push(fact("started", started));
        if let Some(e) = r.ended_at {
            let d = fmt_duration(e.duration_since(r.started_at));
            let code = r.exit_code.map(|c| format!(", exit {c}")).unwrap_or_default();
            lines.push(fact("ended", format!("{} (after {d}{code})", fmt_clock(e, now))));
        }
        if let Some(d) = &r.detail {
            lines.push(fact("doing", d.clone()));
        }
        if let Some(l) = &r.last_line {
            lines.push(fact("last line", l.clone()));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(" events").dim());
        if self.events.is_empty() {
            lines.push(Line::raw("   (none yet)").dim());
        }
        for e in self.events.iter().rev() {
            lines.push(Line::from(vec![
                Span::raw(format!(" {}  ", fmt_clock(e.at, now))).dim(),
                Span::raw(format!("{:<16} ", e.kind)),
                Span::raw(e.summary.clone()),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(" facts (P1): commits · files · commands · questions · cost — n/a").dim());
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn draw_term(&mut self, f: &mut Frame, area: Rect) {
        if let Some(t) = &self.term {
            screen::draw(t.screen(), area, f.buffer_mut());
        }
    }

    fn footer(&self) -> String {
        match &self.mode {
            Mode::Stop(id) => {
                let title = self
                    .digest
                    .items
                    .iter()
                    .find(|i| &i.run.id == id)
                    .map(|i| i.run.title.clone())
                    .unwrap_or_default();
                return format!("stop {id} ({title})? y / any key");
            }
            Mode::Mark(id) => return format!("mark {id}: d done · b blocked · w working · esc"),
            Mode::Filter(buf) => return format!("/{buf}▏  enter apply · esc clear"),
            Mode::Normal => {}
        }
        if let Some((m, at)) = &self.flash
            && at.elapsed() < FLASH
        {
            return m.clone();
        }
        match self.screen {
            Screen::Since => "enter terminal · i detail · x stop · d dismiss all · a fleet · q quit".into(),
            Screen::Fleet => "enter terminal · i detail · x stop · m mark · / filter · a since · q quit".into(),
            Screen::Detail => "enter terminal · x stop · m mark · esc back".into(),
            Screen::Term => {
                let Some(t) = &self.term else { return String::new() };
                let mut s = format!("{} · ", t.id);
                if t.ended.is_some() {
                    s.push_str("ended · any key back");
                } else if t.joined() {
                    s.push_str("joined · ctrl-\\ back");
                } else if let Some(who) = &t.busy_with {
                    s.push_str(&format!("watching · {who} is joined · enter to take over · esc back"));
                } else if let Some(who) = &t.info.joiner {
                    s.push_str(&format!("watching · {who} is joined · enter to join · esc back"));
                } else {
                    s.push_str("watching · enter to join · esc back");
                }
                if let Some((n, at)) = &t.note
                    && at.elapsed() < FLASH
                {
                    s.push_str(&format!(" · {n}"));
                }
                s
            }
        }
    }
}

/// The area the terminal screen draws into: everything but the footer.
fn term_pane(size: Rect) -> (u16, u16) {
    (size.width.max(1), size.height.saturating_sub(1).max(1))
}

fn state_style(s: RunState) -> Style {
    match s {
        RunState::NeedsInput | RunState::Blocked => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        RunState::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        RunState::Stale => Style::default().fg(Color::Magenta),
        RunState::Finished | RunState::Stopped | RunState::Idle => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

/// A workspace path as a group reads better as its last component.
fn short_group(g: &str) -> String {
    if g.starts_with('/') {
        g.rsplit('/').next().unwrap_or(g).to_string()
    } else {
        g.to_string()
    }
}
