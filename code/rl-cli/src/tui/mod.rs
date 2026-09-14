//! The control panel: one list, one keystroke to anything.
//!
//! Everything ranked by attention — questions, then runs that need you,
//! then what is working and what finished — grouped by the folder it belongs
//! to, whose rules ride the group's header. `enter` is the run's terminal,
//! full screen and watched; `enter` again takes the keyboard, the way you
//! would have it in a tab of your own, and `ctrl-\` lets go.
//!
//! Typing is what the panel rests in. One live line sits at the foot and
//! always takes what you type: a shell line runs here, an agent's name runs
//! that agent, and anything else is words the assistant reads and drafts into
//! the form for your `enter`. The line says which of those it is, at its right
//! edge, before you commit. `tab` completes against the folder of the row you
//! are on. Everything that is not typing is a `/` command. Colour carries
//! meaning — a saturated accent per state, on the terminal's own foreground so
//! it reads on either theme (see `theme`); the footer is the only chrome (J8).
//! Nothing on screen is an id.

mod complete;
pub mod form;
mod keys;
mod line;
mod palette;
mod pane;
mod rows;
mod rules;
mod screen;
mod term;
mod theme;

use crate::attach::client_name;
use crate::client::Client;
use crate::rulefile;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use form::{Act, Form};
use futures::StreamExt;
use jiff::Timestamp;
use pane::state_style;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use relay_core::Home;
use relay_core::time::fmt_age;
use relay_proto::decision::Decision;
use relay_proto::methods::{
    AgentList, DecisionDismiss, DecisionNote, DecisionTake, DecisionTakeParams, Digest, DigestGet, DigestItem,
    DigestParams, EventFilter, EventList, GcParams, GcRun, PresenceClient, PresenceTouch, ReactionPause,
    ReactionReload, ReactionResume, RuleApprove, RuleList, RuleRef, RunArchive, RunArchiveParams, RunId, RunIds,
    RunMark, RunMarkParams, RunSeen, RunSpec, RunStart, RunStop, RunStopParams, ScheduleFilter, ScheduleName,
    SchedulePause, ScheduleReload, ScheduleResume, ScheduleRun, SummaryGet, SummaryMake, SummaryParams, WorkspaceList,
};
use relay_proto::rule::{Rule, RuleKind, RuleStatus};
use relay_proto::types::{Cause, Event as LedgerEvent, Mark, Run, RunKind, RunState, Summary};
use rows::{Names, Row, fit, short_path};
use rules::RuleRow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use term::TermView;

const REFRESH: Duration = Duration::from_secs(1);
/// Footer messages fade after this.
const FLASH: Duration = Duration::from_secs(4);
/// The most candidates `tab` shows at once.
const CANDS: usize = 6;
/// Narrower than this, the pane takes the screen instead of sharing it.
const SPLIT_AT: u16 = 100;
/// The list's share of a split screen, in percent.
const LIST_SHARE: u16 = 50;
/// Air around the panel: columns each side, a row top and bottom. None of it
/// in the full-screen terminal, which wants every cell.
const GUTTER_X: u16 = 2;
const GUTTER_Y: u16 = 1;

/// What the right pane shows for a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Facts,
    Term,
}

/// Which list the left side shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Work,
    Rules,
}

enum Mode {
    Normal,
    /// `x` pressed: `y` stops this run.
    Stop(String),
    /// `m` pressed: pick what to say about this run.
    Mark(String),
    /// `n` pressed: `y` dismisses this question.
    Dismiss(String),
    /// `enter` on a schedule: `y` fires it now.
    Fire(ScheduleName),
    /// A run or a rule being written down, by hand or drafted for you.
    Form(Box<Form>),
    /// A question taken out of the list: its options, answered by number.
    Answer(String),
}

/// What `tab` is offering for the word being typed, and where the cycle is.
struct Completing {
    /// The line as it was when tab was first pressed, and where the cursor
    /// was: every candidate goes into that, never into the last one taken.
    line: String,
    cursor: usize,
    /// The line as it stands with that candidate in it, so any keystroke
    /// since drops the offer.
    shown: String,
    cands: Vec<complete::Candidate>,
    at: usize,
}

/// Words handed to the assistant, until it answers.
struct Asking {
    words: String,
    folder: PathBuf,
    editing: Option<Rule>,
    task: tokio::task::JoinHandle<Result<(crate::interpret::Draft, relay_proto::methods::AssistantOutput)>>,
}

/// A run being summarised, until the assistant answers.
struct Summarising {
    run: String,
    title: String,
    task: tokio::task::JoinHandle<Result<Summary>>,
}

struct App {
    home: Home,
    rpc: Client,
    me: String,
    digest: Digest,
    names: Names,
    filter: String,
    /// Which slice of the list is shown: all, agents, jobs or reactions.
    lens: rows::Lens,
    view: View,
    /// Every rule, fetched while the rules view is up.
    rules: Vec<Rule>,
    /// The other view's selection, kept for when you come back.
    other_sel: Option<String>,
    /// The rules view, narrowed to one folder's rules (`r` on its row).
    rules_scope: Option<PathBuf>,
    /// Jobs opened to their runs.
    opened: HashSet<String>,
    /// First list line on screen, and how many fit: the scroll map. Cells,
    /// because drawing sets them while the rows still borrow the app.
    scroll: std::cell::Cell<usize>,
    list_h: std::cell::Cell<u16>,
    /// The selected row's key (`r:<id>` or `q:<id>`); the cursor follows it
    /// across re-ranks.
    sel: Option<String>,
    pane: Pane,
    /// The pane beside the list (`i`); off, the list has the width.
    show_pane: bool,
    /// The pane alone, full width: a terminal, unless `f` says otherwise.
    wide: bool,
    term: Option<TermView>,
    /// Size the terminal was last told about.
    term_size: (u16, u16),
    /// The selected run's recent events.
    events: Vec<LedgerEvent>,
    /// The selected run's summary, when there is one kept.
    summary: Option<Summary>,
    /// The assistant at work on a run's summary (`s`).
    summarising: Option<Summarising>,
    /// `E` pressed: the file and line to open in the editor once the
    /// screen is given back.
    editor: Option<(PathBuf, usize)>,
    /// The assistant at work on some words: what was asked, the rule it is
    /// about if any, and the task waiting for the answer.
    asking: Option<Asking>,
    /// Options marked so far on a multi-choice question.
    picked: Vec<usize>,
    /// The live line at the foot; typing is the panel's resting state.
    line: line::Line,
    /// The agents the registry knows (`agent.list`), read once.
    agents: Vec<String>,
    /// The directories of $PATH, for completing the first word.
    path_dirs: Vec<PathBuf>,
    completing: Option<Completing>,
    mode: Mode,
    flash: Option<(String, Instant)>,
    quit: bool,
}

/// The first of `n` lines to show in a window `h` tall so that line `sel` is
/// visible and not under an edge marker: when the window starts past the
/// top, its first line says how much is above; when it ends before the end,
/// its last line says how much is below. `prev` is where the window was, so
/// it only moves when it must.
fn viewport(n: usize, h: usize, sel: usize, prev: usize) -> usize {
    if n <= h || h < 3 {
        return 0;
    }
    let mut top = prev.min(n - h);
    if sel < top + usize::from(top > 0) {
        top = sel.saturating_sub(1);
    }
    loop {
        let more = top + h < n;
        let last = top + h - 1 - usize::from(more);
        if sel <= last || top + h >= n {
            break;
        }
        top += 1;
    }
    top
}

/// One row of the list, to the width: mark, title, state, the summary
/// filling the middle, who is joined, then the ref and the age at the right.
#[allow(clippy::too_many_arguments)]
fn list_line(
    w: usize,
    tw: usize,
    mark: Span<'static>,
    title: Span<'static>,
    word: Span<'static>,
    summary: &str,
    joined: Option<&str>,
    ref_: Option<&str>,
    age: String,
) -> Line<'static> {
    let right = match ref_ {
        Some(r) => format!("{r}  {age:>4}"),
        None => format!("{age:>4}"),
    };
    let joined = joined.map(|j| format!("● {j}  ")).unwrap_or_default();
    let fixed = 3 + tw + 1 + 10 + 1 + joined.chars().count() + right.chars().count() + 1;
    let mut spans = vec![mark, title, Span::raw(" "), word, Span::raw(" ")];
    if !joined.is_empty() {
        spans.push(Span::raw(joined).dim());
    }
    spans.push(Span::raw(fit(summary, w.saturating_sub(fixed))).dim());
    spans.push(Span::raw(format!(" {right}")).dim());
    Line::from(spans)
}

/// Where things go on screen.
struct Areas {
    head: Rect,
    /// The lens strip, just under the header, in the work view.
    lens: Option<Rect>,
    /// The rules between the sections: under the header, above the line, above
    /// the footer. Empty in the full-screen terminal.
    rules: Vec<Rect>,
    list: Option<Rect>,
    pane: Option<Rect>,
    /// The form, at the foot of the body.
    form: Option<Rect>,
    /// What `tab` is offering, just above the line.
    cands: Option<Rect>,
    /// The live line.
    line: Rect,
    footer: Rect,
}

fn areas(area: Rect, wide: bool, show_pane: bool, form_h: u16, cands_h: u16, lens_h: u16) -> Areas {
    // A gutter of air around the panel and rules between its sections — but not
    // in the full-screen terminal, which wants every cell.
    let chrome = !wide;
    let area = if chrome {
        Rect {
            x: area.x + GUTTER_X,
            y: area.y + GUTTER_Y,
            width: area.width.saturating_sub(2 * GUTTER_X),
            height: area.height.saturating_sub(2 * GUTTER_Y),
        }
    } else {
        area
    };
    let rule = u16::from(chrome);
    let [head, lens, rule_a, body, form, cands, rule_b, line, rule_c, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(lens_h),
        Constraint::Length(rule),
        Constraint::Fill(1),
        Constraint::Length(if form_h > 0 { form_h + 1 } else { 0 }),
        Constraint::Length(cands_h),
        Constraint::Length(rule),
        Constraint::Length(1),
        Constraint::Length(rule),
        Constraint::Length(1),
    ])
    .areas(area);
    let rules = if chrome { vec![rule_a, rule_b, rule_c] } else { vec![] };
    let lens = (lens_h > 0).then_some(lens);
    let form = (form_h > 0).then_some(form);
    let cands = (cands_h > 0).then_some(cands);
    let (list, pane) = if wide {
        (None, Some(body))
    } else if !show_pane {
        (Some(body), None)
    } else if area.width < SPLIT_AT {
        (None, Some(body))
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
        lens,
        rules,
        list,
        pane,
        form,
        cands,
        line,
        footer,
    }
}

pub async fn run(home: &Home) -> Result<()> {
    let me = client_name();
    // Before the screen is taken: the daemon is started or, if it is another
    // release, a restart is offered on the plain terminal.
    let mut rpc = crate::daemon::ensure(home, true).await?;
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
        lens: rows::Lens::All,
        view: View::Work,
        rules: Vec::new(),
        other_sel: None,
        rules_scope: None,
        opened: HashSet::new(),
        scroll: std::cell::Cell::new(0),
        list_h: std::cell::Cell::new(0),
        sel: None,
        pane: Pane::Facts,
        show_pane: false,
        wide: false,
        term: None,
        term_size: (0, 0),
        events: Vec::new(),
        editor: None,
        asking: None,
        summary: None,
        summarising: None,
        picked: Vec::new(),
        line: line::Line::default(),
        agents: Vec::new(),
        path_dirs: complete::path_dirs(),
        completing: None,
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
            let area: Rect = terminal.size()?.into();
            let size = self.pane_size(area);
            if self.pane == Pane::Term && self.term.is_some() && size != self.term_size {
                self.term_size = size;
                if let Some(t) = &mut self.term {
                    t.resized(size).await?;
                }
            }
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                        self.key(k, area).await?;
                        if let Some((file, line)) = self.editor.take() {
                            // The editor gets the terminal; our reader lets go of stdin first.
                            drop(input);
                            ratatui::restore();
                            let outcome = crate::env::open_in_editor(&file, line);
                            *terminal = ratatui::init();
                            input = EventStream::new();
                            match outcome {
                                Ok(()) => self.reload_rules().await,
                                Err(e) => self.say(format!("{e:#}")),
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => self.quit = true,
                },
                _ = tick.tick() => self.refresh().await,
                answer = async {
                    match &mut self.asking {
                        Some(a) => (&mut a.task).await,
                        None => std::future::pending().await,
                    }
                } => self.answered(answer).await,
                done = async {
                    match &mut self.summarising {
                        Some(s) => (&mut s.task).await,
                        None => std::future::pending().await,
                    }
                } => self.summarised(done).await,
                more = async {
                    match &mut self.term {
                        Some(t) if !t.over => t.pump().await,
                        _ => std::future::pending().await,
                    }
                } => match more {
                    Ok(true) => {}
                    Ok(false) => {
                        // An ended run's output is still worth reading, so the
                        // view stays; there is just nothing more to poll.
                        if self.term.as_ref().is_some_and(|t| t.ended.is_none()) {
                            self.say("connection closed");
                            self.close_term();
                        }
                    }
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
        let a = areas(size, self.wide, true, 0, 0, self.lens_h());
        let r = a.pane.unwrap_or(size);
        (r.width.max(1), r.height.max(1))
    }

    /// The lens strip is a row of its own in the work view, unless the pane has
    /// taken the whole screen.
    fn lens_h(&self) -> u16 {
        u16::from(self.view == View::Work && !self.wide)
    }

    /// Move the lens along the strip; the selection settles onto whatever the
    /// new slice shows.
    fn cycle_lens(&mut self, by: isize) {
        let all = rows::Lens::ALL;
        let at = all.iter().position(|l| *l == self.lens).unwrap_or(0);
        let n = all.len() as isize;
        self.set_lens(all[(((at as isize + by) % n + n) % n) as usize]);
    }

    /// Set the lens outright, and settle the selection onto the new slice.
    fn set_lens(&mut self, lens: rows::Lens) {
        self.lens = lens;
        self.settle();
    }

    async fn refresh(&mut self) {
        if let Ok(ws) = self.rpc.call::<WorkspaceList>(()).await {
            self.names = Names::new(ws.into_iter().map(|w| (w.path, w.name)));
        }
        match self.rpc.call::<DigestGet>(DigestParams::default()).await {
            Ok(d) => self.digest = d,
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        match self.rpc.call::<RuleList>(ScheduleFilter { all: false }).await {
            Ok(r) => self.rules = r,
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        // Which CLIs are agents decides how a line reads; asked once.
        if self.agents.is_empty()
            && let Ok(list) = self.rpc.call::<AgentList>(()).await
        {
            self.agents = list.into_iter().map(|a| a.name).collect();
        }
        self.settle();
        if self.pane == Pane::Facts
            && let Some(id) = self.selected_run().map(|i| i.run.id.clone())
        {
            let filter = EventFilter {
                run: Some(id.clone()),
                limit: Some(200),
                ..Default::default()
            };
            if let Ok(evs) = self.rpc.call::<EventList>(filter).await {
                self.events = evs;
            }
            if let Ok(s) = self.rpc.call::<SummaryGet>(RunId { id }).await {
                self.summary = s;
            }
        }
    }

    // ---- the list ----------------------------------------------------------

    fn rows(&self) -> Vec<Row<'_>> {
        rows::build(
            &self.digest,
            &rows::Context {
                names: &self.names,
                rules: &self.rules,
                opened: &self.opened,
                filter: &self.filter,
                lens: self.lens,
                now: Timestamp::now(),
            },
        )
    }

    fn rule_rows(&self) -> Vec<RuleRow<'_>> {
        rules::build(&self.rules, &self.names, &self.filter, self.rules_scope.as_deref())
    }

    /// The folder the selected row's work is in.
    fn selected_folder(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Run(i) | Row::Fold(i) => Some(i.run.workspace.clone()),
            Row::Job(j) => Some(j.last().run.workspace.clone()),
            Row::Question(q) => Some(q.workspace.clone()),
            Row::Group(_) => None,
        }
    }

    /// Keys of the selectable rows of the view that is up, in order.
    fn keys(&self) -> Vec<String> {
        match self.view {
            View::Work => self.rows().into_iter().filter_map(|r| r.key()).collect(),
            View::Rules => self.rule_rows().into_iter().filter_map(|r| r.key()).collect(),
        }
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
        if self.view != View::Work {
            return None;
        }
        let key = self.sel.as_ref()?;
        self.rows().into_iter().find(|r| r.key().as_ref() == Some(key))
    }

    fn selected_run(&self) -> Option<&DigestItem> {
        self.selected().and_then(|r| r.item())
    }

    fn selected_job(&self) -> Option<rows::Job<'_>> {
        match self.selected() {
            Some(Row::Job(j)) => Some(j),
            _ => None,
        }
    }

    fn selected_question(&self) -> Option<&Decision> {
        match self.selected() {
            Some(Row::Question(d)) => Some(d),
            _ => None,
        }
    }

    fn selected_rule(&self) -> Option<&Rule> {
        if self.view != View::Rules {
            return None;
        }
        let key = self.sel.as_ref()?;
        self.rules.iter().find(|r| &rules::key(r) == key)
    }

    /// The rule behind a job row.
    fn rule_of_job(&self, j: &rows::Job) -> Option<&Rule> {
        self.rules.iter().find(|x| {
            x.kind() == j.kind
                && x.name() == j.name
                && (x.is_global() || Path::new(x.scope()) == j.last().run.workspace)
        })
    }

    /// The rule under the cursor: in the rules view, or behind a job row.
    fn rule_here(&self) -> Option<&Rule> {
        self.selected_rule()
            .or_else(|| self.selected_job().and_then(|j| self.rule_of_job(&j)))
    }

    fn run_by_id(&self, id: &str) -> Option<&Run> {
        self.digest.items.iter().map(|i| &i.run).find(|r| r.id == id)
    }

    /// The cursor is on it: that is looking. A job's runs count as looked at
    /// when the job is.
    async fn saw_selected(&mut self) {
        let ids: Vec<String> = match self.selected() {
            Some(Row::Run(i)) | Some(Row::Fold(i)) if i.unseen => vec![i.run.id.clone()],
            Some(Row::Job(j)) => j.runs.iter().filter(|i| i.unseen).map(|i| i.run.id.clone()).collect(),
            _ => vec![],
        };
        self.seen(ids).await;
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

    /// The view goes for good: the stream died, or another run wants it.
    fn close_term(&mut self) {
        if self.term.is_some() {
            self.wide = false;
        }
        self.term = None;
        self.pane = Pane::Facts;
    }

    /// Stepping back to the list. The view stays open and goes on reading, so
    /// coming back is instant and lands where it was left; the keyboard, if
    /// it was ours, goes back to the run.
    fn leave_term(&mut self) {
        if let Some(t) = &mut self.term {
            t.unjoin();
        }
        self.wide = false;
        self.pane = Pane::Facts;
    }

    async fn seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let _ = self.rpc.call::<RunSeen>(RunIds { ids }).await;
    }

    // ---- doing things ------------------------------------------------------

    /// The selected run's terminal, full screen: joined, or only watched.
    async fn open_term(&mut self, area: Rect, join: bool) {
        let Some(id) = self.selected_run().map(|i| i.run.id.clone()) else {
            self.say("nothing here to open");
            return;
        };
        // Still reading this one: back to it, where it was left.
        if self.term.as_ref().is_some_and(|t| t.id == id) {
            self.pane = Pane::Term;
            self.wide = true;
            self.term_size = self.pane_size(area);
            if join {
                let home = self.home.clone();
                if let Some(t) = &mut self.term {
                    let take = t.busy_with.is_some();
                    if let Err(e) = t.join(&home, take).await {
                        self.say(format!("join: {e:#}"));
                    }
                }
            }
            return;
        }
        self.seen(vec![id.clone()]).await;
        let a = areas(area, true, true, 0, 0, 0);
        let r = a.pane.unwrap_or(area);
        let size = (r.width.max(1), r.height.max(1));
        match TermView::open(&self.home, &id, size).await {
            Ok(mut t) => {
                if join && let Err(e) = t.join(&self.home, false).await {
                    self.say(format!("join: {e:#}"));
                }
                self.term = Some(t);
                self.term_size = size;
                self.pane = Pane::Term;
                self.wide = true;
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
                self.say(format!("marked {}", rows::state_words(r.state, r.confidence)));
                self.refresh().await;
            }
            Err(e) => self.say(format!("mark: {e:#}")),
        }
    }

    /// Done with these ended runs: out of the list, kept in the ledger.
    async fn archive(&mut self, ids: Vec<String>, what: &str) {
        match self
            .rpc
            .call::<RunArchive>(RunArchiveParams { ids, all_seen: false })
            .await
        {
            Ok(0) => self.say("nothing to be done with: still going"),
            Ok(_) => {
                self.say(format!("done with {what}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("done: {e:#}")),
        }
    }

    /// `ctrl-x`: get whatever the cursor is on out of the list. An ended run
    /// or a job is archived at once (reversible: the ledger keeps it); a run
    /// still going is stopped first, so a second `ctrl-x` once it has ended is
    /// what archives it; a question is dismissed. Nothing here is destroyed —
    /// that is retention's alone.
    async fn remove_selected(&mut self) {
        if let Some(j) = self.selected_job() {
            let ids: Vec<String> = j.runs.iter().map(|i| i.run.id.clone()).collect();
            let what = format!("{} · {} runs", j.name, ids.len());
            self.archive(ids, &what).await;
        } else if let Some(i) = self.selected_run() {
            if i.run.state.is_terminal() {
                let (id, title) = (i.run.id.clone(), i.run.title.clone());
                self.archive(vec![id], &title).await;
            } else {
                // Can't archive what is still going; stop it, then a second
                // ctrl-x is done with it.
                self.mode = Mode::Stop(i.run.id.clone());
            }
        } else if let Some(q) = self.selected_question() {
            self.mode = Mode::Dismiss(q.id.clone());
        } else {
            self.say("nothing here to remove");
        }
    }

    /// Done with everything ended that has been looked at.
    async fn archive_seen(&mut self) {
        match self
            .rpc
            .call::<RunArchive>(RunArchiveParams {
                ids: vec![],
                all_seen: true,
            })
            .await
        {
            Ok(n) => {
                self.say(format!("done with {n}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("done: {e:#}")),
        }
    }

    // ---- rules -------------------------------------------------------------

    async fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Work => View::Rules,
            View::Rules => View::Work,
        };
        std::mem::swap(&mut self.sel, &mut self.other_sel);
        self.picked.clear();
        self.events.clear();
        self.close_term();
        self.refresh().await;
    }

    async fn approve(&mut self, name: String, scope: String, kind: RuleKind) {
        let p = RuleRef {
            name: name.clone(),
            scope: Some(scope),
            kind: Some(kind),
        };
        match self.rpc.call::<RuleApprove>(p).await {
            Ok(_) => {
                self.say(format!("{name} approved"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("approve: {e:#}")),
        }
    }

    async fn set_paused(&mut self, name: String, scope: String, kind: RuleKind, paused: bool) {
        let p = ScheduleName {
            name: name.clone(),
            scope: Some(scope),
        };
        let r = match (kind, paused) {
            (RuleKind::Schedule, true) => self.rpc.call::<SchedulePause>(p).await.map(|_| ()),
            (RuleKind::Schedule, false) => self.rpc.call::<ScheduleResume>(p).await.map(|_| ()),
            (RuleKind::Reaction, true) => self.rpc.call::<ReactionPause>(p).await.map(|_| ()),
            (RuleKind::Reaction, false) => self.rpc.call::<ReactionResume>(p).await.map(|_| ()),
        };
        match r {
            Ok(()) => {
                self.say(format!("{name} {}", if paused { "paused" } else { "resumed" }));
                self.refresh().await;
            }
            Err(e) => self.say(format!("pause: {e:#}")),
        }
    }

    async fn fire(&mut self, name: ScheduleName) {
        match self.rpc.call::<ScheduleRun>(name).await {
            Ok(run) => {
                self.say(format!("started {}", run.title));
                self.refresh().await;
            }
            Err(e) => self.say(format!("run: {e:#}")),
        }
    }

    async fn reload_rules(&mut self) {
        let s = self.rpc.call::<ScheduleReload>(()).await;
        let r = self.rpc.call::<ReactionReload>(()).await;
        match (s, r) {
            (Ok(s), Ok(r)) => {
                self.say(format!("reloaded · {} schedules · {} reactions", s.loaded, r.loaded));
                self.refresh().await;
            }
            (Err(e), _) | (_, Err(e)) => self.say(format!("reload: {e:#}")),
        }
    }

    // ---- the form ----------------------------------------------------------

    /// Where a new thing would go: the selected row's folder, else here.
    fn folder_here(&self) -> PathBuf {
        self.selected_folder()
            .or_else(|| {
                self.rule_here()
                    .filter(|r| !r.is_global())
                    .map(|r| PathBuf::from(r.scope()))
            })
            .or_else(|| self.rules_scope.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn new_rule_form(&mut self) {
        let folder = self.folder_here();
        let yours =
            self.view == View::Rules && self.rules_scope.is_none() && self.rule_here().is_none_or(Rule::is_global);
        let form = Form::rule((!yours).then_some(folder.as_path()), &folder)
            .with_folders(&self.names.paths())
            .with_agents(&self.agents);
        self.mode = Mode::Form(Box::new(form));
    }

    fn edit_rule_form(&mut self) {
        if let Some(r) = self.rule_here() {
            let form = Form::edit(r)
                .with_folders(&self.names.paths())
                .with_agents(&self.agents);
            self.mode = Mode::Form(Box::new(form));
        }
    }

    /// `E`: the rule's file in the editor, at its line, once the screen is free.
    fn edit_rule_file(&mut self) {
        if let Some(r) = self.rule_here() {
            let file = r.source().to_path_buf();
            let line = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| rulefile::line_of(&t, rulefile::top(r.kind()), r.name()))
                .unwrap_or(1);
            self.editor = Some((file, line));
        }
    }

    /// `enter` on the form: what it says, or why not — the form stays up then.
    async fn save_form(&mut self, f: Box<Form>, area: Rect) {
        let outcome = match f.kind {
            form::Kind::Run => match form::run_draft(&f) {
                Ok(d) => self.start_run(d, area).await,
                Err(e) => Err(e),
            },
            form::Kind::Rule => match form::rule_draft(&f) {
                Ok(d) => self.save_rule(d, f.editing.as_ref()).await,
                Err(e) => Err(e),
            },
        };
        if let Err(e) = outcome {
            self.say(format!("{e:#}"));
            self.mode = Mode::Form(f);
        }
    }

    /// A run from the form, sized to this screen; an agent's is joined at once.
    async fn start_run(&mut self, d: form::RunDraft, area: Rect) -> Result<()> {
        let spec = RunSpec {
            argv: d.argv,
            kind: d.kind,
            cwd: d.cwd,
            title: d.title,
            ref_: None,
            tags: vec![],
            tty: true,
            cols: Some(area.width),
            rows: Some(area.height.saturating_sub(2)),
            env: crate::env::child_env(&[])?,
            inherit_env: false,
            cause: Cause::Manual,
            flow: None,
        };
        let run = self.rpc.call::<RunStart>(spec).await?;
        self.say(format!("started {}", run.title));
        if self.view == View::Rules {
            self.toggle_view().await;
        }
        self.refresh().await;
        self.select(Some(format!("r:{}", run.id)));
        if run.kind == RunKind::Agent {
            self.open_term(area, true).await;
        }
        Ok(())
    }

    /// A rule from the form into its file, then loaded and, since this is
    /// your keystroke, on. A folder new to Relay is registered on the way.
    async fn save_rule(&mut self, d: form::RuleDraft, was: Option<&form::Editing>) -> Result<()> {
        let registered = d.folder.as_deref().is_none_or(|f| self.names.registered(f).is_some());
        let was = was.map(form::Editing::was);
        let file = rulefile::install(&self.home, &mut self.rpc, &d, was.as_ref(), registered).await?;
        self.say(format!("{} on · {}", d.name, short_path(&file)));
        self.refresh().await;
        if self.view == View::Rules {
            self.select(Some(format!("{}:{}/{}", d.kind, d.scope(), d.name)));
        }
        Ok(())
    }

    // ---- asking ------------------------------------------------------------

    /// `enter` on the ask line: the words go to the assistant as a run; the
    /// panel stays live and the answer comes back through the main loop.
    fn ask(&mut self, words: String) {
        if words.trim().is_empty() {
            return;
        }
        let editing = self.rule_here().cloned();
        let folder = match &editing {
            Some(r) if !r.is_global() => PathBuf::from(r.scope()),
            _ => self.folder_here(),
        };
        let home = self.home.clone();
        let (w, f, e) = (words.clone(), folder.clone(), editing.clone());
        let task = tokio::spawn(async move { crate::interpret::ask(&home, &f, &w, e.as_ref()).await });
        self.asking = Some(Asking {
            words,
            folder,
            editing,
            task,
        });
    }

    async fn answered(
        &mut self,
        answer: std::result::Result<
            Result<(crate::interpret::Draft, relay_proto::methods::AssistantOutput)>,
            tokio::task::JoinError,
        >,
    ) {
        let Some(asking) = self.asking.take() else { return };
        match answer {
            Ok(Ok((draft, out))) => {
                let form = crate::interpret::form_of(&draft, &asking.folder, asking.editing.as_ref())
                    .with_folders(&self.names.paths())
                    .with_agents(&self.agents);
                self.mode = Mode::Form(Box::new(form));
                self.say(format!(
                    "{} drafted this from \"{}\" · enter does it · esc does not",
                    out.agent, asking.words
                ));
            }
            Ok(Err(e)) => self.say(format!("{e:#}")),
            Err(e) => self.say(format!("asking: {e}")),
        }
        self.refresh().await;
    }

    // ---- summarising -------------------------------------------------------

    /// `s` on an ended run: the assistant is asked what it did, as a run; the
    /// answer lands in the pane through the main loop.
    fn summarise(&mut self) {
        if let Some(s) = &self.summarising {
            self.say(format!("still summarising `{}`; x on its run stops it", s.title));
            return;
        }
        let Some(i) = self.selected_run() else { return };
        let run = &i.run;
        if !run.state.is_terminal() {
            self.say(format!("`{}` is still going; s when it has ended", run.title));
            return;
        }
        if matches!(run.cause, Cause::Assistant(_)) {
            self.say("an assistant's own run; nothing to summarise");
            return;
        }
        let (id, title) = (run.id.clone(), run.title.clone());
        let again = self.summary.as_ref().is_some_and(|s| s.run == id);
        let home = self.home.clone();
        let params = SummaryParams {
            run: id.clone(),
            again,
            timeout: None,
        };
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<SummaryMake>(params).await
        });
        self.show_pane = true;
        self.summarising = Some(Summarising { run: id, title, task });
    }

    async fn summarised(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
        let Some(s) = self.summarising.take() else { return };
        match done {
            Ok(Ok(summary)) => {
                self.say(format!("{} summarised `{}`", summary.agent, s.title));
                if self.selected_run().is_some_and(|i| i.run.id == s.run) {
                    self.summary = Some(summary);
                }
            }
            Ok(Err(e)) => self.say(format!("{e:#}")),
            Err(e) => self.say(format!("summarising: {e}")),
        }
        self.refresh().await;
    }

    // ---- keys --------------------------------------------------------------

    async fn key(&mut self, k: KeyEvent, area: Rect) -> Result<()> {
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
            Mode::Fire(name) => {
                if k.code == KeyCode::Char('y') {
                    self.fire(name).await;
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
                match k.code {
                    KeyCode::Esc => self.picked.clear(),
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = (c as u8 - b'1') as usize;
                        match (i < q.options.len(), q.multi) {
                            (true, true) => {
                                match self.picked.iter().position(|&p| p == i) {
                                    Some(at) => {
                                        self.picked.remove(at);
                                    }
                                    None => self.picked.push(i),
                                }
                                self.mode = Mode::Answer(id);
                            }
                            (true, false) => {
                                let key = q.options[i].key.clone();
                                self.answer(id, vec![key]).await;
                            }
                            (false, _) => self.mode = Mode::Answer(id),
                        }
                    }
                    KeyCode::Enter if q.multi && !self.picked.is_empty() => {
                        let opts: Vec<String> = self.picked.iter().map(|&i| q.options[i].key.clone()).collect();
                        self.answer(id, opts).await;
                    }
                    _ => self.mode = Mode::Answer(id),
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
            Mode::Normal => {}
        }
        // Joined, every key is the run's. Watching, the line still has them,
        // so you can type the next thing without leaving the run you are on.
        if self.pane == Pane::Term && self.term.as_ref().is_some_and(|t| t.joined() && t.ended.is_none()) {
            return self.key_term(k).await;
        }
        // An empty line has nothing to complete, so `tab` steps the lens along
        // instead — shift-tab the other way.
        if self.line.is_empty()
            && self.view == View::Work
            && self.pane != Pane::Term
            && matches!(k.code, KeyCode::Tab | KeyCode::BackTab)
        {
            self.cycle_lens(if k.code == KeyCode::BackTab { -1 } else { 1 });
            return Ok(());
        }
        // The line takes the keyboard first; what it leaves, the list has.
        match self.line.key(&k) {
            line::Act::Typing => {
                self.completing = None;
                Ok(())
            }
            line::Act::Complete => {
                self.tab();
                Ok(())
            }
            line::Act::Submit => self.submit(area).await,
            line::Act::Cancel if !self.line.is_empty() => {
                self.line.clear();
                self.completing = None;
                Ok(())
            }
            _ if self.pane == Pane::Term && self.term.is_some() => {
                let page = area.height.saturating_sub(3).max(1);
                self.watch_key(&k, page);
                Ok(())
            }
            _ => self.key_list(k, area).await,
        }
    }

    /// What the line does not take: moving about, and backing out.
    async fn key_list(&mut self, k: KeyEvent, area: Rect) -> Result<()> {
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => self.remove_selected().await,
            (KeyCode::Esc, _) => {
                if self.wide {
                    self.wide = false;
                } else if self.show_pane && area.width < SPLIT_AT {
                    self.show_pane = false;
                } else if self.view == View::Rules && self.rules_scope.is_some() {
                    self.rules_scope = None;
                    self.settle();
                } else if let Some(key) = self.selected_job().map(|j| j.key.clone())
                    && self.opened.remove(&key)
                {
                } else if let Some(id) = match self.selected() {
                    Some(Row::Fold(i)) => Some(i.run.id.clone()),
                    _ => None,
                } {
                    // Under an opened job: back up to the job, and fold it.
                    let keys = self.keys();
                    let at = keys.iter().position(|k| *k == format!("r:{id}")).unwrap_or(0);
                    let job = keys[..at].iter().rev().find(|k| k.starts_with("j:")).cloned();
                    if let Some(job) = job {
                        self.opened.remove(&job);
                        self.select(Some(job));
                    }
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.settle();
                }
            }
            (KeyCode::Down, _) => {
                self.step(1);
                self.saw_selected().await;
                self.refresh().await;
            }
            (KeyCode::Up, _) => {
                self.step(-1);
                self.saw_selected().await;
                self.refresh().await;
            }
            (KeyCode::Home, _) => {
                self.step(isize::MIN / 2);
                self.saw_selected().await;
                self.refresh().await;
            }
            (KeyCode::End, _) => {
                self.step(isize::MAX / 2);
                self.saw_selected().await;
                self.refresh().await;
            }
            (KeyCode::PageDown, _) => {
                self.step(self.list_h.get().saturating_sub(2).max(1) as isize);
                self.saw_selected().await;
                self.refresh().await;
            }
            (KeyCode::PageUp, _) => {
                self.step(-(self.list_h.get().saturating_sub(2).max(1) as isize));
                self.saw_selected().await;
                self.refresh().await;
            }
            _ => {}
        }
        Ok(())
    }

    /// `enter` on an empty line: whatever the row under the cursor is for.
    async fn enter_row(&mut self, area: Rect) -> Result<()> {
        // Watching a run: enter is what takes the keyboard.
        if self.pane == Pane::Term && self.term.as_ref().is_some_and(|t| !t.joined() && t.ended.is_none()) {
            let home = self.home.clone();
            if let Some(t) = &mut self.term {
                let take = t.busy_with.is_some();
                if let Err(e) = t.join(&home, take).await {
                    self.say(format!("join: {e:#}"));
                }
            }
            return Ok(());
        }
        if let Some(key) = self.selected_job().map(|j| j.key.clone()) {
            if !self.opened.remove(&key) {
                self.opened.insert(key);
            }
        } else if let Some(r) = self.selected_rule() {
            match r.kind() {
                RuleKind::Schedule if r.approved() => {
                    self.mode = Mode::Fire(ScheduleName {
                        name: r.name().into(),
                        scope: Some(r.scope().into()),
                    })
                }
                RuleKind::Schedule => self.say("not approved yet: /approve"),
                RuleKind::Reaction => self.say("a reaction fires on its event"),
            }
        } else if let Some(q) = self.selected_question() {
            let id = q.id.clone();
            self.picked.clear();
            // The options are numbered in the pane; asking for a number with
            // nothing numbered on screen is no question at all.
            self.show_pane = true;
            self.mode = Mode::Answer(id);
        } else {
            // Watch first — the next enter is what takes the keyboard.
            self.open_term(area, false).await;
        }
        Ok(())
    }

    /// Where the line is being typed: the folder of the row under the cursor,
    /// what agents there are, and $PATH.
    fn here<'a>(&'a self, folder: &'a Path) -> complete::Here<'a> {
        complete::Here {
            folder,
            agents: &self.agents,
            path_dirs: &self.path_dirs,
        }
    }

    /// `enter` on the line: what it says. Nothing typed, the row instead.
    async fn submit(&mut self, area: Rect) -> Result<()> {
        self.completing = None;
        let folder = self.folder_here();
        let what = {
            let here = self.here(&folder);
            line::what(self.line.text(), &self.agents, &|h| complete::runnable(h, &here))
        };
        match what {
            line::What::Empty => return self.enter_row(area).await,
            line::What::Slash(rest) => return self.command(&rest, area).await,
            line::What::Shell(cmd) => {
                self.line.clear();
                let d = form::RunDraft {
                    cwd: folder,
                    kind: RunKind::Command,
                    argv: vec!["sh".into(), "-c".into(), cmd],
                    title: None,
                };
                if let Err(e) = self.start_run(d, area).await {
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
                    self.ask(words);
                }
            }
        }
        Ok(())
    }

    /// `tab`: as much of the word as every candidate agrees on, then round
    /// the candidates one at a time.
    fn tab(&mut self) {
        let text = self.line.text().to_string();
        // A `/` line completes to the command it names.
        if let Some(rest) = text.strip_prefix('/') {
            if let Some(c) = palette::matching(rest).first() {
                let taken = format!("/{} ", c.name);
                let at = taken.len();
                self.line.set_with_cursor(taken, at);
            }
            self.completing = None;
            return;
        }
        // Tab again on what tab last wrote: the next one.
        if let Some(c) = self.completing.take()
            && c.shown == text
            && !c.cands.is_empty()
        {
            let at = (c.at + 1) % c.cands.len();
            let (l, pos) = complete::apply(&c.line, c.cursor, &c.cands[at].text);
            self.line.set_with_cursor(l.clone(), pos);
            self.completing = Some(Completing { shown: l, at, ..c });
            return;
        }
        let cur = self.line.cursor();
        let folder = self.folder_here();
        let cands = {
            let here = self.here(&folder);
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
    async fn command(&mut self, rest: &str, area: Rect) -> Result<()> {
        let Some((c, arg)) = palette::parse(rest) else {
            self.say(format!("no such command: /{rest} · /help lists them"));
            return Ok(());
        };
        self.line.clear();
        match c.cmd {
            palette::Cmd::Quit => self.quit = true,
            palette::Cmd::Help => {
                let names: Vec<&str> = palette::ALL.iter().map(|c| c.name).collect();
                self.say(format!("/{}", names.join(" · /")));
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
            palette::Cmd::Done => {
                if let Some(j) = self.selected_job() {
                    let ids: Vec<String> = j.runs.iter().map(|i| i.run.id.clone()).collect();
                    let what = format!("{} · {} runs", j.name, ids.len());
                    self.archive(ids, &what).await;
                } else if let Some(i) = self.selected_run() {
                    if i.run.state.is_terminal() {
                        let (id, title) = (i.run.id.clone(), i.run.title.clone());
                        self.archive(vec![id], &title).await;
                    } else {
                        self.say("still going: /stop first");
                    }
                }
            }
            palette::Cmd::DoneAll => self.archive_seen().await,
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
            palette::Cmd::Watch => {
                if self.selected_run().is_some() {
                    self.open_term(area, false).await;
                }
            }
            palette::Cmd::Join => {
                if self.selected_run().is_some() {
                    self.open_term(area, true).await;
                }
            }
            palette::Cmd::Info => self.show_pane = !self.show_pane,
            palette::Cmd::Full => {
                self.wide = !self.wide;
                if self.wide {
                    self.show_pane = true;
                }
            }
            palette::Cmd::Rules => {
                self.rules_scope = match self.view {
                    View::Work => self.selected_folder(),
                    View::Rules => None,
                };
                self.toggle_view().await;
            }
            palette::Cmd::Reload => self.reload_rules().await,
            palette::Cmd::Approve => {
                if let Some(r) = self.selected_rule()
                    && r.status() == RuleStatus::NeedsApproval
                {
                    let (name, scope, kind) = (r.name().to_string(), r.scope().to_string(), r.kind());
                    self.approve(name, scope, kind).await;
                } else {
                    self.say("no rule here waiting on you");
                }
            }
            palette::Cmd::Pause => {
                if let Some(r) = self.selected_rule() {
                    let (name, scope, kind, paused) =
                        (r.name().to_string(), r.scope().to_string(), r.kind(), r.paused());
                    self.set_paused(name, scope, kind, !paused).await;
                }
            }
            palette::Cmd::New => self.new_rule_form(),
            palette::Cmd::Edit => self.edit_rule_form(),
            palette::Cmd::Editor => self.edit_rule_file(),
            palette::Cmd::Summarise => self.summarise(),
            palette::Cmd::Ask => {
                if arg.trim().is_empty() {
                    self.say("/ask what? plain words, and the assistant drafts it");
                } else if self.asking.is_some() {
                    self.say("still asking; /stop on its run");
                } else {
                    self.ask(arg);
                }
            }
            palette::Cmd::Filter => {
                self.filter = arg.trim().to_string();
                self.settle();
            }
            palette::Cmd::LensAll => self.set_lens(rows::Lens::All),
            palette::Cmd::LensAgents => self.set_lens(rows::Lens::Agents),
            palette::Cmd::LensJobs => self.set_lens(rows::Lens::Jobs),
            palette::Cmd::LensReactions => self.set_lens(rows::Lens::Reactions),
            palette::Cmd::Gc => match self.rpc.call::<GcRun>(GcParams::default()).await {
                Ok(r) if r.runs == 0 => self.say(format!("nothing with a journal ended over {} ago", r.older_than)),
                Ok(r) => self.say(format!(
                    "dropped {} journal{} · {} · the runs and their events stay",
                    r.runs,
                    if r.runs == 1 { "" } else { "s" },
                    bytes_words(r.bytes)
                )),
                Err(e) => self.say(format!("gc: {e:#}")),
            },
        }
        Ok(())
    }

    /// Joined: every key is the run's, bar the one that lets go.
    async fn key_term(&mut self, k: KeyEvent) -> Result<()> {
        let Some(t) = &mut self.term else {
            self.close_term();
            return Ok(());
        };
        // ctrl-\ is byte 0x1c, which a plain terminal reports as ctrl-4.
        let back =
            matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) && k.modifiers.contains(KeyModifiers::CONTROL);
        if back {
            self.leave_term();
            return Ok(());
        }
        if let Some(bytes) = keys::encode(&k, t.app_cursor()) {
            t.input(bytes).await?;
        }
        Ok(())
    }

    /// Watching a run, or reading an ended one: the few keys the view keeps
    /// for itself. Everything else has already gone to the line.
    fn watch_key(&mut self, k: &KeyEvent, page: u16) -> bool {
        if self.term.is_none() {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let back = matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) && ctrl;
        if back || k.code == KeyCode::Esc {
            self.leave_term();
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

    // ---- drawing -----------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let form_h = match &self.mode {
            Mode::Form(x) => x.height(),
            _ => 0,
        };
        // A `/` line offers commands while it is typed; tab offers words.
        let slash: Option<Vec<&palette::Command>> = matches!(self.mode, Mode::Normal)
            .then(|| self.line.text().strip_prefix('/').map(palette::matching))
            .flatten();
        let cands_h = match (&slash, &self.completing) {
            (Some(cs), _) => cs.len().min(CANDS) as u16,
            (None, Some(c)) => c.cands.len().min(CANDS) as u16,
            (None, None) => 0,
        };
        let a = areas(f.area(), self.wide, self.show_pane, form_h, cands_h, self.lens_h());
        let rows = self.rows();
        let rule_rows = self.rule_rows();
        // News is counted the way it is shown: a job with unseen runs is one.
        let news = rows
            .iter()
            .filter(|r| match r {
                Row::Run(i) => i.unseen,
                Row::Job(j) => j.unseen(),
                _ => false,
            })
            .count();
        let mut title = match self.view {
            View::Work => rows::headline(&rows, news, &self.filter),
            View::Rules => rules::headline(&rule_rows, &self.filter),
        };
        if self.view == View::Rules
            && let Some(p) = &self.rules_scope
        {
            title.push_str(&format!(" · in {} · esc all", self.names.workspace(p)));
        }
        let [l, r] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(self.me.len() as u16 + 1)]).areas(a.head);
        // The lead word wears the accent; the tally after it reads plainly.
        let head = match title.split_once(" · ") {
            Some((lead, rest)) => Line::from(vec![
                Span::styled(lead.to_string(), theme::accent().add_modifier(Modifier::BOLD)),
                Span::raw(format!(" · {rest}")),
            ]),
            None => Line::from(Span::styled(title, theme::accent().add_modifier(Modifier::BOLD))),
        };
        f.render_widget(Paragraph::new(head), l);
        f.render_widget(Paragraph::new(self.me.clone()).dim().right_aligned(), r);
        if let Some(area) = a.lens {
            f.render_widget(Paragraph::new(self.lens_line()), area);
        }
        // The rules between the sections — a little air, a little structure.
        for rule in &a.rules {
            f.render_widget(Paragraph::new("─".repeat(rule.width as usize)).dim(), *rule);
        }
        if let Some(list) = a.list {
            match self.view {
                View::Work => self.draw_list(f, list, &rows),
                View::Rules => self.draw_rules(f, list, &rule_rows),
            }
            if a.pane.is_some() {
                let x = list.x + list.width;
                let bar = Rect::new(x, list.y, 1, list.height);
                f.render_widget(Paragraph::new("│\n".repeat(list.height as usize)).dim(), bar);
            }
        }
        if let Some(pane) = a.pane {
            self.draw_pane(f, pane);
        }
        if let (Some(area), Mode::Form(x)) = (a.form, &self.mode) {
            let mut lines = vec![Line::from(Span::raw("─".repeat(area.width as usize)).dim())];
            lines.extend(x.lines());
            f.render_widget(Paragraph::new(lines), area);
        }
        if let Some(area) = a.cands {
            let lines: Vec<Line> = match (&slash, &self.completing) {
                (Some(cs), _) => cs
                    .iter()
                    .take(area.height as usize)
                    .map(|c| {
                        let name = if c.args.is_empty() {
                            format!("   /{}", c.name)
                        } else {
                            format!("   /{} {}", c.name, c.args)
                        };
                        Line::from(vec![
                            Span::raw(format!("{name:<30}")),
                            Span::raw(c.what.to_string()).dim(),
                        ])
                    })
                    .collect(),
                (None, Some(c)) => c
                    .cands
                    .iter()
                    .take(area.height as usize)
                    .map(|x| Line::from(Span::raw(format!("   {}", x.text)).dim()))
                    .collect(),
                (None, None) => vec![],
            };
            f.render_widget(Paragraph::new(lines), area);
        }
        if matches!(self.mode, Mode::Normal) {
            f.render_widget(Paragraph::new(self.line_line(a.line.width)), a.line);
        }
        f.render_widget(Paragraph::new(format!(" {}", self.footer())).dim(), a.footer);
    }

    /// The lens strip under the header: the slices of the list, the one in
    /// force accented, the rest quiet. `tab` on an empty line steps along it.
    fn lens_line(&self) -> Line<'static> {
        let mut spans = vec![Span::raw(" ")];
        for (i, l) in rows::Lens::ALL.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", theme::dim()));
            }
            let style = if *l == self.lens {
                theme::accent().add_modifier(Modifier::BOLD)
            } else {
                theme::dim()
            };
            spans.push(Span::styled(l.label(), style));
        }
        spans.push(Span::styled("   tab", theme::dim()));
        Line::from(spans)
    }

    /// The live line: what is typed, where the cursor is, and — at the right
    /// edge, before you commit — what `enter` is about to do with it.
    fn line_line(&self, width: u16) -> Line<'static> {
        if self.line.is_empty() {
            return Line::from(vec![
                Span::styled(" › ", theme::accent()),
                Span::raw("what do you want done here? / for commands").style(theme::dim()),
            ]);
        }
        let folder = self.folder_here();
        let what = {
            let here = self.here(&folder);
            line::what(self.line.text(), &self.agents, &|h| complete::runnable(h, &here))
        };
        // A glyph and a colour say what `enter` will do: teal when it will run
        // something here, dim when the words are only asked or a command.
        let (glyph, tail, style) = match &what {
            line::What::Empty => ("↵", String::new(), theme::dim()),
            line::What::Slash(_) => ("↵", " a command".into(), theme::dim()),
            line::What::Shell(_) => (
                "❯",
                format!(" runs in {}", self.names.workspace(&folder)),
                theme::accent(),
            ),
            line::What::Agent { agent, .. } => ("✦", format!(" {agent} here"), theme::accent()),
            line::What::Ask(_) => ("…", " asks the assistant".into(), theme::dim()),
        };
        let hint = format!("{glyph}{tail}");
        let text = self.line.text();
        let (before, after) = text.split_at(self.line.cursor());
        let typed = format!(" › {before}▏{after}");
        let room = (width as usize).saturating_sub(typed.chars().count() + hint.chars().count() + 1);
        Line::from(vec![
            Span::raw(typed),
            Span::raw(" ".repeat(room)),
            Span::styled(hint, style),
        ])
    }

    fn draw_list(&self, f: &mut Frame, area: Rect, rows: &[Row]) {
        self.list_h.set(area.height);
        if rows.is_empty() {
            let msg = if !self.filter.is_empty() {
                " nothing matches".into()
            } else if self.lens != rows::Lens::All {
                format!(" no {} · tab for the rest", self.lens.label())
            } else {
                " nothing running · rl run -- <command>".into()
            };
            f.render_widget(Paragraph::new(msg).dim(), area);
            return;
        }
        let now = Timestamp::now();
        let w = area.width as usize;
        // Columns: mark, title, state, the summary to the edge, ref and age
        // at the right — the title column is the screen's share, bounded.
        let tw = (w * 35 / 100).clamp(14, 44);
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        let mut sel_line = 0usize;
        for (n, row) in rows.iter().enumerate() {
            let selected = row.key().is_some() && row.key() == self.sel;
            let mut line = match row {
                Row::Group(h) => {
                    if n > 0 {
                        lines.push(Line::raw(""));
                    }
                    let mut l = vec![Span::raw(" ")];
                    let mut used = 1;
                    // The name reads first; the folder it lives in follows, quiet.
                    l.push(Span::styled(h.name.clone(), theme::name()));
                    used += h.name.chars().count();
                    if let Some(p) = &h.place {
                        l.push(Span::styled(format!("  {p}"), theme::dim()));
                        used += 2 + p.chars().count();
                    }
                    // The count of what is set up here sits at the right edge and
                    // is the honest total even when few chips fit.
                    let count = group_count(&h.rules);
                    let cw = if count.is_empty() { 0 } else { count.chars().count() + 1 };
                    // The rules as chips, just after the name — what needs a yes
                    // first (they are already sorted so). As many as fit; a `+K`
                    // stands in for the rest so the clip is never silent.
                    let texts: Vec<String> = h.rules.iter().map(rows::Chip::text).collect();
                    let width = |t: &[String]| {
                        t.iter().map(|x| x.chars().count()).sum::<usize>() + 3 * t.len().saturating_sub(1)
                    };
                    let mut show = texts.len();
                    let more = |shown: usize, total: usize| {
                        if shown < total {
                            format!(" +{}", total - shown).chars().count()
                        } else {
                            0
                        }
                    };
                    while show > 0 && used + 3 + width(&texts[..show]) + more(show, texts.len()) + cw + 2 > w {
                        show -= 1;
                    }
                    if show > 0 || !texts.is_empty() {
                        l.push(Span::raw("   "));
                        for (i, (c, t)) in h.rules.iter().zip(&texts[..show]).enumerate() {
                            if i > 0 {
                                l.push(Span::styled(" · ", theme::dim()));
                            }
                            l.push(match c.status {
                                RuleStatus::NeedsApproval => Span::styled(t.clone(), theme::needs()),
                                RuleStatus::Paused => Span::styled(t.clone(), theme::dim()),
                                _ => Span::styled(t.clone(), theme::accent()),
                            });
                        }
                        if show < texts.len() {
                            l.push(Span::styled(format!(" +{}", texts.len() - show), theme::dim()));
                        }
                    }
                    if !count.is_empty() {
                        let used_now: usize = l.iter().map(|s| s.content.chars().count()).sum();
                        let pad = w.saturating_sub(used_now + count.chars().count() + 1).max(1);
                        l.push(Span::raw(" ".repeat(pad)));
                        l.push(Span::styled(count, theme::dim()));
                    }
                    Line::from(l)
                }
                Row::Question(q) => {
                    let summary = q.summary.clone().unwrap_or_else(|| {
                        q.options
                            .iter()
                            .map(|o| o.label.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    });
                    list_line(
                        w,
                        tw,
                        Span::styled(" ✱ ", theme::needs()),
                        Span::styled(fit(&q.title, tw), theme::name()),
                        Span::styled(fit("to decide", 10), theme::needs()),
                        &summary,
                        None,
                        None,
                        fmt_age(q.created_at, now),
                    )
                }
                Row::Run(i) | Row::Fold(i) => {
                    let r = &i.run;
                    let under = matches!(row, Row::Fold(_));
                    let style = state_style(r.state);
                    let title = if under {
                        format!("  {}", r.title)
                    } else {
                        r.title.clone()
                    };
                    let title = Span::raw(fit(&title, tw));
                    let title = if under {
                        title.dim()
                    } else if i.unseen {
                        title.bold()
                    } else {
                        title
                    };
                    list_line(
                        w,
                        tw,
                        Span::styled(format!(" {} ", rows::glyph(r.state)), style),
                        title,
                        Span::styled(fit(&rows::state_words(r.state, r.confidence), 10), style),
                        &i.summary,
                        i.joined_by.as_deref(),
                        r.ref_.as_deref(),
                        fmt_age(i.at, now),
                    )
                }
                Row::Job(j) => {
                    let last = j.last();
                    let open = if self.opened.contains(&j.key) { "▾" } else { "▸" };
                    let word = match last.run.state {
                        RunState::Failed => "failed",
                        RunState::Stopped => "stopped",
                        _ => "ok",
                    };
                    let title = Span::raw(fit(&j.name, tw));
                    let title = if j.unseen() { title.bold() } else { title };
                    list_line(
                        w,
                        tw,
                        Span::raw(format!(" {open} ")),
                        title,
                        Span::styled(fit(word, 10), state_style(last.run.state)),
                        &j.summary(),
                        None,
                        None,
                        fmt_age(last.at, now),
                    )
                }
            };
            if selected {
                line = line.style(theme::selected());
                sel_line = lines.len();
            }
            lines.push(line);
        }
        // The window follows the selection; when there is more either side,
        // the edge line says how much.
        let h = area.height as usize;
        let n = lines.len();
        if h == 0 {
            return;
        }
        let top = viewport(n, h, sel_line, self.scroll.get());
        self.scroll.set(top);
        let more_below = top + h < n;
        let mut shown: Vec<Line> = lines[top..(top + h).min(n)].to_vec();
        if top > 0 {
            shown[0] = Line::from(Span::raw(format!(" ▲ {top} more")).dim());
        }
        if more_below {
            let left = n - (top + h);
            let i = shown.len() - 1;
            shown[i] = Line::from(Span::raw(format!(" ▼ {left} more")).dim());
        }
        f.render_widget(Paragraph::new(shown), area);
    }

    fn draw_rules(&self, f: &mut Frame, area: Rect, rows: &[RuleRow]) {
        if rows.is_empty() {
            let msg = if self.filter.is_empty() {
                " no rules · rl on <event> -- <command>, or rl up in a folder with .relay/"
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
                RuleRow::Group(g) => {
                    if n > 0 {
                        lines.push(Line::raw(""));
                    }
                    Line::from(Span::raw(format!(" {g}")).bold())
                }
                RuleRow::Rule(r) => {
                    let status = rules::status_word(r, now);
                    let status = match r.status() {
                        RuleStatus::NeedsApproval => Span::raw(format!(" ! {status:<14}")).yellow().bold(),
                        RuleStatus::Paused => Span::raw(format!("   {status:<14}")).dim(),
                        _ => Span::raw(format!("   {status:<14}")),
                    };
                    Line::from(vec![
                        status,
                        Span::raw(format!(" {:<18}", r.name())),
                        Span::raw(format!(" {:<16}", r.when())).dim(),
                        Span::raw(format!(" {}", r.does())).dim(),
                    ])
                }
            };
            if selected {
                line = line.style(theme::selected());
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
        if let Some(r) = self.selected_rule() {
            let last = r.last_run().and_then(|id| self.run_by_id(id)).map(|x| x.title.as_str());
            let lines = rules::lines(r, &self.names, now, last);
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        let lines = match self.selected() {
            Some(Row::Run(i)) | Some(Row::Fold(i)) => pane::run_lines(
                i,
                &self.events,
                self.summary.as_ref().filter(|s| s.run == i.run.id),
                &self.names,
                now,
            ),
            Some(Row::Job(j)) => pane::job_lines(&j, self.rule_of_job(&j), &self.names, now),
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
            Mode::Fire(n) => return format!("run {} now? y / any key", n.name),
            Mode::Mark(id) => return format!("mark {}: d done · b blocked · w working · esc", title_of(id)),
            Mode::Answer(id) => {
                let q = self.digest.decisions.iter().find(|d| &d.id == id);
                let n = q.map_or(0, |q| q.options.len().min(pane::MAX_KEYS));
                let how = if q.is_some_and(|q| q.multi) {
                    "pick · enter answers"
                } else {
                    "answers it"
                };
                return format!("1-{n} {how} · esc");
            }
            Mode::Form(f) => {
                if let Some((m, at)) = &self.flash
                    && at.elapsed() < FLASH
                {
                    return m.clone();
                }
                return if f.on_folders() {
                    "↑↓ another folder · type a new one · tab next field · enter save · esc cancel".into()
                } else {
                    "enter save · tab next field · esc cancel".into()
                };
            }
            Mode::Normal => {}
        }
        if let Some((m, at)) = &self.flash
            && at.elapsed() < FLASH
        {
            return m.clone();
        }
        if let Some(a) = &self.asking {
            return format!("asking about \"{}\"… · its run is in the list · /stop there", a.words);
        }
        if let Some(s) = &self.summarising {
            return format!("summarising `{}`… · its run is in the list · /stop there", s.title);
        }
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            let mut s = String::new();
            if t.ended.is_some() {
                s.push_str("ended · pgup/pgdn reads back · esc back");
            } else if t.joined() {
                s.push_str("joined · ctrl-\\ back");
            } else if let Some(who) = &t.busy_with {
                s.push_str(&format!("watching · {who} is joined · enter takes over · esc back"));
            } else if let Some(who) = &t.info.joiner {
                s.push_str(&format!("watching · {who} is joined · enter joins · esc back"));
            } else {
                s.push_str("watching · enter joins · pgup/pgdn reads back · esc back");
            }
            if t.scrolled() > 0 {
                s.push_str(&format!(" · {} back", t.scrolled()));
            }
            if let Some((n, at)) = &t.note
                && at.elapsed() < FLASH
            {
                s.push_str(&format!(" · {n}"));
            }
            return s;
        }
        if let Some(r) = self.selected_rule() {
            let mut s = String::new();
            if r.status() == RuleStatus::NeedsApproval {
                s.push_str("/approve · ");
            }
            s.push_str(if r.paused() { "/pause lets it run" } else { "/pause" });
            if r.kind() == RuleKind::Schedule {
                s.push_str(" · enter runs it now");
            }
            s.push_str(" · /edit · /editor · /new · /reload · /rules back");
            return s;
        }
        if self.view == View::Rules {
            return "/new a rule · /reload · /rules back · or type something to run here".into();
        }
        let pane = if self.show_pane {
            "/info hides details"
        } else {
            "/info details"
        };
        match self.selected() {
            Some(Row::Question(_)) => format!("enter answers it · ^x dismiss · {pane}"),
            Some(Row::Job(j)) => {
                let open = if self.opened.contains(&j.key) {
                    "enter closes"
                } else {
                    "enter opens"
                };
                format!("{open} · /edit its rule · ^x done · {pane}")
            }
            Some(Row::Fold(_)) => format!("enter output · ^x done · esc folds · {pane}"),
            Some(Row::Run(i)) if i.run.state.is_terminal() => {
                format!("enter output · /summarise · ^x done · /done-all · {pane}")
            }
            Some(Row::Run(_)) => format!("enter watches it · /join · ^x stops it · /mark · {pane}"),
            _ => String::new(),
        }
    }
}

/// A group header's right-edge tally: how many schedules and reactions the
/// folder has, in words, dropping a kind that has none and empty when it has
/// neither. Schedules read as "jobs", the word the list uses for their runs.
fn group_count(rules: &[rows::Chip]) -> String {
    let jobs = rules.iter().filter(|c| c.kind == RuleKind::Schedule).count();
    let reactions = rules.iter().filter(|c| c.kind == RuleKind::Reaction).count();
    let mut parts = Vec::new();
    if jobs > 0 {
        parts.push(format!("{jobs} job{}", if jobs == 1 { "" } else { "s" }));
    }
    if reactions > 0 {
        parts.push(format!("{reactions} reaction{}", if reactions == 1 { "" } else { "s" }));
    }
    parts.join(" · ")
}

/// Bytes in words, near enough for a line of chrome.
fn bytes_words(n: u64) -> String {
    const K: f64 = 1024.0;
    let x = n as f64;
    if x < K {
        format!("{n} B")
    } else if x < K * K {
        format!("{:.0} KB", x / K)
    } else if x < K * K * K {
        format!("{:.1} MB", x / (K * K))
    } else {
        format!("{:.1} GB", x / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    use super::{group_count, viewport};
    use crate::tui::rows::Chip;
    use relay_proto::rule::{RuleKind, RuleStatus};

    fn chip(kind: RuleKind) -> Chip {
        Chip {
            name: "x".into(),
            kind,
            status: RuleStatus::On,
            due: None,
        }
    }

    #[test]
    fn the_header_count_drops_a_kind_with_none_and_pluralises() {
        use RuleKind::*;
        assert_eq!(group_count(&[]), "");
        assert_eq!(group_count(&[chip(Schedule)]), "1 job");
        assert_eq!(group_count(&[chip(Reaction)]), "1 reaction");
        assert_eq!(
            group_count(&[chip(Schedule), chip(Schedule), chip(Reaction)]),
            "2 jobs · 1 reaction"
        );
    }

    #[test]
    fn the_window_keeps_the_selection_clear_of_the_edge_markers() {
        // Everything fits: no window.
        assert_eq!(viewport(10, 20, 9, 0), 0);
        // Selection at the top: no marker above, so line 0 shows.
        assert_eq!(viewport(50, 10, 0, 0), 0);
        // Down to the last usable line (the last of the window is the marker).
        assert_eq!(viewport(50, 10, 8, 0), 0);
        assert_eq!(viewport(50, 10, 9, 0), 1, "top 1 shows 1..=10; 1 and 10 are markers");
        assert_eq!(viewport(50, 10, 10, 1), 2);
        // The window stays put while the selection is inside it.
        assert_eq!(viewport(50, 10, 5, 2), 2);
        // Up under the top marker pulls the window up.
        assert_eq!(viewport(50, 10, 2, 2), 1);
        assert_eq!(viewport(50, 10, 1, 1), 0);
        // At the very end there is no bottom marker, so the last line is usable.
        assert_eq!(viewport(50, 10, 49, 0), 40);
        assert_eq!(viewport(50, 10, 41, 40), 40);
        // Too short for markers: just the lines.
        assert_eq!(viewport(50, 2, 30, 0), 0);
    }
}
