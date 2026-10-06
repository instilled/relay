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

mod catchup;
mod complete;
mod events;
pub mod form;
mod graph;
mod input;
mod line;
mod mark;
mod palette;
mod pane;
mod query;
mod remove;

pub use pane::{recap_label, summary_label};
pub(crate) mod rows;
mod rules;
mod screen;
mod setup;
mod shortcuts;
mod term;
mod theme;

use crate::attach::client_name;
use crate::client::Client;
use crate::rulefile;
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, EventStream, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::supports_keyboard_enhancement;
use form::{Act, Form};
use futures::StreamExt;
use jiff::{SignedDuration, Timestamp};
use pane::state_style;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rl_core::Home;
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::methods::{
    AgentList, DecisionDismiss, DecisionGet, DecisionId, DecisionNote, DecisionTake, DecisionTakeParams, Digest,
    DigestGet, DigestHide, DigestHideParams, DigestItem, DigestOrder, DigestParams, EventFilter, EventList, GcParams,
    GcRun, GraphGet, GraphParams, ImportList, ImportListParams, PresenceClient, PresenceTouch, ReactionPause,
    ReactionReload, ReactionResume, RuleApprove, RuleErrors, RuleList, RuleRef, RunArchive, RunArchiveParams,
    RunFilter, RunGet, RunHide, RunHideParams, RunId, RunIds, RunInput, RunInputParams, RunList, RunMark,
    RunMarkParams, RunPin, RunPinParams, RunRename, RunRenameParams, RunResume, RunResumeParams, RunSeen, RunSpec,
    RunStart, RunStop, RunStopParams, SETUP_STEPS, ScheduleFilter, ScheduleName, SchedulePause, ScheduleReload,
    ScheduleResume, ScheduleRun, SetupGet, SummaryGet, SummaryMake, SummaryParams, WorkspaceList, WorkspaceRemove,
    WorkspaceRemoveParams, WorkspaceRemoved, WorkspaceRename, WorkspaceRenameParams, WorkspaceUp, WorkspaceUpParams,
};
use rl_proto::rule::{NotLoaded, Rule, RuleKind, RuleStatus};
use rl_proto::types::{AttachMode, Cause, Event as LedgerEvent, Mark, Run, RunKind, RunState, Summary, SummaryTarget};
use rows::{Names, Row, fit, short_path};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use term::TermView;

const REFRESH: Duration = Duration::from_secs(1);
/// Footer messages fade after this.
const FLASH: Duration = Duration::from_secs(4);
/// The most candidates `tab` shows at once.
const CANDS: usize = 6;
use rows::SHELL_TAG;
/// How many runs, or questions, the events view asks the daemon for one at
/// a time in a tick; more than this at once, it reads a page of runs.
const EVENT_ASKS: usize = 40;
/// The most rows the live line grows to before it scrolls within itself.
const LINE_MAX: u16 = 8;
/// Recent output replayed when joining a run for real, enough to paint the
/// screen on arrival. Bytes.
const JOIN_SCROLLBACK: u64 = 256 * 1024;
/// Where the header and footer text starts: level with what is typed on the
/// line, past its `❯ `.
const TEXT_INDENT: u16 = 2;
/// Narrower than this, the pane takes the screen instead of sharing it.
const SPLIT_AT: u16 = 100;
/// The tall header — the mark, and three lines beside it — wants a screen
/// at least this tall and this wide; on a smaller one the header is one line.
const TALL_H: u16 = 24;
const TALL_W: u16 = 80;
/// The air between the header's mark and its words.
const MARK_GAP: u16 = 3;
/// The air above the tall header, so the mark does not sit on the screen's
/// top edge.
const HEAD_AIR: u16 = 1;
/// The list's share of a split screen, in percent: two thirds beside a
/// details pane, half beside a terminal, which needs the width.
const LIST_SHARE: u16 = 67;
const LIST_SHARE_TERM: u16 = 50;

/// What the right pane shows for a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Facts,
    Term,
}

/// Which list the left side shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum View {
    Work,
    /// The wiring graph, over every project.
    Graph,
    /// Every event, newest first, and what each set off (`/events`).
    Events,
    /// What happened while you were away: what the panel opens on after a
    /// while gone (`catch_up.after`).
    CatchUp,
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
    /// A join refused because someone holds the keyboard: `y` takes it from
    /// them. The run, and who has it.
    Take(String, String),
    /// The second `^x` on a project with a `.relay` that can move: `y` moves
    /// it, `n` keeps it, and either removes the project; anything else
    /// leaves everything as it was.
    Remove(Box<Removal>),
    /// `^r`: a run's or a project's name being changed where it stands, in
    /// the list.
    Rename(Box<Renaming>),
    /// `/summary deep` over more than one assistant call: how many runs it
    /// reads and how many calls that takes. `y` asks (`App::summary_ask`),
    /// anything else lets it be.
    SummaryCost {
        runs: u32,
        calls: u32,
    },
}

/// A name being changed in place: the row it is on, what it is the name
/// of, and what is typed so far.
struct Renaming {
    /// The row's key: moving off the row lets the name go.
    key: String,
    what: Renamed,
    /// The name as it was, so one left as it was is not saved.
    was: String,
    field: line::Field,
    /// The first column of the name its cell shows, for a name wider than
    /// the cell: it moves only as far as keeps the cursor in view.
    from: std::cell::Cell<usize>,
}

/// What a name belongs to: a run by its id, a project by its folder.
enum Renamed {
    Run(String),
    Project(PathBuf),
}

/// A project removal waiting on the question about its `.relay`.
struct Removal {
    folder: PathBuf,
    name: String,
    question: String,
}

/// A project being removed, until the daemon answers: stopping its runs can
/// take a few seconds, and the panel goes on meanwhile.
struct Removing {
    name: String,
    fate: remove::RulesFate,
    task: tokio::task::JoinHandle<Result<WorkspaceRemoved>>,
}

/// The line searching rather than taking work: what is typed is the filter,
/// live. Whatever was being typed for work waits in the main line.
struct Search {
    line: line::Line,
    /// The filter as it stood when the search opened, for `esc` to put back.
    before: String,
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

/// The kind of candidate list under the line, and how many it holds.
#[derive(Clone, Copy)]
enum Cands {
    /// The `/` commands the line names.
    Slash(usize),
    /// The completion words `tab` laid out.
    Words(usize),
}

impl Cands {
    fn len(self) -> usize {
        match self {
            Cands::Slash(n) | Cands::Words(n) => n,
        }
    }
}

/// Words handed to the assistant, until it answers.
struct Asking {
    words: String,
    folder: PathBuf,
    editing: Option<Rule>,
    task: tokio::task::JoinHandle<Result<(crate::interpret::Draft, rl_proto::methods::AssistantOutput)>>,
}

/// A run, a job, a project or everything being summarised, until the
/// assistant answers.
struct Summarising {
    target: SummaryTarget,
    /// What it is of, in words: the run's title, `schedule nightly`, the
    /// project's name, `everything`.
    title: String,
    deep: bool,
    from: Option<Timestamp>,
    task: tokio::task::JoinHandle<Result<Summary>>,
}

/// The catch-up's top as the cursor knows it: above its first project, where
/// `/summary` is of everything in the stretch.
const TOP: &str = "top";

/// What the catch-up showed that was not seen yet: the runs, and where each
/// job's firings not seen start, by its row key.
#[derive(Default)]
struct Shown {
    ids: Vec<String>,
    from: Vec<(String, Timestamp)>,
}

struct App {
    home: Home,
    rpc: Client,
    me: String,
    digest: Digest,
    names: Names,
    /// A search the list and the graph are narrowed to; see `query`.
    filter: String,
    /// `^f`: the line is searching, and the filter follows what is typed.
    search: Option<Search>,
    /// Which slice of the list is shown: all, agents, jobs or reactions.
    lens: rows::Lens,
    view: View,
    /// Every rule: for the chips on project rows, a job's rule, and the graph's nodes.
    rules: Vec<Rule>,
    /// What did not load at each rules file's last load: a red `! rules` on
    /// its header, the headline's `rules broken` for the global ones.
    not_loaded: Vec<NotLoaded>,
    /// What the last `/reload` did not load, when it was more than the flash
    /// has room for: listed in the pane until the next key.
    reload_list: Option<Vec<NotLoaded>>,
    /// The wiring, fetched while the graph is up.
    graph: Option<rl_proto::graph::Graph>,
    /// The views gone through to reach the one that is up, the list first:
    /// going back walks them.
    back: Vec<View>,
    /// Each view's selection while another is up.
    kept: HashMap<View, Option<String>>,
    /// The events view's search while another view is up, or the list's
    /// and the graph's while it is: `kind:` means another thing there.
    filter_aside: String,
    /// What the events view has read of the ledger.
    feed: events::Feed,
    /// Where the window over the graph starts, column and row.
    graph_at: std::cell::Cell<(usize, usize)>,
    /// Which of a folder's shells a shell line goes to, and the one the
    /// arrows step from. A folder is absent until you step; the entry is
    /// ignored once that shell has ended.
    shell_at: HashMap<PathBuf, String>,
    /// First list line on screen, and how many fit: the scroll map. Cells,
    /// because drawing sets them while the rows still borrow the app.
    scroll: std::cell::Cell<usize>,
    list_h: std::cell::Cell<u16>,
    /// What the list drew last, so the mouse can tell which row it is over.
    hit: std::cell::RefCell<ListHit>,
    /// The selected row's key (`r:<id>` or `q:<id>`); the cursor follows it
    /// across re-ranks.
    sel: Option<String>,
    pane: Pane,
    /// The pane beside the list (`i`); off, the list has the width.
    show_pane: bool,
    /// The pane was opened by selecting a question, and goes when the cursor
    /// moves off it; opened by hand, it stays.
    auto_pane: bool,
    /// The highlighted choice of the question being answered.
    answer_at: usize,
    /// What the question pane drew last, so a click can take a choice.
    pane_hit: std::cell::RefCell<PaneHit>,
    /// The pane alone, full width: a terminal, unless `f` says otherwise.
    wide: bool,
    term: Option<TermView>,
    /// Size the terminal was last told about.
    term_size: (u16, u16),
    /// The selected run's recent events.
    events: Vec<LedgerEvent>,
    /// The selected row's summary, when there is one kept.
    summary: Option<Summary>,
    /// The assistant at work on a summary (`/summary`).
    summarising: Option<Summarising>,
    /// The assistant at work on the catch-up's top, apart from `/summary`
    /// so a row's can be asked for meanwhile.
    topping: Option<Summarising>,
    /// The catch-up's top as written, for the stretch it shows; until there
    /// is one, it is counted.
    top: Option<Summary>,
    /// `/summary deep` waiting on `y`: what it would ask.
    summary_ask: Option<SummaryParams>,
    /// The last summary made of each target (`SummaryTarget::key`) while the
    /// panel is open: which stretch and depth the pane asks the kept one
    /// for, and what it shows if none is kept.
    made: HashMap<String, Summary>,
    /// When you were last here before this open: where a project's or
    /// everything's `/summary` starts with no stretch typed. None: the last
    /// day.
    away_since: Option<Timestamp>,
    /// When you were last here before this open, as presence said: where a
    /// bare `/since` starts. None: the first open.
    left: Option<Timestamp>,
    /// The catch-up's stretch, read while it is up or on the way back to
    /// it: the list's digest counted from when you left. The list's own
    /// stays as it is.
    catch: Option<Digest>,
    /// Where a job row's `/summary` starts with no stretch typed, by row key:
    /// when its oldest firing not seen started, noted as the cursor lands on
    /// it — landing marks them seen, so the daemon could no longer tell.
    /// Kept until used, or until the row has firings not seen again.
    unseen_from: HashMap<String, Timestamp>,
    /// A project on its way out (`^x` twice on its header).
    removing: Option<Removing>,
    /// `/edit`: the rule file and line to open in the editor once the
    /// screen is given back.
    editor: Option<(PathBuf, usize)>,
    /// A run to hand the real terminal to (its id, and whether to take the
    /// keyboard from whoever holds it), once ratatui has stepped aside.
    attach: Option<(String, bool)>,
    /// `/import`: the Claude sessions step, once the screen is free for it.
    importing: bool,
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
    /// The shell's own names — aliases and functions — read once at startup,
    /// so a `ll` reads as a command to run, not words for the assistant.
    aliases: Vec<String>,
    completing: Option<Completing>,
    /// Which candidate the arrows have walked to, when a list is on offer.
    /// `None` until you press an arrow; typing clears it. When set, `enter`
    /// and `tab` take this one rather than submitting or cycling.
    pick: Option<usize>,
    mode: Mode,
    flash: Option<(String, Instant)>,
    /// A destructive key waits for its own second press: `ctrl-x` to remove,
    /// `ctrl-c` to quit. Any other key clears it, so nothing goes on one slip.
    arm: Option<Arm>,
    /// `?` on an empty line: the keys for where you are, under the line,
    /// until `?` again, `esc`, or typing.
    shortcuts: bool,
    quit: bool,
}

/// What a second press of a destructive key would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Remove,
    Quit,
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

/// The first candidate to show, so the arrowed-to one is always on screen.
/// Walking down past the last visible row slides the window by one; wrapping
/// back to the top brings it home.
fn cand_window(n: usize, h: usize, pick: Option<usize>) -> usize {
    if h == 0 || n <= h {
        return 0;
    }
    match pick {
        Some(i) => i.saturating_sub(h - 1).min(n - h),
        None => 0,
    }
}

/// The question pane as drawn: which question, where, and the choice on each
/// row.
#[derive(Default)]
struct PaneHit {
    area: Rect,
    question: String,
    hits: Vec<Option<usize>>,
}

impl PaneHit {
    fn choice(&self, col: u16, row: u16) -> Option<usize> {
        let a = self.area;
        if col < a.x || col >= a.x + a.width || row < a.y || row >= a.y + a.height {
            return None;
        }
        self.hits.get((row - a.y) as usize).copied().flatten()
    }
}

/// Where the list last drew, so a click can find its row: the area it filled,
/// how far it was scrolled, and the row key each of its lines carries — `None`
/// for the blank line a group leaves above itself.
#[derive(Default)]
struct ListHit {
    area: Rect,
    scroll: usize,
    keys: Vec<Option<String>>,
}

impl ListHit {
    fn has(&self, col: u16, row: u16) -> bool {
        col >= self.area.x
            && col < self.area.x + self.area.width
            && row >= self.area.y
            && row < self.area.y + self.area.height
    }
}

/// The row key on the line under a pointer, if the list drew one there.
fn hit_row(hit: &ListHit, col: u16, row: u16) -> Option<String> {
    if !hit.has(col, row) {
        return None;
    }
    let line = hit.scroll + (row - hit.area.y) as usize;
    hit.keys.get(line).cloned().flatten()
}

/// The icon slot: a space, a one-column icon, a space. Every row has one, so
/// every title starts in the same column.
const ICON_W: usize = 3;
/// The state column's width: `needs you~` and a job's marks both fit.
const WORD_W: usize = 10;

/// Where the state column starts, for a title column `tw` wide — where a
/// header's checkout starts too, so the two line up.
fn state_col(tw: usize) -> usize {
    ICON_W + tw + 1
}

/// A row's icon, in its slot and its colour; `ended`, quiet.
fn icon_span(icon: &str, ended: bool) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled(icon.to_string(), rows::icon_style(icon, ended)),
        Span::raw(" "),
    ]
}

/// A name being typed, in its cell `w` wide, having last shown it from
/// column `from`: the part of it the cursor keeps in view, the caret where
/// the cursor is, and all of it lit while it is selected — padded out to the
/// cell. And the column it shows from now.
fn name_cell(field: &line::Field, from: usize, w: usize) -> (Vec<Span<'static>>, usize) {
    if w == 0 {
        return (vec![], 0);
    }
    let from = field.window(from, w);
    let text = field.text();
    let at = |col: usize| text.char_indices().nth(col).map_or(text.len(), |(i, _)| i);
    let (start, end) = (at(from), at(from + w));
    let style = if field.is_selected() {
        theme::picked()
    } else {
        Style::default()
    };
    let mut spans = theme::with_caret(&text[start..end], field.cursor() - start, style);
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
    (spans, from)
}

/// One row of the list, to the width: icon, title, state, whatever goes
/// before the summary (who is joined), the summary filling the middle, then
/// the ref or branch and the age at the right.
#[allow(clippy::too_many_arguments)]
fn list_line(
    w: usize,
    tw: usize,
    icon: Vec<Span<'static>>,
    title: Vec<Span<'static>>,
    word: Vec<Span<'static>>,
    summary: &str,
    lead: Vec<Span<'static>>,
    aside: Option<&str>,
    age: String,
) -> Line<'static> {
    let right = match aside {
        Some(r) => format!("{r}  {age:>4}"),
        None => format!("{age:>4}"),
    };
    let before: usize = lead.iter().map(Span::width).sum();
    let fixed = state_col(tw) + WORD_W + 1 + before + right.chars().count() + 1;
    // The word, padded out to its column whatever it is made of.
    let used: usize = word.iter().map(Span::width).sum();
    let mut spans = icon;
    spans.extend(title);
    spans.push(Span::raw(" "));
    spans.extend(word);
    spans.push(Span::raw(" ".repeat(WORD_W.saturating_sub(used) + 1)));
    spans.extend(lead);
    spans.push(Span::raw(fit(summary, w.saturating_sub(fixed))).dim());
    spans.push(Span::raw(format!(" {right}")).dim());
    Line::from(spans)
}

/// How many of a job's firings the row shows as marks.
const ROW_MARKS: usize = 5;

/// Who is joined, for the slot before a row's summary.
fn joined_lead(joined: Option<&str>) -> Vec<Span<'static>> {
    joined
        .map(|j| vec![Span::raw(format!("● {j}  ")).dim()])
        .unwrap_or_default()
}

/// How a job has been going, oldest first, for its state column: `···×·`.
fn job_marks(j: &rows::Job) -> Vec<Span<'static>> {
    j.marks(ROW_MARKS)
        .chars()
        .map(|c| match c {
            '×' => Span::raw("×").red().bold(),
            '○' => Span::raw("○").dim(),
            _ => Span::raw("·").dim(),
        })
        .collect()
}

/// A group's header, to the width: its place in the title column — the
/// folder bold, what it is in dim, the name it goes by after — then its git
/// checkout from the state column on, and at the right edge the tally of its
/// rules, with any that wait for a yes just before it, in amber. Each piece
/// has its column, whatever the others say.
fn header_line(h: &rows::Head, w: usize, tw: usize, broken: bool) -> Line<'static> {
    let (parent, last, own) = rows::place_parts(&h.place, h.own.as_deref(), place_w(tw));
    let place = vec![
        Span::styled(parent, theme::dim()),
        Span::styled(last, theme::name()),
        Span::styled(own, theme::dim()),
    ];
    header_with(place, h, w, tw, broken)
}

/// The room a header's place has: up to the checkout's column, and a gap.
fn place_w(tw: usize) -> usize {
    state_col(tw) - 2
}

/// A header with `place` where its folder goes — the name being typed, when
/// it is renamed where it stands.
fn header_with(place: Vec<Span<'static>>, h: &rows::Head, w: usize, tw: usize, broken: bool) -> Line<'static> {
    let col = state_col(tw);
    let mut l = place;
    let width = |l: &[Span]| l.iter().map(Span::width).sum::<usize>();
    // The tally at the right edge is the honest total; a rule that waits for a
    // yes is named beside it, since that one is yours to give.
    let mut right: Vec<Span<'static>> = Vec::new();
    // A rules file here that did not load comes before anything waiting for
    // a yes, in red: what is loaded is not what the file says.
    if broken {
        right.push(Span::styled(rows::BROKEN_CHIP, theme::bad()));
        right.push(Span::raw("  "));
    }
    for c in h.rules.iter().filter(|c| c.needs_yes()) {
        right.push(Span::styled(c.text(), theme::needs()));
        right.push(Span::raw("  "));
    }
    let count = group_count(&h.rules);
    if count.is_empty() {
        right.pop();
    } else {
        right.push(Span::styled(count, theme::dim()));
    }
    if let Some(c) = &h.checkout {
        let at = col.max(width(&l) + 2);
        l.push(Span::raw(" ".repeat(at - width(&l))));
        let room = w.saturating_sub(at + width(&right) + usize::from(!right.is_empty()) * 2);
        let git = fit(&rows::checkout_words(c), room);
        l.push(Span::styled(git.trim_end().to_string(), theme::dim()));
    }
    if !right.is_empty() {
        let pad = w.saturating_sub(width(&l) + width(&right)).max(1);
        l.push(Span::raw(" ".repeat(pad)));
        l.extend(right);
    }
    Line::from(l)
}

/// A line with the search's words lit up, wherever a span holds them.
fn light(line: Line<'static>, words: &[String]) -> Line<'static> {
    let mut out = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        let marks = query::marks(&span.content, words);
        if marks.is_empty() {
            out.push(span);
            continue;
        }
        let text = span.content.to_string();
        let lit = span.style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        let mut at = 0;
        for (a, b) in marks {
            if a > at {
                out.push(Span::styled(text[at..a].to_string(), span.style));
            }
            out.push(Span::styled(text[a..b].to_string(), lit));
            at = b;
        }
        if at < text.len() {
            out.push(Span::styled(text[at..].to_string(), span.style));
        }
    }
    Line { spans: out, ..line }
}

/// The panel's name, as the header says it.
const NAME: &str = "relay";

/// The header's words, for either of its shapes: the headline — its lead
/// word and the tally after it — and the notes that follow, each in its own
/// tone: what a restart took, the global rules.
struct Said {
    lead: String,
    rest: Option<String>,
    notes: Vec<Span<'static>>,
}

impl Said {
    fn new(title: String, notes: Vec<Span<'static>>) -> Self {
        let (lead, rest) = match title.split_once(" · ") {
            Some((lead, rest)) => (lead.to_string(), Some(rest.to_string())),
            None => (title, None),
        };
        Said { lead, rest, notes }
    }

    /// The headline: the lead word wears the accent, the tally reads plainly.
    fn headline(&self) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(
            self.lead.clone(),
            theme::accent().add_modifier(Modifier::BOLD),
        )];
        if let Some(rest) = &self.rest {
            spans.push(Span::raw(format!(" · {rest}")));
        }
        spans
    }

    /// The notes, a ` · ` between each.
    fn notes(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, n) in self.notes.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" · "));
            }
            spans.push(n.clone());
        }
        spans
    }

    /// All of it on one line: the header where there is no room for the mark.
    fn line(&self) -> Line<'static> {
        let mut spans = self.headline();
        if !self.notes.is_empty() {
            spans.push(Span::raw(" · "));
            spans.extend(self.notes());
        }
        Line::from(spans)
    }

    /// The three lines beside the mark: the panel's name and version — and
    /// its home, when that is not the usual one — then the tally, then the
    /// notes. A headline that leads with the name goes without it, since the
    /// first line says it already.
    fn lines(&self, home: Option<&str>) -> [Line<'static>; 3] {
        let mut first = vec![
            Span::styled(NAME, theme::accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), theme::dim()),
        ];
        if let Some(home) = home {
            first.push(Span::styled(format!(" · {home}"), theme::dim()));
        }
        let tally = if self.lead == NAME {
            Line::raw(self.rest.clone().unwrap_or_default())
        } else {
            Line::from(self.headline())
        };
        [Line::from(first), tally, Line::from(self.notes())]
    }
}

/// The Relay home, for the header, when it is not the usual `~/.relay` — a
/// panel on a test ledger should say so — with `~` for home.
fn home_note(home: &Path, user: Option<&Path>) -> Option<String> {
    if user.is_some_and(|u| home == u.join(".relay")) {
        return None;
    }
    Some(rl_core::display_path(home))
}

/// What the list says when it has nothing to show. When nothing is running
/// at all — no search, no lens — the mark stands full size in the middle of
/// it with the words under it, where it fits; otherwise the words alone.
fn empty_list(msg: &str, area: Rect, nothing: bool) -> Paragraph<'static> {
    let words = msg.trim_start();
    let need = mark::FULL.len() as u16 + 2;
    let fits = area.height >= need && area.width as usize >= words.chars().count().max(mark::W as usize);
    if !nothing || !fits {
        return Paragraph::new(msg.to_string()).dim();
    }
    let mut lines = vec![Line::raw(""); ((area.height - need) / 2) as usize];
    lines.extend(mark::lines(&mark::FULL));
    lines.push(Line::raw(""));
    lines.push(Line::styled(words.to_string(), theme::dim()));
    Paragraph::new(lines).centered()
}

/// Where things go on screen.
struct Areas {
    head: Rect,
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

fn areas(area: Rect, wide: bool, show_pane: bool, share: u16, form_h: u16, cands_h: u16, line_h: u16) -> Areas {
    // The panel runs to the screen's edges, with rules between its sections —
    // but not in the full-screen terminal, which wants every cell.
    let chrome = !wide;
    let rule = u16::from(chrome);
    let [head, rule_a, body, form, cands, rule_b, line, rule_c, footer] = Layout::vertical([
        Constraint::Length(head_h(area, wide)),
        Constraint::Length(rule),
        Constraint::Fill(1),
        Constraint::Length(if form_h > 0 { form_h + 1 } else { 0 }),
        Constraint::Length(cands_h),
        Constraint::Length(rule),
        Constraint::Length(line_h.max(1)),
        Constraint::Length(rule),
        Constraint::Length(1),
    ])
    .areas(area);
    let rules = if chrome { vec![rule_a, rule_b, rule_c] } else { vec![] };
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
            Constraint::Percentage(share),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        (Some(l), Some(r))
    };
    Areas {
        head,
        rules,
        list,
        pane,
        form,
        cands,
        line,
        footer,
    }
}

/// How tall the header is: a row of air and the small mark's rows, with
/// three lines beside it, where the screen has room; one line in the full-screen terminal, which
/// wants every cell, and on a screen too short or too narrow to spare them.
fn head_h(area: Rect, wide: bool) -> u16 {
    if wide || area.height < TALL_H || area.width < TALL_W {
        1
    } else {
        HEAD_AIR + mark::SMALL.len() as u16
    }
}

/// The size a terminal gets in the pane: the pane's area, less the row that
/// names the run — or the whole screen, where there is no pane.
fn term_size(size: Rect, wide: bool, foot_h: u16) -> (u16, u16) {
    let a = areas(size, wide, true, LIST_SHARE_TERM, 0, 0, foot_h);
    let r = a.pane.unwrap_or(size);
    (r.width.max(1), r.height.saturating_sub(1).max(1))
}

/// Whether this terminal speaks the kitty keyboard protocol — asked once, the
/// first time we take the screen, and remembered. The query is a terminal
/// round-trip (it writes an escape sequence and reads the reply); asking it
/// again at every screen handoff would fire that query just as we hand stdin
/// to a run's terminal, and its reply would leak into the run — the stray
/// `^[[?…c` an attached agent would otherwise see. So we ask exactly once.
fn kitty_supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| supports_keyboard_enhancement().unwrap_or(false))
}

/// Take the screen, and — where the terminal speaks the kitty keyboard
/// protocol — ask it to report modifiers on keys the base protocol folds
/// away, so shift-enter arrives as its own event rather than a bare newline.
/// Terminals that don't (Terminal.app, older tmux) simply see no flags.
///
/// It takes both flags to get that. Disambiguation only tells the special keys
/// apart from the escape sequences they are spelled with; `enter` still comes
/// back as a bare `\r`, which has nowhere to carry a modifier, so shift-enter
/// arrives indistinguishable from enter and sends instead of opening a line.
/// Reporting every key as an escape code is what gives it somewhere to put the
/// modifier (`CSI 13;2u`), which is what the line is watching for. It also
/// sends a shifted key as the key plus shift — `?` as shift and `/` — so the
/// terminal is asked, too, to say what shift made of it (see `input::typed`).
fn enter_screen() -> ratatui::DefaultTerminal {
    // And the input is whatever they left too: answers to questions a run put
    // to the terminal just before letting go, which were never its keys and
    // are not ours (see `input`).
    input::discard_pending(std::io::stdin());
    // Coming back from a run or an editor, the screen is whatever they left:
    // a full-screen agent keeps its own alternate screen, its scroll region
    // and its colours when it is let go of mid-session, and every mode it
    // asked the terminal for — focus reports, mouse tracking. Leave that
    // screen so ours starts fresh, and put the region, the colours and the
    // modes back; ours are asked for again below.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::style::Print("\x1b[?1049l\x1b[r\x1b[0m\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l")
    );
    let mut term = ratatui::init();
    // A fresh terminal diffs against an empty buffer, so it would never write
    // the cells it means to leave blank — and whatever the run drew there
    // would stay. Clearing makes the first draw paint every cell.
    let _ = term.clear();
    if kitty_supported() {
        // A run's own keyboard flags would change what every key sends us:
        // none, then ours.
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::style::Print("\x1b[=0;1u"),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }
    // The wheel and a click reach the list. This also takes the terminal's own
    // drag-to-select, so hold shift (option, on macOS) to copy text as before.
    // A paste comes whole, so its newlines never send the line half-way.
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    term
}

/// Hand the screen back, popping any enhancement flags we pushed and giving
/// the mouse to whoever we hand the terminal to — an editor, or a run.
///
/// And the caret. Drawing hides the terminal's cursor, and `ratatui::restore`
/// leaves raw mode and the alternate screen but never shows it again; the
/// terminal we drew on is still alive across a handover, so nothing else
/// does either. A joined shell or agent would get a terminal with its cursor
/// switched off, and most never switch it back on. The next draw hides it again.
fn leave_screen() {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    if kitty_supported() {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
}

/// `rl setup`: the whole guide, on its own, whatever was seen before.
/// `rl import`: its Claude sessions step alone.
pub async fn setup(home: &Home, only_import: bool) -> Result<()> {
    let mut rpc = crate::daemon::ensure(home, true).await?;
    let state = rpc.call::<SetupGet>(()).await?;
    let sessions = rpc.call::<ImportList>(ImportListParams { mark: true }).await?;
    let file = state.config.file.clone();
    let guide = if only_import {
        setup::Guide::importing(&state, &sessions)
    } else {
        setup::Guide::new(&state, &sessions)
    };
    let agents = state.agents.clone();
    let mut terminal = enter_screen();
    let outcome = setup::run(&mut terminal, &mut rpc, guide, &agents).await;
    leave_screen();
    match outcome? {
        setup::Outcome::Saved(words) if only_import => println!("{words}"),
        setup::Outcome::Saved(words) => println!("{words}\n{}", file.display()),
        setup::Outcome::Later => println!("left as it was"),
    }
    Ok(())
}

/// Where the catch-up starts, if the panel opens on it: when you were last
/// here (`prev`, what touching presence on this open gave back), when that
/// is longer ago than `after`. A first open has nothing to catch up on, and
/// `after: off` never opens it.
pub(crate) fn away(prev: Option<Timestamp>, now: Timestamp, after: Option<SignedDuration>) -> Option<Timestamp> {
    let (prev, after) = (prev?, after?);
    (now.duration_since(prev) > after).then_some(prev)
}

pub async fn run(home: &Home) -> Result<()> {
    let me = client_name();
    // Before the screen is taken: the daemon is started or, if it is another
    // release, a restart is offered on the plain terminal.
    let mut rpc = crate::daemon::ensure(home, true).await?;
    // Touching presence gives back when you were last here — read before it
    // is overwritten, since that is where the catch-up starts.
    let prev = rpc
        .call::<PresenceTouch>(PresenceClient { client: me.clone() })
        .await
        .ok()
        .and_then(|p| p.last_seen_at);
    let aliases = complete::shell_names().await;
    let mut app = App::new(home, rpc, me, aliases);
    // A project's or everything's `/summary` starts there, catch-up or not,
    // and a bare `/since`.
    app.away_since = prev;
    app.left = prev;
    app.refresh().await;

    let mut terminal = enter_screen();
    // The first start, or a release with a step not seen yet: the guide
    // before the list, asking only what is new. Otherwise a word about
    // Claude's sessions that are not here, once each. Away long enough, and
    // with no guide this time, the catch-up before the list.
    let mut after = None;
    let mut guided = false;
    if let Ok(state) = app.rpc.call::<SetupGet>(()).await {
        after = state.config.config.catch_up.after();
        let sessions = app
            .rpc
            .call::<ImportList>(ImportListParams { mark: true })
            .await
            .unwrap_or_default();
        let outside = sessions
            .sessions
            .iter()
            .filter(|s| s.new && s.run.is_none() && !s.in_a_terminal())
            .count();
        match setup::Guide::since(&state, &sessions) {
            Some(guide) if state.seen < SETUP_STEPS => {
                guided = true;
                let agents = state.agents.clone();
                match setup::run(&mut terminal, &mut app.rpc, guide, &agents).await {
                    Ok(setup::Outcome::Saved(words)) => {
                        if !words.is_empty() {
                            app.say(words);
                        }
                        app.agents.clear();
                        app.refresh().await;
                    }
                    Ok(setup::Outcome::Later) => {
                        app.say("setup left for later · rl setup, or it asks again next start")
                    }
                    Err(e) => app.say(format!("setup: {e:#}")),
                }
            }
            _ if outside > 0 => app.say(format!(
                "{outside} Claude session{} outside Relay · /import",
                if outside == 1 { "" } else { "s" }
            )),
            _ => {}
        }
    }
    if !guided && let Some(since) = away(prev, Timestamp::now(), after) {
        app.open_catch_up(since).await;
    }
    let result = app.main_loop(&mut terminal).await;
    leave_screen();
    // Closing the panel on the catch-up is leaving it too.
    app.saw_catch_up().await;
    // Leaving is looking: `rl since` starts from here next time.
    let _ = app
        .rpc
        .call::<PresenceTouch>(PresenceClient { client: app.me.clone() })
        .await;
    result
}

impl App {
    /// The panel as it opens: talking to the daemon on `rpc`, as `me`, with
    /// the shell's `aliases` for the line to know. Nothing is read yet.
    fn new(home: &Home, rpc: Client, me: String, aliases: Vec<String>) -> Self {
        Self {
            home: home.clone(),
            rpc,
            me,
            digest: Digest {
                since: Timestamp::now(),
                items: Vec::new(),
                decisions: Vec::new(),
                checkouts: Vec::new(),
                lost: Vec::new(),
                hidden: Vec::new(),
                order: Vec::new(),
            },
            names: Names::default(),
            filter: String::new(),
            search: None,
            lens: rows::Lens::All,
            view: View::Work,
            rules: Vec::new(),
            graph: None,
            back: Vec::new(),
            kept: HashMap::new(),
            filter_aside: String::new(),
            feed: events::Feed::default(),
            graph_at: std::cell::Cell::new((0, 0)),
            shell_at: HashMap::new(),
            scroll: std::cell::Cell::new(0),
            list_h: std::cell::Cell::new(0),
            hit: std::cell::RefCell::default(),
            sel: None,
            pane: Pane::Facts,
            show_pane: false,
            auto_pane: false,
            answer_at: 0,
            pane_hit: std::cell::RefCell::default(),
            wide: false,
            term: None,
            term_size: (0, 0),
            events: Vec::new(),
            editor: None,
            attach: None,
            importing: false,
            not_loaded: Vec::new(),
            reload_list: None,
            asking: None,
            summary: None,
            summarising: None,
            topping: None,
            top: None,
            summary_ask: None,
            made: HashMap::new(),
            away_since: None,
            left: None,
            catch: None,
            unseen_from: HashMap::new(),
            removing: None,
            picked: Vec::new(),
            line: line::Line::default(),
            agents: Vec::new(),
            path_dirs: complete::path_dirs(),
            aliases,
            completing: None,
            pick: None,
            mode: Mode::Normal,
            flash: None,
            arm: None,
            shortcuts: false,
            quit: false,
        }
    }

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
                        self.key(input::typed(k), area).await?;
                        if let Some((file, line)) = self.editor.take() {
                            // The editor gets the terminal; our reader lets go of stdin first.
                            drop(input);
                            leave_screen();
                            let outcome = crate::env::open_in_editor(&file, line);
                            *terminal = enter_screen();
                            input = EventStream::new();
                            match outcome {
                                Ok(()) => self.reload_rules().await,
                                Err(e) => self.say(format!("{e:#}")),
                            }
                        }
                        if std::mem::take(&mut self.importing) {
                            // The guide takes the screen; the list comes back after.
                            let state = self.rpc.call::<SetupGet>(()).await;
                            let sessions = self.rpc.call::<ImportList>(ImportListParams { mark: true }).await;
                            match (state, sessions) {
                                (Ok(state), Ok(sessions)) => {
                                    let agents = state.agents.clone();
                                    let guide = setup::Guide::importing(&state, &sessions);
                                    match setup::run(terminal, &mut self.rpc, guide, &agents).await {
                                        Ok(setup::Outcome::Saved(words)) if !words.is_empty() => self.say(words),
                                        Ok(_) => {}
                                        Err(e) => self.say(format!("import: {e:#}")),
                                    }
                                    self.refresh().await;
                                }
                                (Err(e), _) | (_, Err(e)) => self.say(format!("import: {e:#}")),
                            }
                        }
                        if let Some((id, take)) = self.attach.take() {
                            let id_back = id.clone();
                            // The run gets the real terminal, the same handover the editor
                            // uses and the same passthrough `rl join` uses — a real terminal,
                            // not our re-render. We come back to the list when it lets go.
                            drop(input);
                            leave_screen();
                            let outcome = crate::attach::attach(
                                &self.home,
                                crate::attach::Opts {
                                    id: id_back,
                                    mode: AttachMode::Join,
                                    take,
                                    scrollback: JOIN_SCROLLBACK,
                                },
                            )
                            .await;
                            *terminal = enter_screen();
                            input = EventStream::new();
                            self.refresh().await;
                            match outcome {
                                Ok(ended) => self.back_from_join(&id, ended),
                                Err(e) => self.join_refused(id, &e),
                            }
                        }
                    }
                    Some(Ok(Event::Paste(s))) => self.paste(s, area).await?,
                    Some(Ok(Event::Mouse(m))) => self.mouse(m, area).await?,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => self.quit = true,
                },
                _ = tick.tick() => self.tick().await,
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
                done = async {
                    match &mut self.topping {
                        Some(s) => (&mut s.task).await,
                        None => std::future::pending().await,
                    }
                } => self.topped(done).await,
                gone = async {
                    match &mut self.removing {
                        Some(r) => (&mut r.task).await,
                        None => std::future::pending().await,
                    }
                } => self.removed(gone).await,
                more = async {
                    match &mut self.term {
                        Some(t) if !t.over => t.pump().await,
                        _ => std::future::pending().await,
                    }
                } => match more {
                    Ok(true) => {}
                    Ok(false) => {
                        // An ended run's output is still worth reading, so the
                        // view stays; there is just nothing more to poll. A
                        // stream that went without an exit is worth reading
                        // most of all — throwing it away used to take the last
                        // thing the run said, at the one moment it mattered.
                        if self.term.as_ref().is_some_and(|t| t.ended.is_none()) {
                            let words = self.lost_words();
                            if let Some(t) = self.term.as_mut() {
                                t.lost = true;
                            }
                            self.say(words);
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
        term_size(size, self.wide, self.foot_h(size.width))
    }

    /// How many rows the live line takes: its own, capped, at least one.
    fn line_h(&self) -> u16 {
        if self.search.is_some() {
            return 1;
        }
        (self.line.rows() as u16).clamp(1, LINE_MAX)
    }

    /// The live line and the shortcuts under it, when they are up: the rows
    /// the foot of the panel takes at this width.
    fn foot_h(&self, width: u16) -> u16 {
        self.line_h() + self.shortcut_lines(width).len() as u16
    }

    /// Where the keyboard is, for the shortcuts line; none where it has no
    /// place — a form, a `y`-or-not question, a search, which keep their keys
    /// in the footer.
    fn shortcuts_place(&self) -> Option<shortcuts::Place> {
        match &self.mode {
            Mode::Normal => {}
            Mode::Answer(id) => {
                let q = self.digest.decisions.iter().find(|d| &d.id == id)?;
                return Some(shortcuts::Place::Answer {
                    n: q.options.len().min(pane::MAX_KEYS),
                    multi: q.multi,
                });
            }
            Mode::Rename(_) => return Some(shortcuts::Place::Rename),
            _ => return None,
        }
        if self.search.is_some() {
            return None;
        }
        // The same test as the footer's: the tile's keys while the cursor is
        // on the run it shows.
        if self.pane == Pane::Term
            && let Some(t) = &self.term
            && (self.wide || self.selected_run().is_some_and(|i| i.run.id == t.id))
        {
            return Some(shortcuts::Place::Term {
                wide: self.wide,
                live: !t.lost && t.ended.is_none(),
            });
        }
        Some(match self.view {
            View::Graph => shortcuts::Place::Graph,
            View::Events => shortcuts::Place::Events,
            View::CatchUp => shortcuts::Place::CatchUp,
            View::Work if self.selected_question().is_some() => shortcuts::Place::Question,
            View::Work if self.lens == rows::Lens::Hidden => shortcuts::Place::Hidden,
            View::Work => shortcuts::Place::List,
        })
    }

    /// The shortcuts line as drawn at this width, wrapped between keys; none
    /// while it is put away.
    fn shortcut_lines(&self, width: u16) -> Vec<Line<'static>> {
        if !self.shortcuts {
            return vec![];
        }
        let Some(at) = self.shortcuts_place() else {
            return vec![];
        };
        let room = width.saturating_sub(TEXT_INDENT).max(1) as usize;
        shortcuts::lines(&shortcuts::keys(at), room)
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
        // A daemon from before `rule.errors` has nothing to say here.
        self.not_loaded = self.rpc.call::<RuleErrors>(()).await.unwrap_or_default();
        // The catch-up's stretch, while it is up or there to go back to.
        match self.catch.as_ref().map(|c| c.since) {
            Some(since) if self.view == View::CatchUp || self.back.contains(&View::CatchUp) => {
                match self.rpc.call::<DigestGet>(DigestParams { since: Some(since) }).await {
                    Ok(d) => self.catch = Some(d),
                    Err(e) => self.say(format!("daemon: {e:#}")),
                }
            }
            _ => self.catch = None,
        }
        if self.view == View::Graph {
            match self.rpc.call::<GraphGet>(GraphParams::default()).await {
                Ok(g) => self.graph = Some(g),
                Err(e) => self.say(format!("daemon: {e:#}")),
            }
        }
        // Which CLIs are agents decides how a line reads and what it offers:
        // the ones here that the config has the panel offer. Asked once.
        if self.agents.is_empty()
            && let Ok(list) = self.rpc.call::<AgentList>(()).await
        {
            self.agents = list
                .into_iter()
                .filter(|a| a.offered && a.available())
                .map(|a| a.name)
                .collect();
        }
        self.settle();
        self.load_pane().await;
    }

    /// What the details pane shows of the selected row: a run's events, and
    /// the summary kept of the row — over the stretch and depth last made of
    /// it here, else its default one. Only when the pane is open on it.
    async fn load_pane(&mut self) {
        if self.pane != Pane::Facts {
            return;
        }
        if let Some(id) = self.selected_run().map(|i| i.run.id.clone()) {
            let filter = EventFilter {
                run: Some(id),
                limit: Some(200),
                ..Default::default()
            };
            if let Ok(evs) = self.rpc.call::<EventList>(filter).await {
                self.events = evs;
            }
        }
        let Some(target) = self.summary_target() else { return };
        let made = self.made.get(&target.key()).cloned();
        let (deep, from) = match (&made, &target) {
            (Some(m), _) => (m.deep, m.from),
            (None, SummaryTarget::Project { .. } | SummaryTarget::All) => (false, self.away_since),
            (None, SummaryTarget::Job { .. }) => {
                (false, self.sel.as_ref().and_then(|k| self.unseen_from.get(k)).copied())
            }
            (None, _) => (false, None),
        };
        // A project or everything with no stretch has nothing kept for it.
        let needs_from = matches!(target, SummaryTarget::Project { .. } | SummaryTarget::All);
        let kept = if needs_from && from.is_none() {
            None
        } else {
            let ask = SummaryParams {
                target,
                deep,
                from,
                again: false,
                dry: false,
                timeout: None,
            };
            match self.rpc.call::<SummaryGet>(ask).await {
                Ok(s) => s,
                Err(_) => return,
            }
        };
        self.summary = kept.or(made);
    }

    // ---- the list ----------------------------------------------------------

    fn rows(&self) -> Vec<Row<'_>> {
        rows::build(
            &self.digest,
            &rows::Context {
                names: &self.names,
                rules: &self.rules,
                filter: &self.filter,
                lens: self.lens,
                now: Timestamp::now(),
            },
        )
    }

    /// What `/summary` on the selected row is of: everything on the
    /// catch-up's top; a question's, the run that asked it.
    fn summary_target(&self) -> Option<SummaryTarget> {
        if self.on_top() {
            return Some(SummaryTarget::All);
        }
        self.target_of(&self.selected()?)
    }

    /// What a summary of `row` is of: a run; a job's runs, in its project's
    /// folder or, under `everywhere`, anywhere; a project; everything, on the
    /// `everywhere` header.
    fn target_of(&self, row: &Row) -> Option<SummaryTarget> {
        Some(match row {
            Row::Run(i) => SummaryTarget::Run { id: i.run.id.clone() },
            Row::Job(j) => {
                let last = &j.last().run;
                SummaryTarget::Job {
                    rule: j.kind,
                    name: j.name.clone(),
                    workspace: (!rows::everywhere_run(last, &self.names, &self.rules)).then(|| last.workspace.clone()),
                }
            }
            Row::Group(h) if h.is_everywhere() => SummaryTarget::All,
            Row::Group(h) => SummaryTarget::Project {
                workspace: h.workspace.clone(),
            },
            // A question is told by the run that asked it.
            Row::Question(q) => SummaryTarget::Run { id: q.run.clone()? },
        })
    }

    /// The summary kept of `row`, when the one loaded is of it.
    fn summary_of_row(&self, row: &Row) -> Option<&Summary> {
        let s = self.summary.as_ref()?;
        match self.target_of(row)? {
            SummaryTarget::Run { id } => (s.run == id).then_some(s),
            t => (s.target == t.key()).then_some(s),
        }
    }

    /// The folder the selected row's work is in. The `everywhere` header has
    /// none: what is under it ran in folders of all sorts.
    fn selected_folder(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Run(i) => Some(i.run.workspace.clone()),
            Row::Job(j) => Some(j.last().run.workspace.clone()),
            Row::Question(q) => Some(q.workspace.clone()),
            Row::Group(h) if h.is_everywhere() => None,
            Row::Group(h) => Some(h.workspace.clone()),
        }
    }

    /// The registered folder under the cursor, when the cursor is on a project
    /// header — what `^r` acts on.
    fn selected_project(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Group(h) if !h.is_everywhere() => Some(h.workspace.clone()),
            _ => None,
        }
    }

    /// Whether the cursor is on the `everywhere` header, which is not a
    /// project: what acts on a project's folder says so there, and does
    /// nothing.
    fn on_everywhere(&self) -> bool {
        matches!(self.selected(), Some(Row::Group(h)) if h.is_everywhere())
    }

    /// What the `everywhere` header says to a project's action.
    const NOT_A_PROJECT: &'static str = "everywhere is not a project";

    /// Keys of the selectable rows of the view that is up, in order.
    fn keys(&self) -> Vec<String> {
        match self.view {
            View::Work => self.rows().into_iter().filter_map(|r| r.key()).collect(),
            View::Graph => self.graph_plan().map(|(_, p)| p.order()).unwrap_or_default(),
            View::Events => self.event_items().iter().flat_map(|i| i.keys()).collect(),
            View::CatchUp => std::iter::once(TOP.to_string())
                .chain(self.catch_rows().into_iter().filter_map(|r| r.key()))
                .collect(),
        }
    }

    /// The graph as the filter leaves it, laid out.
    fn graph_plan(&self) -> Option<(&rl_proto::graph::Graph, graph::Plan)> {
        let g = self.graph.as_ref()?;
        let kept = graph::keep(g, &self.filter, &self.names, &self.digest.checkouts);
        Some((g, graph::plan(g, &kept, &self.names, Timestamp::now())))
    }

    /// Keeps the selection on something that exists: the first row, when
    /// it has to move — in the catch-up, the first under its top.
    fn settle(&mut self) {
        let keys = self.keys();
        if self.sel.as_ref().is_none_or(|k| !keys.contains(k)) {
            let first = keys.iter().find(|k| *k != TOP).or(keys.first());
            self.select(first.cloned());
        }
    }

    /// Whether the cursor is on the catch-up's top.
    fn on_top(&self) -> bool {
        self.view == View::CatchUp && self.sel.as_deref() == Some(TOP)
    }

    /// Keys of what the search's words name, best first; see `query::ranked`.
    /// Nothing when it has no words, only tokens.
    fn matches(&self) -> Vec<String> {
        let words = query::Query::parse(&self.filter).words;
        if words.is_empty() {
            return vec![];
        }
        let (keys, named): (Vec<Option<String>>, Vec<query::Named>) = match self.view {
            View::Work => self
                .rows()
                .iter()
                .map(|r| (r.key(), rows::named(r, &self.digest, &self.names)))
                .unzip(),
            View::Graph => {
                let Some((g, p)) = self.graph_plan() else {
                    return vec![];
                };
                p.order()
                    .into_iter()
                    .filter_map(|id| g.node(&id).map(|n| (Some(id.clone()), graph::named(n, &self.names))))
                    .unzip()
            }
            View::Events => self
                .event_items()
                .iter()
                .map(|i| (Some(i.key()), events::named(i)))
                .unzip(),
            View::CatchUp => self
                .catch_rows()
                .iter()
                .map(|r| (r.key(), rows::named(r, &self.digest, &self.names)))
                .unzip(),
        };
        query::ranked(&words, &named)
            .into_iter()
            .filter_map(|i| keys[i].clone())
            .collect()
    }

    /// The search changed: the cursor goes to the best match. With no words
    /// to match, it stays where it is if it can.
    fn land(&mut self) {
        match self.matches().into_iter().next() {
            Some(k) => self.select(Some(k)),
            None => self.settle(),
        }
    }

    /// `tab` with nothing to complete: on to the next match, round; shift-tab
    /// back. False when there is none to go to.
    fn next_match(&mut self, back: bool) -> bool {
        let order = self.matches();
        let Some(k) = query::next_match(&order, self.sel.as_ref(), back).cloned() else {
            return false;
        };
        self.select(Some(k));
        true
    }

    fn select(&mut self, key: Option<String>) {
        if key != self.sel {
            // A name being changed belongs to its row: off the row, it goes
            // unsaved.
            if matches!(self.mode, Mode::Rename(_)) {
                self.mode = Mode::Normal;
            }
            // A question is only worth its details: selecting one opens the
            // pane on it, and moving off closes the pane again — unless it was
            // open already.
            let question =
                matches!(self.view, View::Work | View::CatchUp) && key.as_deref().is_some_and(|k| k.starts_with("q:"));
            if question {
                if !self.show_pane {
                    self.auto_pane = true;
                    self.show_pane = true;
                }
                if self.pane == Pane::Term {
                    self.leave_term();
                }
            } else if self.auto_pane {
                self.hide_pane();
            }
            self.sel = key;
            self.picked.clear();
            self.answer_at = 0;
            self.events.clear();
            // A watch tile stays pinned to its run while you browse the list;
            // it closes on `esc` or when another run's terminal takes its place,
            // not merely because the cursor moved.
        }
    }

    /// The row under the cursor, on the list or in the catch-up.
    fn selected(&self) -> Option<Row<'_>> {
        let rows = match self.view {
            View::Work => self.rows(),
            View::CatchUp => self.catch_rows(),
            View::Graph | View::Events => return None,
        };
        let key = self.sel.as_ref()?;
        rows.into_iter().find(|r| r.key().as_ref() == Some(key))
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

    /// The question the line answers: the one under the cursor on the list
    /// or in the catch-up, or the first one waiting at the node under it in
    /// the graph.
    fn question_here(&self) -> Option<&Decision> {
        match self.view {
            View::Work | View::CatchUp => self.selected_question(),
            View::Graph => {
                let n = self.selected_node()?;
                n.waiting
                    .iter()
                    .find_map(|id| self.digest.decisions.iter().find(|d| &d.id == id))
            }
            View::Events => None,
        }
    }

    fn selected_rule(&self) -> Option<&Rule> {
        // A node stands for its rule, or for what its rule runs or asks.
        let key = self.selected_node()?.rule.clone()?;
        self.rules.iter().find(|r| rules::key(r) == key)
    }

    /// The node under the cursor in the graph.
    fn selected_node(&self) -> Option<&rl_proto::graph::Node> {
        if self.view != View::Graph {
            return None;
        }
        self.graph.as_ref()?.node(self.sel.as_deref()?)
    }

    /// An arrow in the graph: along the wiring, or within a column.
    fn graph_step(&mut self, dir: graph::Dir) {
        let next = self
            .graph_plan()
            .and_then(|(_, p)| graph::step(&p, self.sel.as_deref(), dir));
        if next.is_some() {
            self.select(next);
        }
    }

    /// `enter` on a node: answer the question waiting there, run a schedule
    /// now, or else show what the node is.
    fn graph_enter(&mut self) {
        let Some(n) = self.selected_node() else {
            return;
        };
        let waiting = n
            .waiting
            .iter()
            .find(|id| self.digest.decisions.iter().any(|d| &d.id == *id))
            .cloned();
        let schedule = match self.selected_rule() {
            Some(r) if r.kind() == RuleKind::Schedule && !matches!(n.kind, rl_proto::graph::NodeKind::Ask) => Some((
                r.approved(),
                ScheduleName {
                    name: r.name().into(),
                    scope: Some(r.scope().into()),
                },
            )),
            _ => None,
        };
        if let Some(id) = waiting {
            self.picked.clear();
            self.pane = Pane::Facts;
            self.show_pane = true;
            self.answer_at = 0;
            self.mode = Mode::Answer(id);
        } else if let Some((approved, name)) = schedule {
            if approved {
                self.mode = Mode::Fire(name);
            } else {
                self.say("not approved yet: /approve");
            }
        } else {
            self.pane = Pane::Facts;
            self.show_pane = !self.show_pane;
        }
    }

    /// The rule behind a job row.
    fn rule_of_job(&self, j: &rows::Job) -> Option<&Rule> {
        self.rules.iter().find(|x| {
            x.kind() == j.kind
                && x.name() == j.name
                && (x.is_global() || Path::new(x.scope()) == j.last().run.workspace)
        })
    }

    /// The rule under the cursor: a node's in the graph, or behind a job row.
    fn rule_here(&self) -> Option<&Rule> {
        self.selected_rule()
            .or_else(|| self.selected_job().and_then(|j| self.rule_of_job(&j)))
    }

    fn run_by_id(&self, id: &str) -> Option<&Run> {
        self.digest.items.iter().map(|i| &i.run).find(|r| r.id == id)
    }

    /// The cursor is on it: that is looking. A job's runs count as looked at
    /// when the job is.
    /// The cursor moved: what the new row needs, and nothing more. The list
    /// itself is not fetched again — the tick keeps it current — so holding
    /// a key moves at the speed of drawing, not of a whole digest per step.
    async fn moved(&mut self) {
        // The catch-up is looked at as a whole, when you leave it.
        if self.view != View::CatchUp {
            self.saw_selected().await;
        }
        self.load_pane().await;
    }

    async fn saw_selected(&mut self) {
        let mut from = None;
        let ids: Vec<String> = match self.selected() {
            Some(Row::Run(i)) if i.unseen => vec![i.run.id.clone()],
            Some(Row::Job(j)) => {
                let unseen: Vec<_> = j.runs.iter().filter(|i| i.unseen).collect();
                from = unseen
                    .iter()
                    .map(|i| i.run.started_at)
                    .min()
                    .map(|at| (j.key.clone(), at));
                unseen.iter().map(|i| i.run.id.clone()).collect()
            }
            _ => vec![],
        };
        if let Some((key, at)) = from {
            self.unseen_from.insert(key, at);
        }
        self.seen(ids).await;
    }

    /// The wheel steps the selection — the list scrolls to follow the cursor,
    /// so there is no scroll of its own to move — and the cursor travels with
    /// the pointer: moving over a row puts the cursor on it, and so does a
    /// click. All only over the list itself.
    ///
    /// Moving is not looking, though. Passing over a row on the way somewhere
    /// else must not mark it seen, and a pointer sweeps many rows a second, so
    /// a move only selects; the wheel and a click count as looking and refresh.
    async fn mouse(&mut self, m: MouseEvent, area: Rect) -> Result<()> {
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

    /// Back from a join: to the list, with the cursor on the run and no pane
    /// beside it, and a word on how it went.
    fn back_from_join(&mut self, id: &str, ended: crate::attach::Ended) {
        use crate::attach::Ended;
        let title = self
            .run_by_id(id)
            .map(|r| r.title.clone())
            .unwrap_or_else(|| id.to_string());
        match ended {
            Ended::Closed { .. } => {
                if let Some(t) = self.term.as_mut().filter(|t| t.id == id) {
                    t.lost = true;
                }
                self.say(self.gone_words(id, &title));
            }
            Ended::Exit { how, .. } => self.say(format!("{title} ended ({how})")),
            Ended::Detached | Ended::Left => self.say(format!("let go of {title} · it keeps going")),
        }
        self.select(Some(format!("r:{id}")));
        self.hide_pane();
    }

    /// A paste, whole: into the line as it came, newlines and all, over a
    /// name being changed as one line, or a form's field a character at a
    /// time.
    async fn paste(&mut self, s: String, area: Rect) -> Result<()> {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        if let Mode::Rename(r) = &mut self.mode {
            r.field.paste(&s);
            return Ok(());
        }
        if matches!(self.mode, Mode::Normal) {
            self.line.insert(&s);
            self.completing = None;
            self.shortcuts = shortcuts::still_open(self.shortcuts, self.line.is_empty());
            return Ok(());
        }
        for c in s.chars().filter(|c| !c.is_control()) {
            self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), area)
                .await?;
        }
        Ok(())
    }

    /// A join that did not happen: someone has the keyboard — asked whether
    /// to take it from them — or it failed.
    fn join_refused(&mut self, id: String, e: &anyhow::Error) {
        match held_by(e) {
            Some(who) => self.mode = Mode::Take(id, who),
            None => self.say(format!("join: {e:#}")),
        }
    }

    /// The view goes for good: the stream died, or another run wants it.
    /// A stream that went without an exit, in words. What rld last said the
    /// run was is the honest part: it may have failed, or it may be going on
    /// without us, and those are not the same news.
    fn lost_words(&self) -> String {
        let Some(t) = self.term.as_ref() else {
            return "lost the connection".into();
        };
        self.gone_words(&t.id, &t.info.run.title)
    }

    /// The same, for run `id`, known as `title` if the list no longer has it.
    fn gone_words(&self, id: &str, title: &str) -> String {
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

    fn close_term(&mut self) {
        if self.term.is_some() {
            self.wide = false;
        }
        self.term = None;
        self.pane = Pane::Facts;
    }

    /// Stepping back to the list. The view stays open and goes on reading, so
    /// coming back is instant and lands where it was left.
    fn leave_term(&mut self) {
        self.wide = false;
        self.pane = Pane::Facts;
    }

    /// Hide the side pane. `pane == Term` must not outlive `show_pane`: a
    /// stale `Term` with the pane hidden would size and route keys for a tile
    /// nobody can see — the invariant `leave_term` keeps by always resetting
    /// to `Facts`.
    fn hide_pane(&mut self) {
        self.show_pane = false;
        self.auto_pane = false;
        // Full screen is the pane too: hiding it gives the list back.
        self.wide = false;
        if self.pane == Pane::Term {
            self.pane = Pane::Facts;
        }
    }

    /// `/pane`, `^o`: the pane beside the list, shown or hidden.
    fn toggle_pane(&mut self) {
        self.auto_pane = false;
        if self.show_pane {
            self.hide_pane();
        } else {
            self.show_pane = true;
        }
    }

    /// `/pane full`: the pane takes the whole screen, or gives it back.
    fn toggle_full(&mut self) {
        self.wide = !self.wide;
        if self.wide {
            self.show_pane = true;
        }
    }

    /// `/rename`, `^r`: whatever the cursor is on, by the name it is known by —
    /// a folder through the registry, a run (a shell is one) through the
    /// ledger, both typed over where they stand in the list, and a job by the
    /// rule it comes from, which is the rule form's own rename.
    fn rename_here(&mut self) {
        // A name is changed where it stands, and it stands on the list.
        if self.view == View::CatchUp {
            return self.say("^r renames on the list · esc goes there");
        }
        let what = if let Some(folder) = self.selected_project() {
            Some((self.names.workspace(&folder), Renamed::Project(folder)))
        } else {
            self.selected_run()
                .map(|i| (i.run.title.clone(), Renamed::Run(i.run.id.clone())))
        };
        if self.on_everywhere() {
            self.say(format!("{} · nothing to rename", Self::NOT_A_PROJECT));
        } else if let (Some((was, what)), Some(key)) = (what, self.sel.clone()) {
            // The keys are the name's now: a search gives the line back as
            // `enter` would, and what the line offered is put away.
            self.search = None;
            self.completing = None;
            self.pick = None;
            self.mode = Mode::Rename(Box::new(Renaming {
                key,
                what,
                field: line::Field::selected(was.clone()),
                was,
                from: Default::default(),
            }));
        } else if self.rule_here().is_some() {
            self.edit_rule_form();
        } else {
            self.say("^r renames what the cursor is on");
        }
    }

    /// `/reload shell`: the shell's aliases and functions, read again.
    async fn reload_shell(&mut self) {
        self.aliases = complete::shell_names().await;
        self.say(format!("re-read the shell: {} names", self.aliases.len()));
    }

    async fn seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        mark_seen(&mut self.digest, &ids);
        let _ = self.rpc.call::<RunSeen>(RunIds { ids }).await;
    }

    // ---- doing things ------------------------------------------------------

    /// An ended run's terminal, re-rendered in the pane to read. A live run
    /// is never shown here: `enter` on one hands the real terminal over
    /// (`raw::attach`).
    async fn open_term(&mut self, area: Rect) {
        let Some(id) = self.selected_run().map(|i| i.run.id.clone()) else {
            self.say("nothing here to open");
            return;
        };
        // Watching is a tile beside the list, not the whole screen: the list
        // stays, the line stays yours, and `/pane full` pops it big.
        // Still reading this one: back to it, where it was left.
        if self.term.as_ref().is_some_and(|t| t.id == id) {
            self.pane = Pane::Term;
            self.show_pane = true;
            self.wide = false;
            self.term_size = self.pane_size(area);
            return;
        }
        self.seen(vec![id.clone()]).await;
        self.pane = Pane::Term;
        self.show_pane = true;
        self.wide = false;
        let size = self.pane_size(area);
        match TermView::open(&self.home, &id, size).await {
            Ok(t) => {
                self.term = Some(t);
                self.term_size = size;
            }
            Err(e) => {
                self.pane = Pane::Facts;
                self.say(format!("attach: {e:#}"));
            }
        }
    }

    async fn answer(&mut self, id: String, options: Vec<String>, note: Option<String>) {
        let what = options.join(", ");
        match self
            .rpc
            .call::<DecisionTake>(DecisionTakeParams { id, options, note })
            .await
        {
            Ok(d) => {
                self.picked.clear();
                // In the graph the cursor goes on to what the answer runs, a
                // node there; in the list, to the run it started.
                let next_node = (self.view == View::Graph && d.chosen.len() == 1)
                    .then(|| self.sel.as_ref().map(|ask| format!("run:{ask}/{}", d.chosen[0])))
                    .flatten();
                self.refresh().await;
                match d.outcome_runs.first() {
                    Some(r) => {
                        let title = self.run_by_id(r).map(|r| r.title.clone()).unwrap_or_default();
                        self.say(format!("{what} → started {title}"));
                        let started = format!("r:{r}");
                        if self.view == View::Graph {
                            if next_node.as_ref().is_some_and(|id| self.keys().contains(id)) {
                                self.select(next_node);
                            }
                        } else if self.view == View::Work || self.keys().contains(&started) {
                            self.select(Some(started));
                        }
                    }
                    None => self.say(format!("answered {what}")),
                }
            }
            Err(e) => self.say(format!("answer: {e:#}")),
        }
    }

    /// Mark an option of a question taking several, or unmark it.
    fn toggle_pick(&mut self, i: usize) {
        match self.picked.iter().position(|&p| p == i) {
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(i),
        }
    }

    /// Do what a question's choice says — by `enter` on it, its number, or a
    /// click. Picking where several are taken keeps the question open.
    async fn take_choice(&mut self, q: &Decision, c: pane::Choice, area: Rect) {
        let id = q.id.clone();
        match c {
            pane::Choice::Option(i) if q.multi => {
                self.toggle_pick(i);
                self.mode = Mode::Answer(id);
            }
            pane::Choice::Option(i) => self.answer(id, vec![q.options[i].key.clone()], None).await,
            pane::Choice::Send if self.picked.is_empty() => {
                self.say("pick one first: space, or its number");
                self.mode = Mode::Answer(id);
            }
            pane::Choice::Send => {
                let keys = self.picked.iter().map(|&i| q.options[i].key.clone()).collect();
                self.answer(id, keys, None).await;
            }
            pane::Choice::Attach(run) => self.attach_to(run, area).await,
            pane::Choice::Dismiss => self.dismiss(id).await,
        }
    }

    /// The run that asked, in its terminal: selected on the list and joined,
    /// as `enter` on it would.
    async fn attach_to(&mut self, run: String, area: Rect) {
        self.back_to_list().await;
        let key = format!("r:{run}");
        if !self.keys().contains(&key) {
            self.say("the run that asked is no longer on the list");
            return;
        }
        self.select(Some(key));
        self.open_run(area).await;
    }

    /// "Not this". Whatever is typed on the line goes with it, as the reason.
    async fn dismiss(&mut self, id: String) {
        let note = Some(self.line.text().trim().to_string()).filter(|n| !n.is_empty());
        match self
            .rpc
            .call::<DecisionDismiss>(DecisionNote { id, note: note.clone() })
            .await
        {
            Ok(_) => {
                if note.is_some() {
                    self.line.clear();
                }
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
            Ok(0) => self.say("nothing to archive: still going"),
            Ok(_) => {
                self.say(format!("archived {what}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("archive: {e:#}")),
        }
    }

    /// Archives a job's runs, however they ended: out of the list, kept in
    /// the ledger. Shared by `^x` (`remove_selected`) and `/archive`. Takes the
    /// name and run ids rather than `&rows::Job` itself: `Job` borrows from
    /// `self`, and this needs `&mut self` to archive.
    async fn archive_job(&mut self, name: &str, ids: Vec<String>) {
        let what = format!("{name} · {} runs", ids.len());
        self.archive(ids, &what).await;
    }

    /// The second `ctrl-x` (the first only armed it): get whatever the cursor
    /// is on out of the list. An ended run or a job is archived (reversible:
    /// the ledger keeps it); a run still going is stopped, so once it has ended
    /// another `ctrl-x` archives it; a question is dismissed; a project is
    /// removed, its runs stopped and archived and its `.relay` moved aside if
    /// you say so. Nothing here is destroyed — that is retention's alone.
    async fn remove_selected(&mut self) {
        if let Some(j) = self.selected_job() {
            let ids: Vec<String> = j.runs.iter().map(|i| i.run.id.clone()).collect();
            self.archive_job(&j.name, ids).await;
        } else if let Some(i) = self.selected_run() {
            let (id, title) = (i.run.id.clone(), i.run.title.clone());
            if i.run.state.is_terminal() {
                self.archive(vec![id], &title).await;
            } else {
                self.stop(id).await;
            }
        } else if let Some(q) = self.selected_question() {
            self.dismiss(q.id.clone()).await;
        } else if self.on_everywhere() {
            self.say(format!("{} · nothing to remove", Self::NOT_A_PROJECT));
        } else if let Some(Row::Group(h)) = self.selected() {
            let (folder, name) = (h.workspace.clone(), h.name.clone());
            self.remove_project(folder, name);
        } else {
            self.say("nothing here to remove");
        }
    }

    /// The second `^x` on a project's header. Its `.relay` is asked about
    /// first, when there is one that can move, and nothing is done until the
    /// answer; with none, the project goes at once.
    fn remove_project(&mut self, folder: PathBuf, name: String) {
        if self.removing.is_some() {
            self.say("still removing a project; a moment");
            return;
        }
        let dir = rl_core::rules_dir::rules_dir(&folder.join(rl_proto::schedule::WORKSPACE_DIR), &self.home);
        if let rl_core::rules_dir::RulesDir::Movable(files) = &dir {
            let of = |kind: RuleKind| {
                self.rules
                    .iter()
                    .filter(|r| r.kind() == kind && !r.is_global() && Path::new(r.scope()) == folder)
                    .count()
            };
            let contents = remove::contents(of(RuleKind::Schedule), of(RuleKind::Reaction), files);
            let question = remove::question(&name, &contents, &rl_core::display_path(&self.home.removed_dir()));
            self.mode = Mode::Remove(Box::new(Removal { folder, name, question }));
            return;
        }
        self.start_removal(folder, name, false, remove::RulesFate::unasked(&dir));
    }

    /// Hands the removal to the daemon on a connection of its own, so the
    /// panel keeps drawing while the project's runs are stopped.
    fn start_removal(&mut self, folder: PathBuf, name: String, move_rules: bool, fate: remove::RulesFate) {
        let home = self.home.clone();
        let params = WorkspaceRemoveParams {
            path: folder,
            stop_runs: true,
            archive_runs: true,
            move_rules,
        };
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<WorkspaceRemove>(params).await
        });
        self.removing = Some(Removing { name, fate, task });
    }

    async fn removed(&mut self, done: std::result::Result<Result<WorkspaceRemoved>, tokio::task::JoinError>) {
        let Some(r) = self.removing.take() else { return };
        match done {
            Ok(Ok(out)) => self.say(remove::done(&r.name, &out, &r.fate, rl_core::display_path)),
            Ok(Err(e)) => self.say(format!("remove {}: {e:#}", r.name)),
            Err(e) => self.say(format!("remove {}: {e}", r.name)),
        }
        self.refresh().await;
    }

    /// What a second `ctrl-x` would do to the row under the cursor, for the
    /// armed hint — `None` when there is nothing to remove there.
    fn remove_hint(&self) -> Option<String> {
        if let Some(j) = self.selected_job() {
            Some(format!("remove {} · {} runs", j.name, j.runs.len()))
        } else if let Some(i) = self.selected_run() {
            if i.run.state.is_terminal() {
                Some(format!("remove {}", i.run.title))
            } else {
                Some(format!("stop {}", i.run.title))
            }
        } else if let Some(q) = self.selected_question() {
            Some(format!("dismiss {}", q.title))
        } else if self.on_everywhere() {
            None
        } else if let Some(Row::Group(h)) = self.selected() {
            Some(remove::arm(&remove::Plan::of(
                &h.name,
                &h.workspace,
                &self.digest.items,
            )))
        } else {
            None
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
                self.say(format!("archived {n}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("archive: {e:#}")),
        }
    }

    // ---- rules -------------------------------------------------------------

    /// `/graph` and `^g`: into the wiring, or back to the list, each keeping
    /// its own place. From a job or a project, the graph opens on the rule
    /// that stands there; from anywhere else — an empty list too — on the
    /// graph's own first node. The global rules have no row, so this is
    /// where they are reached.
    async fn toggle_graph(&mut self) {
        let target = self.rule_target();
        if self.view == View::Graph {
            self.go_back();
        } else {
            self.open_view(View::Graph);
            self.close_term();
        }
        self.picked.clear();
        self.events.clear();
        self.refresh().await;
        if self.view == View::Graph
            && let Some(t) = target
            && self.keys().contains(&t)
        {
            self.select(Some(t));
        }
    }

    /// The rule the list's cursor stands on, as its graph node: a job's own
    /// rule, else the first of a folder's rules — whatever waits for a yes
    /// first.
    fn rule_target(&self) -> Option<String> {
        if self.view == View::Events {
            return self.event_rule();
        }
        if !matches!(self.view, View::Work | View::CatchUp) {
            return None;
        }
        if let Some(j) = self.selected_job() {
            return self.rule_of_job(&j).map(rules::key);
        }
        let scope = match self.selected() {
            Some(Row::Group(h)) if h.is_everywhere() => rl_proto::schedule::GLOBAL_SCOPE.to_string(),
            Some(Row::Group(h)) => h.workspace.to_string_lossy().into_owned(),
            _ => return None,
        };
        let mut here: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.removed_at().is_none() && r.scope() == scope)
            .collect();
        here.sort_by_key(|r| (r.status() != RuleStatus::NeedsApproval, r.name().to_string()));
        here.first().map(|r| rules::key(r))
    }

    // ---- the views ---------------------------------------------------------

    /// Puts another view up, keeping this one's selection for when it comes
    /// back. A view already on the way here is gone back to rather than
    /// stacked again, so going back always ends at the list.
    fn open_view(&mut self, v: View) {
        if v == self.view {
            return;
        }
        match self.back.iter().position(|x| *x == v) {
            Some(i) => self.back.truncate(i),
            None => self.back.push(self.view),
        }
        self.switch_to(v);
    }

    /// Back to the view this one was opened from — the list, at the last.
    fn go_back(&mut self) {
        let v = self.back.pop().unwrap_or(View::Work);
        self.switch_to(v);
    }

    /// The view changes: the selection is kept for its view, and the events
    /// view's search swapped in or out, since its `kind:` means another thing.
    fn switch_to(&mut self, v: View) {
        let sel = self.kept.remove(&v).flatten();
        self.kept.insert(self.view, std::mem::replace(&mut self.sel, sel));
        if (self.view == View::Events) != (v == View::Events) {
            std::mem::swap(&mut self.filter, &mut self.filter_aside);
            self.search = None;
        }
        self.view = v;
    }

    /// Back to the list from whichever view is up. Leaving the catch-up so,
    /// what it showed is seen.
    async fn back_to_list(&mut self) {
        if self.view == View::Work {
            return;
        }
        self.saw_catch_up().await;
        self.back.clear();
        self.switch_to(View::Work);
        self.picked.clear();
        self.events.clear();
        self.refresh().await;
    }

    /// The tick: the list, and while the events view is up, what is new in
    /// the ledger.
    async fn tick(&mut self) {
        self.refresh().await;
        if self.view == View::Events {
            self.fetch_events().await;
        }
    }

    // ---- the events view ---------------------------------------------------

    /// `/events`: the list gives way to every event, newest first, and what
    /// each set off; `/events` again or `esc` goes back.
    async fn toggle_events(&mut self) {
        if self.view == View::Events {
            self.go_back();
        } else {
            self.open_view(View::Events);
            self.close_term();
            // Asked again when the view opens: something may be back.
            self.feed.gone.clear();
            self.fetch_events().await;
        }
        self.picked.clear();
        self.events.clear();
        self.refresh().await;
    }

    // ---- the catch-up -----------------------------------------------------

    /// What happened since `since`, in the list's place until you leave it:
    /// `esc` goes to the list, `enter` to the row there. It reads a digest of
    /// its own, counted from `since`; the list's stays as it is. `since` is
    /// also where a project's or everything's `/summary` starts.
    async fn open_catch_up(&mut self, since: Timestamp) {
        let d = match self.rpc.call::<DigestGet>(DigestParams { since: Some(since) }).await {
            Ok(d) => d,
            Err(e) => return self.say(format!("daemon: {e:#}")),
        };
        // Another stretch over the one up: that one is left, and what it
        // showed is seen.
        if self.view == View::CatchUp {
            self.saw_catch_up().await;
        }
        self.away_since = Some(since);
        self.catch = Some(d);
        self.write_top(since).await;
        self.open_view(View::CatchUp);
        self.close_term();
        self.picked.clear();
        self.events.clear();
        self.settle();
        self.load_pane().await;
    }

    /// The catch-up as it stands, built on the list's own rows and narrowed
    /// by the search, as the list is.
    fn catch_up(&self) -> Option<crate::catchup::CatchUp<'_>> {
        let d = self.catch.as_ref()?;
        let cx = rows::Context {
            names: &self.names,
            rules: &self.rules,
            filter: &self.filter,
            lens: rows::Lens::All,
            now: Timestamp::now(),
        };
        Some(crate::catchup::CatchUp::build(d, &cx))
    }

    /// The catch-up's rows in order: each project's line, then its rows.
    fn catch_rows(&self) -> Vec<Row<'_>> {
        let Some(cu) = self.catch_up() else {
            return vec![];
        };
        cu.projects
            .into_iter()
            .flat_map(|p| std::iter::once(Row::Group(p.head)).chain(p.rows))
            .collect()
    }

    /// What leaving the catch-up marks seen: every run it shows that was
    /// not, and where each job's firings not seen till now start, so its
    /// `/summary` still starts there (as `saw_selected` notes).
    fn catch_up_unseen(&self) -> Shown {
        let mut shown = Shown::default();
        for r in self.catch_rows() {
            match r {
                Row::Run(i) if i.unseen => shown.ids.push(i.run.id.clone()),
                Row::Job(j) => {
                    let unseen: Vec<&DigestItem> = j.runs.iter().copied().filter(|i| i.unseen).collect();
                    if let Some(at) = unseen.iter().map(|i| i.run.started_at).min() {
                        shown.from.push((j.key.clone(), at));
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
    /// showed is seen now.
    async fn saw_catch_up(&mut self) {
        if self.view != View::CatchUp && !self.back.contains(&View::CatchUp) {
            return;
        }
        let shown = self.catch_up_unseen();
        self.unseen_from.extend(shown.from);
        self.seen(shown.ids).await;
    }

    /// `enter` in the catch-up: the row on the list, with the cursor on it
    /// (`^g` from there is a job's rule in the graph). A run the list no
    /// longer has keeps the catch-up up, and says why.
    async fn catch_up_enter(&mut self) {
        let Some(row) = self.selected() else {
            return self.say("nothing here to open");
        };
        let key = row.key();
        let run = row.item().map(|i| i.run.id.clone());
        match run {
            Some(id) => self.show_run(&id).await,
            None => {
                self.back_to_list().await;
                if let Some(k) = &key
                    && !self.keys().contains(k)
                {
                    // A search or a lens hiding it gives way, as for a run.
                    self.filter.clear();
                    self.lens = rows::Lens::All;
                }
                self.select(key);
            }
        }
    }

    fn event_items(&self) -> Vec<events::Item<'_>> {
        events::build(&self.feed, &self.rules, &self.names, &self.filter)
    }

    /// What is new in the ledger since the newest event read — the newest
    /// page when nothing is — and what is known of what it names. When the
    /// view opens and on the tick; never because the cursor moved.
    async fn fetch_events(&mut self) {
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
    async fn fetch_older(&mut self) {
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
    async fn fill_feed(&mut self) {
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
    async fn events_step(&mut self, code: KeyCode) -> bool {
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
    async fn events_mouse(&mut self, m: MouseEvent) {
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
    fn event_target(&self) -> Option<events::Target> {
        let key = self.sel.as_deref()?;
        events::target(&self.event_items(), &self.feed, key)
    }

    /// The rule the line under the cursor stands for, as its graph node: the
    /// one behind what it set off, or behind its run.
    fn event_rule(&self) -> Option<String> {
        let (kind, name, near) = match self.event_target()? {
            events::Target::Rule(kind, name, near) => (kind, name, near),
            events::Target::Run(id) => {
                let r = self.feed.runs.get(&id)?;
                match &r.cause {
                    Cause::Schedule(n) => (RuleKind::Schedule, n.clone(), Some(r.workspace.clone())),
                    Cause::Reaction(n) => (RuleKind::Reaction, n.clone(), Some(r.workspace.clone())),
                    _ => return None,
                }
            }
        };
        events::rule_key(&self.rules, kind, &name, near.as_deref())
    }

    /// `enter` in the events view: an event goes to its run on the list, a
    /// line a rule set off to that rule in the graph (as `^g` there does).
    async fn events_enter(&mut self) {
        match self.event_target() {
            Some(events::Target::Run(id)) => self.show_run(&id).await,
            Some(events::Target::Rule(_, name, _)) => {
                if self.event_rule().is_some() {
                    self.toggle_graph().await;
                } else {
                    self.say(format!("{name} is not a rule any more"));
                }
            }
            None => self.say("nothing here to open"),
        }
    }

    /// The list, with the cursor on a run: its own row, or its job's. When
    /// the lens or the search hides it, they give way; when the list does
    /// not have it at all, the view stays and says why.
    async fn show_run(&mut self, id: &str) {
        let title = self
            .feed
            .runs
            .get(id)
            .or_else(|| self.run_by_id(id))
            .or_else(|| {
                let catch = self.catch.as_ref()?;
                catch.items.iter().map(|i| &i.run).find(|r| r.id == id)
            })
            .map_or_else(|| id.to_string(), |r| r.title.clone());
        let list_filter = match self.view {
            View::Events => self.filter_aside.clone(),
            _ => self.filter.clone(),
        };
        let (key, opened) = match self.list_key(id, &list_filter, self.lens) {
            Some(k) => (k, None),
            None => match self
                .list_key(id, "", rows::Lens::All)
                .map(|k| (k, rows::Lens::All))
                .or_else(|| {
                    self.list_key(id, "", rows::Lens::Hidden)
                        .map(|k| (k, rows::Lens::Hidden))
                }) {
                Some((k, lens)) => (k, Some(lens)),
                None => {
                    // As it stands now: it may have been put away since it was read.
                    if let Ok(r) = self.rpc.call::<RunGet>(RunId { id: id.to_string() }).await {
                        self.feed.runs.insert(id.to_string(), r);
                    }
                    let archived = self.feed.runs.get(id).is_some_and(|r| r.archived_at.is_some());
                    return self.say(if archived {
                        format!("'{title}' is archived: kept in the ledger, off the list")
                    } else {
                        format!("'{title}' is off the list: it ended over a day ago")
                    });
                }
            },
        };
        self.back_to_list().await;
        if let Some(lens) = opened {
            self.filter.clear();
            self.lens = lens;
            self.say(if lens == rows::Lens::Hidden {
                format!("'{title}' is hidden: here in the hidden lens, /hide brings it back")
            } else {
                format!("the search and the lens let go, to show '{title}'")
            });
        }
        self.select(Some(key));
    }

    /// The list's row for a run, as the list would stand with this search
    /// and lens: its own, or the job it folds into.
    fn list_key(&self, id: &str, filter: &str, lens: rows::Lens) -> Option<String> {
        let cx = rows::Context {
            names: &self.names,
            rules: &self.rules,
            filter,
            lens,
            now: Timestamp::now(),
        };
        rows::build(&self.digest, &cx)
            .iter()
            .find(|r| match r {
                Row::Run(i) => i.run.id == id,
                Row::Job(j) => j.runs.iter().any(|i| i.run.id == id),
                _ => false,
            })
            .and_then(|r| r.key())
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

    /// `/reload rules`, and after `/edit`: every rules file read again, and
    /// what did not load said — one in the flash by name, more by count with
    /// the pane listing each with whose it is, its file and line.
    async fn reload_rules(&mut self) {
        let s = self.rpc.call::<ScheduleReload>(()).await;
        let r = self.rpc.call::<ReactionReload>(()).await;
        match (s, r) {
            (Ok(s), Ok(r)) => {
                self.say(rules::reload_note(&s, &r));
                let bad: Vec<NotLoaded> = s.not_loaded.into_iter().chain(r.not_loaded).collect();
                if bad.len() > 1 {
                    self.pane = Pane::Facts;
                    self.show_pane = true;
                    self.reload_list = Some(bad);
                }
                self.refresh().await;
            }
            (Err(e), _) | (_, Err(e)) => self.say(format!("reload: {e:#}")),
        }
    }

    /// What did not load in one folder's files, or (`None`) the Relay home's.
    fn broken_in(&self, scope: Option<&Path>) -> Vec<&NotLoaded> {
        self.not_loaded
            .iter()
            .filter(|n| match scope {
                Some(p) => !n.is_global() && Path::new(&n.scope) == p,
                None => n.is_global(),
            })
            .collect()
    }

    /// Whether a header's files — a folder's, or `everywhere`'s global ones —
    /// did not all load.
    fn header_broken(&self, h: &rows::Head) -> bool {
        let scope = (!h.is_everywhere()).then_some(h.workspace.as_path());
        !self.broken_in(scope).is_empty()
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
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn new_rule_form(&mut self) {
        let folder = self.folder_here();
        // A new rule is global from the `everywhere` header, and from a
        // global rule's node in the graph; else
        // it belongs to the folder here, and `in: global` typed into the form
        // makes it global from anywhere.
        let yours = self.on_everywhere()
            || (self.view == View::Graph
                && self.selected_node().is_some_and(|n| n.folder.is_none())
                && self.rule_here().is_none_or(Rule::is_global));
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

    /// `/edit [schedules|reactions [global]]`: a rule file in `$EDITOR`, once
    /// the screen is free — never the form, which is `/new`'s. Bare, the rule
    /// under the cursor (a job row, a graph node) opens its own file at its
    /// line, and anything else the schedules of the folder the cursor stands
    /// in. Any row answers which folder its work is in, so this reads the same
    /// from a run as from the folder's own header, and a node in the graph
    /// answers with its own folder. Where nothing names a folder — the
    /// `everywhere` header, an empty list, a global rule's node — it is the
    /// Relay home's file; `global` asks for that from anywhere
    /// (`rulefile::edit_target`).
    fn edit_file(&mut self, words: &str) {
        let folder = self
            .selected_folder()
            .or_else(|| self.selected_node().and_then(|n| n.folder.clone()));
        let rule = self.rule_here().map(|r| rulefile::Here {
            source: r.source(),
            kind: r.kind(),
            name: r.name(),
        });
        // A file that did not load opens where it broke.
        let broken: Vec<(&Path, usize)> = self
            .not_loaded
            .iter()
            .map(|n| (n.file.as_path(), n.line.unwrap_or(1)))
            .collect();
        match rulefile::edit_target(&self.home, words, rule, folder.as_deref(), &broken) {
            Ok(at) => self.editor = Some(at),
            Err(e) => self.say(e),
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

    /// `enter` on a name being changed: saved, or why not — the name stays
    /// up to be typed again then. One left as it was has nothing to save.
    async fn save_rename(&mut self, r: Box<Renaming>) {
        let name = r.field.text().trim().to_string();
        if name == r.was {
            return;
        }
        let outcome = match &r.what {
            Renamed::Project(folder) => self.rename_project(folder.clone(), name).await,
            Renamed::Run(id) => self.rename_run(id.clone(), name).await,
        };
        if let Err(e) = outcome {
            self.say(format!("{e:#}"));
            self.mode = Mode::Rename(r);
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
            cols: Some(spawn_size(area).0),
            rows: Some(spawn_size(area).1),
            env: crate::env::child_env(&[])?,
            inherit_env: false,
            input: true,
            cause: Cause::Manual,
            flow: None,
        };
        let run = self.rpc.call::<RunStart>(spec).await?;
        if run.kind == RunKind::Agent {
            self.say(format!("started {} · enter joins it", run.title));
        } else {
            self.say(format!("started {}", run.title));
        }
        self.back_to_list().await;
        self.refresh().await;
        self.select(Some(format!("r:{}", run.id)));
        Ok(())
    }

    /// Run a shell line in a project's warm shell: write it to the shell's
    /// stdin (starting the shell if the folder has none) and put the cursor
    /// on it. What it prints is the shell's own, read by joining it.
    async fn run_in_shell(&mut self, cmd: String, folder: PathBuf, area: Rect) -> Result<()> {
        let id = self.shell_target(folder, area).await?;
        let mut bytes = cmd.into_bytes();
        bytes.push(b'\n');
        self.rpc
            .call::<RunInput>(RunInputParams { id: id.clone(), bytes })
            .await?;
        self.refresh().await;
        self.select(Some(format!("r:{id}")));
        let title = self.run_by_id(&id).map(|r| r.title.clone()).unwrap_or(id);
        self.say(format!("sent to {title} · enter joins it"));
        Ok(())
    }

    /// Where a shell line goes: the selected shell if the cursor is on one,
    /// else the folder's current shell, started if it has none yet.
    async fn shell_target(&mut self, folder: PathBuf, area: Rect) -> Result<String> {
        if let Some(id) = self.selected_shell() {
            return Ok(id);
        }
        if let Some(id) = self.project_shell(&folder) {
            return Ok(id);
        }
        self.spawn_shell(folder, area).await
    }

    /// The live warm shells in `folder`, oldest first. The list itself is
    /// ranked by attention, which moves as runs change state, so the order
    /// here is by when each shell started — numbering has to be steady.
    fn project_shells(&self, folder: &Path) -> Vec<String> {
        let mut live: Vec<(Timestamp, String)> = self
            .digest
            .items
            .iter()
            .filter(|i| !i.run.state.is_terminal())
            .filter(|i| i.run.workspace == folder)
            .filter(|i| i.run.tags.iter().any(|t| t == SHELL_TAG))
            .map(|i| (i.run.started_at, i.run.id.clone()))
            .collect();
        live.sort_by_key(|(at, _)| *at);
        live.into_iter().map(|(_, id)| id).collect()
    }

    /// The folder's current shell: the one last opened while it is still
    /// live, else the oldest one running.
    fn project_shell(&self, folder: &Path) -> Option<String> {
        let shells = self.project_shells(folder);
        match self.shell_at.get(folder) {
            Some(at) if shells.iter().any(|id| id == at) => Some(at.clone()),
            _ => shells.into_iter().next(),
        }
    }

    /// The run under the cursor, when it is a warm shell.
    fn selected_shell(&self) -> Option<String> {
        let i = self.selected_run()?;
        i.run.tags.iter().any(|t| t == SHELL_TAG).then(|| i.run.id.clone())
    }

    /// `/shell`: another shell in this folder, and what you type next goes to
    /// it rather than to the one it joins.
    async fn new_shell(&mut self, area: Rect) {
        if self.on_everywhere() {
            self.say(format!("{} · /shell on a project opens one there", Self::NOT_A_PROJECT));
            return;
        }
        let folder = self.folder_here();
        match self.spawn_shell(folder.clone(), area).await {
            Ok(id) => {
                self.shell_at.insert(folder, id.clone());
                self.refresh().await;
                self.select(Some(format!("r:{id}")));
                self.say("a new shell · what you type goes to it · enter joins it");
            }
            Err(e) => self.say(format!("shell: {e:#}")),
        }
    }

    /// Start a project's warm shell: a persistent `$SHELL -i` tagged so the
    /// list and the router know it for what it is.
    async fn spawn_shell(&mut self, folder: PathBuf, area: Rect) -> Result<String> {
        // The first is `shell`; the rest are numbered, so one of a folder's
        // shells reads differently from another in the list.
        let n = self.project_shells(&folder).len();
        let title = if n == 0 {
            "shell".to_string()
        } else {
            format!("shell {}", n + 1)
        };
        let spec = RunSpec {
            argv: crate::env::shell_i(),
            kind: RunKind::Command,
            cwd: folder,
            title: Some(title),
            ref_: None,
            tags: vec![SHELL_TAG.into()],
            tty: true,
            cols: Some(spawn_size(area).0),
            rows: Some(spawn_size(area).1),
            env: crate::env::child_env(&[])?,
            inherit_env: false,
            input: true,
            cause: Cause::Manual,
            flow: None,
        };
        Ok(self.rpc.call::<RunStart>(spec).await?.id)
    }

    /// Register a folder as a project from the line — the panel's `rl up`. Its
    /// rules load; its schedules wait for a yes (`/approve` on its node in the graph). The cursor lands on
    /// the new row (empty until something runs there, thanks to project rows).
    async fn add_dir(&mut self, arg: &str) -> Result<()> {
        let arg = arg.trim();
        if arg.is_empty() {
            self.say("a folder: /up <path>");
            return Ok(());
        }
        let expanded = match arg.strip_prefix('~') {
            Some(rest) => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
                None => PathBuf::from(arg),
            },
            None => PathBuf::from(arg),
        };
        let path = match std::fs::canonicalize(&expanded) {
            Ok(p) => p,
            Err(_) => {
                self.say(format!("no such folder: {}", expanded.display()));
                return Ok(());
            }
        };
        self.rpc
            .call::<WorkspaceUp>(WorkspaceUpParams {
                path: path.clone(),
                approve: false,
            })
            .await?;
        self.refresh().await;
        self.say(format!("added {}", self.names.workspace(&path)));
        self.select(Some(format!("g:{}", path.display())));
        Ok(())
    }

    /// Give a project a new name — or, given none, its folder's own back, the
    /// one `rl up` registered it under. The cursor stays on it.
    async fn rename_project(&mut self, folder: PathBuf, name: String) -> Result<()> {
        let back = name.is_empty();
        let name = if back {
            let own = folder.file_name().unwrap_or(folder.as_os_str());
            own.to_string_lossy().into_owned()
        } else {
            name
        };
        self.rpc
            .call::<WorkspaceRename>(WorkspaceRenameParams {
                path: folder.clone(),
                name: name.clone(),
            })
            .await?;
        self.say(format!("{} {name}", if back { "back to" } else { "renamed to" }));
        self.refresh().await;
        self.select(Some(format!("g:{}", folder.display())));
        Ok(())
    }

    /// `^p`: the run under the cursor to the top of the list, or back among
    /// the rest. The cursor goes with it.
    async fn toggle_pin(&mut self) {
        let Some((id, pinned)) = self
            .selected_run()
            .map(|i| (i.run.id.clone(), i.run.pinned_at.is_some()))
        else {
            self.say("^t pins a run");
            return;
        };
        match self
            .rpc
            .call::<RunPin>(RunPinParams {
                id: id.clone(),
                pinned: !pinned,
            })
            .await
        {
            Ok(r) => self.say(if pinned {
                format!("unpinned {}", r.title)
            } else {
                format!("pinned {}", r.title)
            }),
            Err(e) => self.say(format!("pin: {e}")),
        }
        self.refresh().await;
        self.select(Some(format!("r:{id}")));
    }

    /// `/hide`: the row under the cursor off the list — a run, going or
    /// ended; a job, its firings to come too; a project and everything in it.
    /// Nothing is stopped or removed. In the `hidden` lens it is the way back:
    /// the row comes back, and a header that is not hidden itself brings back
    /// what was hidden under it. A question is never hidden: it needs you.
    async fn hide(&mut self) {
        let back = self.lens == rows::Lens::Hidden;
        let rows = self.rows();
        let Some(at) = self
            .sel
            .as_ref()
            .and_then(|k| rows.iter().position(|r| r.key().as_ref() == Some(k)))
        else {
            self.say("nothing here to hide");
            return;
        };
        // What to hide or bring back: runs by id, projects and jobs by key.
        let mut ids: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        let hidden = |k: &str| self.digest.hidden.iter().any(|h| h.key == k);
        let own_hidden = |i: &DigestItem| i.run.hidden_at.is_some();
        let mut still: Option<&str> = None;
        let what = match &rows[at] {
            Row::Question(_) => {
                self.say("a question is never hidden: it needs you · answer it, or ^x lets it go");
                return;
            }
            Row::Run(i) => {
                if back {
                    still = rows::hidden_with(&i.run, &self.names, &self.rules, &self.digest.hidden);
                    if own_hidden(i) {
                        ids.push(i.run.id.clone());
                    }
                } else {
                    ids.push(i.run.id.clone());
                }
                i.run.title.clone()
            }
            Row::Job(j) => {
                if !back || hidden(&j.key) {
                    keys.push(j.key.clone());
                }
                if back {
                    ids.extend(j.runs.iter().filter(|i| own_hidden(i)).map(|i| i.run.id.clone()));
                }
                j.name.clone()
            }
            Row::Group(h) => {
                let key = rows[at].key().unwrap_or_default();
                if !back || hidden(&key) {
                    keys.push(key);
                } else {
                    // Not hidden itself: what was hidden under it comes back.
                    for r in rows[at + 1..].iter().take_while(|r| !matches!(r, Row::Group(_))) {
                        match r {
                            Row::Run(i) if own_hidden(i) => ids.push(i.run.id.clone()),
                            Row::Job(j) => {
                                if hidden(&j.key) {
                                    keys.push(j.key.clone());
                                }
                                ids.extend(j.runs.iter().filter(|i| own_hidden(i)).map(|i| i.run.id.clone()));
                            }
                            _ => {}
                        }
                    }
                }
                h.name.clone()
            }
        };
        if ids.is_empty() && keys.is_empty() {
            let why = still.map_or_else(String::new, |w| format!(": it is hidden with {w} · /hide on that"));
            self.say(format!("nothing to bring back in {what}{why}"));
            return;
        }
        // Where the cursor goes once the row has left: the next one, else the
        // one before.
        let next = rows[at + 1..]
            .iter()
            .chain(rows[..at].iter().rev())
            .find_map(|r| r.key());
        let here = rows[at].key();
        drop(rows);
        let mut failed = None;
        for id in ids {
            if let Err(e) = self.rpc.call::<RunHide>(RunHideParams { id, hidden: !back }).await {
                failed = Some(e);
            }
        }
        for key in keys {
            if let Err(e) = self
                .rpc
                .call::<DigestHide>(DigestHideParams { key, hidden: !back })
                .await
            {
                failed = Some(e);
            }
        }
        match failed {
            Some(e) => self.say(format!("hide: {e:#}")),
            None if back => self.say(match still {
                Some(w) => format!("brought back {what} · still hidden with {w}"),
                None => format!("brought back {what}"),
            }),
            None => self.say(format!("hid {what} · the hidden lens has it")),
        }
        self.refresh().await;
        if self.keys().contains(here.as_ref().unwrap_or(&String::new())) {
            self.select(here);
        } else if next.as_ref().is_some_and(|k| self.keys().contains(k)) {
            self.select(next);
        }
    }

    /// Shift-↑/↓: the row under the cursor one place up or down inside its
    /// project, which keeps that order from then on (`digest.order`). The
    /// cursor stays on the row, and holding the key goes on moving it. The
    /// order kept comes back at once, so the next press builds on it without
    /// waiting for the list to be fetched again.
    async fn reorder(&mut self, up: bool) {
        match self.view {
            View::Work => {}
            View::Graph => return self.say("⇧↑↓ moves rows on the list · the graph lays itself out"),
            View::Events => return self.say("⇧↑↓ moves rows on the list · events stay in the order they came"),
            View::CatchUp => return self.say("⇧↑↓ moves rows on the list · esc goes there"),
        }
        if self.lens == rows::Lens::Hidden {
            self.say("the hidden lens keeps the list's order · move it on the list");
            return;
        }
        let Some(key) = self.sel.clone() else {
            self.say("nothing here to move");
            return;
        };
        let order = {
            let shown = self.rows();
            let full = rows::build(
                &self.digest,
                &rows::Context {
                    names: &self.names,
                    rules: &self.rules,
                    filter: "",
                    lens: rows::Lens::All,
                    now: Timestamp::now(),
                },
            );
            rows::reorder(&shown, &full, &self.digest.order, &key, up)
        };
        match order {
            Ok(order) => match self.rpc.call::<DigestOrder>(order).await {
                Ok(all) => self.digest.order = all,
                Err(e) => self.say(format!("reorder: {e:#}")),
            },
            Err(why) => self.say(why),
        }
    }

    /// `/sort`: the project under the cursor — its header, or any row in it
    /// — lets go of the order you put it in and sorts itself again.
    async fn sort_project(&mut self) {
        let found = {
            let rows = self.rows();
            let at = self
                .sel
                .as_ref()
                .and_then(|k| rows.iter().position(|r| r.key().as_ref() == Some(k)));
            at.and_then(|at| rows[..=at].iter().rposition(|r| matches!(r, Row::Group(_))))
                .and_then(|h| match &rows[h] {
                    Row::Group(head) => Some((rows[h].key().unwrap_or_default(), head.name.clone())),
                    _ => None,
                })
        };
        let Some((group, name)) = found else {
            self.say("/sort is for a project: on its header, it lets the project sort itself again");
            return;
        };
        if !self.digest.order.iter().any(|o| o.group == group) {
            self.say(format!("{name} already sorts itself · ⇧↑↓ puts its rows in your order"));
            return;
        }
        match self
            .rpc
            .call::<DigestOrder>(rl_proto::methods::Order {
                group,
                rows: Vec::new(),
            })
            .await
        {
            Ok(all) => {
                self.digest.order = all;
                self.say(format!(
                    "{name} sorts itself again · what needs you, then by state and time"
                ));
            }
            Err(e) => self.say(format!("sort: {e:#}")),
        }
    }

    /// A run's own name, which is its title. The ledger is the only place it
    /// lives, so the rename is one call and the next refresh shows it. Given
    /// none, the run goes back to the name Relay gave it.
    async fn rename_run(&mut self, id: String, title: String) -> Result<()> {
        let back = title.is_empty();
        let r = self
            .rpc
            .call::<RunRename>(RunRenameParams { id: id.clone(), title })
            .await?;
        self.say(format!("{} {}", if back { "back to" } else { "renamed to" }, r.title));
        self.refresh().await;
        self.select(Some(format!("r:{id}")));
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
        if self.view == View::Graph {
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
            Result<(crate::interpret::Draft, rl_proto::methods::AssistantOutput)>,
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

    /// `/summary [deep] [stretch]`: what happened in the row under the
    /// cursor — a run, a job's runs, a project's, or everything on the
    /// `everywhere` header — brief from the recaps or deep from the logs,
    /// since you last looked unless a stretch is typed. Deep over more than
    /// one assistant call says how many first and waits for `y`.
    async fn summary(&mut self, arg: &str) {
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
            } else {
                "/summary works on a run, a job, a project, everything or a question"
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
        // it); a job's, from the firing you had not seen when the cursor
        // landed on it, else the daemon's; a project's and everything's,
        // since you were here.
        let from = match target {
            SummaryTarget::Project { .. } | SummaryTarget::All => Some(
                typed
                    .or(self.away_since)
                    .unwrap_or(now - SignedDuration::from_hours(24)),
            ),
            SummaryTarget::Job { .. } if typed.is_none() => {
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
        if deep {
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
    fn summary_start(&mut self, params: SummaryParams) {
        // A job's note of where its unseen firings began is used once asked.
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
            SummaryTarget::Job { rule, name, .. } => format!("{rule} {name}"),
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
        // that stays open, a step away. The top's is in the top.
        if !self.on_top() {
            self.leave_term();
            self.show_pane = true;
        }
        self.summarising = Some(Summarising {
            target,
            title,
            deep,
            from,
            task,
        });
    }

    async fn summarised(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
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

    /// The catch-up's top for its stretch from `since`: the one kept when it
    /// is up to date; else, when `catch_up.summary` is `auto` and there is
    /// an assistant to ask, one asked for — the counted bullets stand until
    /// it lands; else the one kept, moved on or not, or the bullets.
    async fn write_top(&mut self, since: Timestamp) {
        if let Some(t) = self.topping.take() {
            t.task.abort();
        }
        self.top = None;
        let params = SummaryParams {
            target: SummaryTarget::All,
            deep: false,
            from: Some(since),
            again: false,
            dry: false,
            timeout: None,
        };
        let kept = self
            .rpc
            .call::<SummaryGet>(params.clone())
            .await
            .ok()
            .flatten()
            .filter(crate::catchup::top_worthy);
        if kept.as_ref().is_some_and(|k| !k.stale) {
            self.top = kept;
            return;
        }
        let auto = match self.rpc.call::<rl_proto::methods::ConfigGet>(()).await {
            Ok(c) => c.config.catch_up.auto_summary() && c.summary.is_some_and(|a| a.available),
            Err(_) => false,
        };
        // No run in it: nothing to ask anyone about.
        if !auto || !self.catch_up().is_some_and(|cu| cu.has_runs()) {
            self.top = kept;
            return;
        }
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
            task,
        });
    }

    /// The catch-up's top landed: it takes the bullets' place while its
    /// stretch is the one shown. Failing, the bullets stay and a word says
    /// why.
    async fn topped(&mut self, done: std::result::Result<Result<Summary>, tokio::task::JoinError>) {
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
    fn top_from(&mut self, s: &Summarising, summary: &Summary) {
        let shown = self.catch.as_ref().map(|c| c.since.as_millisecond());
        if s.target == SummaryTarget::All
            && s.from.map(|f| f.as_millisecond()) == shown
            && crate::catchup::top_worthy(summary)
        {
            self.top = Some(summary.clone());
        }
    }

    /// `/since [stretch]`: the catch-up over the stretch typed, else since
    /// you were last here, else the last day — from any view; `esc` goes to
    /// the list.
    async fn since(&mut self, arg: &str) {
        let now = Timestamp::now();
        let since = match arg.trim() {
            "" => self.left.unwrap_or(now - SignedDuration::from_hours(24)),
            typed => match rl_core::time::parse_since(typed, now) {
                Ok(t) => t,
                Err(e) => return self.say(format!("{e:#}")),
            },
        };
        self.open_catch_up(since).await;
    }

    // ---- keys --------------------------------------------------------------

    async fn key(&mut self, mut k: KeyEvent, area: Rect) -> Result<()> {
        self.flash = None;
        // Like the flash, the list of what a reload did not load lasts one key.
        self.reload_list = None;
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
        if !matches!(self.mode, Mode::Normal | Mode::Answer(_) | Mode::Rename(_)) {
            self.shortcuts = false;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Stop(id) => {
                if k.code == KeyCode::Char('y') {
                    self.stop(id).await;
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
            Mode::Fire(name) => {
                if k.code == KeyCode::Char('y') {
                    self.fire(name).await;
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
            Mode::Remove(r) => {
                let Removal { folder, name, .. } = *r;
                match k.code {
                    KeyCode::Char('y') => self.start_removal(folder, name, true, remove::RulesFate::Moved),
                    KeyCode::Char('n') => self.start_removal(folder, name, false, remove::RulesFate::Kept),
                    _ => self.say(format!("{name} is left as it was")),
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
                // Nothing is typed here, so `?` is the shortcuts', and `esc`
                // puts them away before it puts the question back.
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
            Mode::Form(mut f) => {
                match f.key(&k) {
                    Act::Typing => self.mode = Mode::Form(f),
                    Act::Cancel => {}
                    Act::Save => self.save_form(f, area).await,
                }
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
                // it does on the list: `tab` steps the lens and `^j`/`^k`
                // move, whatever is typed on the line below.
                line::Act::Complete | line::Act::Nothing => {
                    if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
                        self.cycle_lens(if k.code == KeyCode::BackTab { -1 } else { 1 });
                        return Ok(());
                    }
                    k = vi_step(k, true);
                }
            },
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
        // Watching, the line still has the keyboard, so you can type the next
        // thing without leaving the run you are on; taking the keyboard is the
        // handover to the real terminal (`raw::attach`), never keys here.
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
        // In the graph, with nothing typed, left and right follow the wiring
        // rather than move a cursor through an empty line.
        if self.view == View::Graph
            && self.line.is_empty()
            && matches!(self.mode, Mode::Normal)
            && matches!(k.code, KeyCode::Left | KeyCode::Right)
        {
            self.graph_step(if k.code == KeyCode::Left {
                graph::Dir::Left
            } else {
                graph::Dir::Right
            });
            return Ok(());
        }
        // Candidates on offer: the arrows walk them, not the list.
        if self.cands().is_some() && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            self.walk_cands(if k.code == KeyCode::Down { 1 } else { -1 });
            return Ok(());
        }
        // The line takes the keyboard first; what it leaves, the list has.
        match self.line.key(&k) {
            line::Act::Typing => {
                self.completing = None;
                self.pick = None;
                self.shortcuts = shortcuts::still_open(self.shortcuts, self.line.is_empty());
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
                self.completing = None;
                self.pick = None;
                Ok(())
            }
            // Full screen, the term has the keyboard: the keys scroll it.
            // `esc` is the one it does not get — that closes the pane, below —
            // and `^o`, which hides it.
            _ if self.pane == Pane::Term
                && self.wide
                && self.term.is_some()
                && k.code != KeyCode::Esc
                && !(k.code == KeyCode::Char('o') && k.modifiers.contains(KeyModifiers::CONTROL)) =>
            {
                let page = area.height.saturating_sub(3).max(1);
                self.watch_key(&k, page);
                Ok(())
            }
            // A tile beside the list: the list keeps the keyboard, `pgup` and
            // `pgdn` read the tile back, and `esc` goes on to close the pane.
            _ if self.pane == Pane::Term && self.term.is_some() => {
                // A page is the tile's own height, not the screen's.
                let page = self.pane_size(area).1;
                match k.code {
                    KeyCode::PageUp | KeyCode::PageDown => {
                        self.watch_key(&k, page);
                        Ok(())
                    }
                    _ => self.key_list(k).await,
                }
            }
            _ => self.key_list(k).await,
        }
    }

    /// `?` and `esc`, for the shortcuts line, before anything else has them:
    /// whether they were its.
    fn shortcut_key(&mut self, k: &KeyEvent, line_empty: bool) -> bool {
        match shortcuts::take(self.shortcuts, line_empty, k) {
            shortcuts::Take::Toggle => self.shortcuts = !self.shortcuts,
            shortcuts::Take::Close => self.shortcuts = false,
            shortcuts::Take::Pass => return false,
        }
        true
    }

    /// `^f` or `/filter`: search, starting from the filter already in force.
    fn open_search(&mut self) {
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
    async fn key_search(&mut self, k: KeyEvent) -> Result<()> {
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
    /// rules, the branches — for `tab` in a search and after `/filter`.
    fn search_values(&self) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        let mut projects: Vec<String> = self.names.paths().iter().map(|p| self.names.workspace(p)).collect();
        projects.push("global".into());
        let rules: Vec<String> = self.rules.iter().map(|r| r.name().to_string()).collect();
        let branches: Vec<String> = (self.digest.checkouts.iter().filter_map(|c| c.branch.clone()))
            .chain(self.digest.items.iter().filter_map(|i| i.run.branch.clone()))
            .collect();
        // The events view's `kind:` is an event's; elsewhere, the list's own.
        let kinds = match self.view {
            View::Events => self.feed.kinds(),
            _ => Vec::new(),
        };
        (projects, rules, branches, kinds)
    }

    /// `tab` in a search: the token names, then a token's values — states,
    /// kinds, projects, rules. One fits: taken. Several: what they share, and
    /// the rest shown in the footer. None — a plain word, or no word at all
    /// after one — and it goes to the next match instead.
    fn complete_search(&mut self) {
        let (projects, rules, branches, kinds) = self.search_values();
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let (text, cur) = (s.line.text().to_string(), s.line.cursor());
        let start = text[..cur].rfind(' ').map_or(0, |i| i + 1);
        let word = &text[start..cur];
        let cands = query::complete(word, &projects, &rules, &branches, &kinds);
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
    async fn key_list(&mut self, k: KeyEvent) -> Result<()> {
        // A destructive key is armed by its first press and acts on its second;
        // any other key here disarms it, so `ctrl-x`/`ctrl-c` never fire on one
        // slip (the way an agent view asks before it deletes).
        let armed = self.arm.take();
        if let Some(up) = reorder_step(&k) {
            self.reorder(up).await;
            return Ok(());
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
                if armed == Some(Arm::Remove) {
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
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => self.toggle_graph().await,
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => self.toggle_pin().await,
            (KeyCode::Esc, _) => {
                // The pane goes first, whatever stands open behind it: it is
                // the largest thing on screen, so it is what `esc` should
                // take away. The term view itself lives on, so stepping back
                // into it stays instant.
                if self.show_pane {
                    self.wide = false;
                    self.hide_pane();
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.settle();
                } else if self.view == View::Graph {
                    self.toggle_graph().await;
                } else if self.view == View::Events {
                    self.toggle_events().await;
                } else if self.view == View::CatchUp {
                    self.back_to_list().await;
                }
            }
            (KeyCode::Down, _) if self.view == View::Graph => self.graph_step(graph::Dir::Down),
            (KeyCode::Up, _) if self.view == View::Graph => self.graph_step(graph::Dir::Up),
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
    async fn enter_row(&mut self, area: Rect) -> Result<()> {
        if self.view == View::Graph {
            self.graph_enter();
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
        if self.selected_run().is_some() {
            self.open_run(area).await;
            return Ok(());
        }
        if self.selected_job().is_some() {
            // A job never grows into its runs on the list: they are in the
            // pane, with the rule and what the last firing raised.
            self.pane = Pane::Facts;
            self.show_pane = true;
        } else if let Some(q) = self.selected_question() {
            let id = q.id.clone();
            self.picked.clear();
            // The choices are in the pane: the arrows walk them from the first.
            self.answer_at = 0;
            self.pane = Pane::Facts;
            self.show_pane = true;
            self.mode = Mode::Answer(id);
        } else if matches!(self.selected(), Some(Row::Group(_))) {
            // A project header: its rules in the pane; `everywhere`'s, the
            // global rules and how their runs outside every project stand.
            self.pane = Pane::Facts;
            self.show_pane = true;
        } else {
            self.say("nothing here to open");
        }
        Ok(())
    }

    /// The run under the cursor, itself: one still going is joined — its
    /// real terminal and its keyboard, handed over by `main_loop` once
    /// ratatui has stepped aside — and an ended one is read in the pane.
    async fn open_run(&mut self, area: Rect) {
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
    async fn resume(&mut self, mut p: RunResumeParams, area: Rect) {
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

    /// Where the line is being typed: the folder of the row under the cursor,
    /// what agents there are, and $PATH.
    fn here<'a>(&'a self, folder: &'a Path) -> complete::Here<'a> {
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
    fn line_folder(&self, text: &str) -> PathBuf {
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

    /// `enter` on the line: what it says. Nothing typed, the row instead.
    async fn submit(&mut self, area: Rect) -> Result<()> {
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
        let what = {
            // Runnable where it will run: a `./build.sh` in the folder the
            // warm shell has `cd`'d into is a shell line.
            let at = self.line_folder(self.line.text());
            let here = self.here(&at);
            line::what(self.line.text(), &self.agents, &|h| complete::runnable(h, &here))
        };
        match what {
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
                    self.ask(words);
                }
            }
        }
        Ok(())
    }

    /// `tab`: as much of the word as every candidate agrees on, then round
    /// the candidates one at a time.
    /// The candidates on offer under the line, if any: the `/` commands it
    /// names, or the words `tab` has laid out. Slash and words never both show.
    fn cands(&self) -> Option<Cands> {
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
    fn slash_list(&self) -> Option<Vec<&'static palette::Command>> {
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
    fn walk_cands(&mut self, by: isize) {
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
    fn accept_cand(&mut self) {
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

    fn tab(&mut self) {
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
        let (projects, rules, branches, kinds) = self.search_values();
        let cands = {
            let here = complete::Here {
                names: complete::Names {
                    projects: &projects,
                    rules: &rules,
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
    async fn command(&mut self, rest: &str, area: Rect) -> Result<()> {
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
                // The commands in the footer, the keys under the line: what
                // `?` shows.
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
                    if let Some(j) = self.selected_job() {
                        let ids: Vec<String> = j.runs.iter().map(|i| i.run.id.clone()).collect();
                        self.archive_job(&j.name, ids).await;
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
                "full" => self.toggle_full(),
                other => self.say(format!("pane {other}? on its own, or /pane full")),
            },
            palette::Cmd::Rename => self.rename_here(),
            palette::Cmd::Pin => self.toggle_pin().await,
            palette::Cmd::Hide => self.hide().await,
            palette::Cmd::Sort => self.sort_project().await,
            palette::Cmd::Graph => self.toggle_graph().await,
            palette::Cmd::Events => self.toggle_events().await,
            palette::Cmd::Shell => self.new_shell(area).await,
            palette::Cmd::Reload => match arg.trim() {
                "rules" => self.reload_rules().await,
                "shell" => self.reload_shell().await,
                "" => {
                    // The shell first: what the rules say is the flash that stays.
                    self.reload_shell().await;
                    self.reload_rules().await;
                }
                other => self.say(format!("reload {other}? rules or shell, or both on its own")),
            },
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
            palette::Cmd::Edit => self.edit_file(&arg),
            palette::Cmd::Summary => self.summary(&arg).await,
            palette::Cmd::Since => self.since(&arg).await,
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
                if arg.trim().is_empty() {
                    self.open_search();
                } else {
                    self.filter = arg.trim().to_string();
                    self.land();
                }
            }
            palette::Cmd::Lens => {
                let want = arg.trim().to_ascii_lowercase();
                match rows::Lens::ALL.iter().find(|l| l.label() == want) {
                    Some(l) => self.set_lens(*l),
                    None => {
                        let all: Vec<&str> = rows::Lens::ALL.iter().map(|l| l.label()).collect();
                        self.say(format!("/lens {} · tab steps through them", all.join("|")));
                    }
                }
            }
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
        let slash = self.slash_list();
        let cands_h = match (&slash, &self.completing) {
            (Some(cs), _) => cs.len().min(CANDS) as u16,
            (None, Some(c)) => c.cands.len().min(CANDS) as u16,
            (None, None) => 0,
        };
        let share = if self.pane == Pane::Term {
            LIST_SHARE_TERM
        } else {
            LIST_SHARE
        };
        // `?`: the keys for here, under the line, as many rows as they wrap to.
        let keys = self.shortcut_lines(f.area().width);
        let a = areas(
            f.area(),
            self.wide,
            self.show_pane,
            share,
            form_h,
            cands_h,
            self.line_h() + keys.len() as u16,
        );
        let rows = self.rows();
        // News is counted the way it is shown: a job with unseen runs is one.
        let news = rows
            .iter()
            .filter(|r| match r {
                Row::Run(i) => i.unseen,
                Row::Job(j) => j.unseen(),
                _ => false,
            })
            .count();
        let title = match self.view {
            View::Work => rows::headline(&rows, news, &self.filter),
            View::Graph => match &self.graph {
                Some(g) => graph::headline(
                    g,
                    graph::keep(g, &self.filter, &self.names, &self.digest.checkouts).len(),
                    &self.filter,
                ),
                None => "wiring".into(),
            },
            View::Events => events::headline(self.event_items().len(), &self.feed, &self.filter),
            View::CatchUp => match &self.catch {
                Some(c) => catchup::headline(c.since, Timestamp::now()),
                None => "while you were away".into(),
            },
        };
        let mut notes = Vec::new();
        if self.view == View::CatchUp {
            notes.push(Span::styled("esc to the list", theme::dim()));
        }
        // What a restart took waits on you like anything else that does.
        if self.view == View::Work
            && let Some(note) = rows::lost_note(&self.digest)
        {
            notes.push(Span::styled(note, theme::needs()));
        }
        // The global rules have no row — nothing would sit under one — so the
        // header says them, amber when one waits for a yes. The graph shows
        // them wired, and it is where that yes is given.
        // A global file that did not load is said beside them, in red.
        if self.view == View::Work {
            let chips = rows::chips(&self.rules, None, Timestamp::now());
            for (words, tone) in rows::global_parts(&chips, !self.broken_in(None).is_empty()) {
                notes.push(match tone {
                    rows::Tone::Plain => Span::raw(words),
                    rows::Tone::Needs => Span::styled(words, theme::needs()),
                    rows::Tone::Bad => Span::styled(words, theme::bad()),
                });
            }
        }
        let said = Said::new(title, notes);
        // Where there is room the mark stands at the left, under a row of
        // air, and the words go beside its middle rows. Its first column is
        // air too, so it starts at the edge and its ink stands one in.
        let text = if a.head.height > 1 {
            let mark = Rect {
                x: a.head.x,
                y: a.head.y + HEAD_AIR,
                width: mark::W,
                height: a.head.height - HEAD_AIR,
            };
            f.render_widget(Paragraph::new(mark::lines(&mark::SMALL)), mark);
            let x = mark.x + mark::W + MARK_GAP;
            Rect::new(x, mark.y + 1, a.head.right().saturating_sub(x), mark.height - 2)
        } else {
            Rect {
                x: a.head.x + TEXT_INDENT,
                width: a.head.width.saturating_sub(TEXT_INDENT),
                ..a.head
            }
        };
        // The header's first line carries three things: its words at the
        // left, the lens at the right of them, and who you are at the edge.
        // The lens rides here rather than a row of its own.
        let lens = (self.view == View::Work).then(|| self.lens_line());
        let lens_w = lens.as_ref().map_or(0, |l| l.width() as u16 + 2);
        let [top, below] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(text);
        let [l, lens_area, r] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(lens_w),
            Constraint::Length(self.me.len() as u16 + 1),
        ])
        .areas(top);
        if a.head.height > 1 {
            let home = home_note(self.home.path(), std::env::var_os("HOME").as_deref().map(Path::new));
            let [first, tally, notes] = said.lines(home.as_deref());
            f.render_widget(Paragraph::new(first), l);
            f.render_widget(Paragraph::new(vec![tally, notes]), below);
        } else {
            f.render_widget(Paragraph::new(said.line()), l);
        }
        if let Some(lens) = lens {
            f.render_widget(Paragraph::new(lens).right_aligned(), lens_area);
        }
        f.render_widget(Paragraph::new(self.me.clone()).dim().right_aligned(), r);
        // The rules between the sections — a little air, a little structure.
        for rule in &a.rules {
            f.render_widget(Paragraph::new("─".repeat(rule.width as usize)).dim(), *rule);
        }
        // Clicks go by where the list was last drawn: with no list on screen,
        // nothing is there to hit.
        if a.list.is_none() || self.view != View::Work {
            *self.hit.borrow_mut() = ListHit::default();
        }
        if let Some(list) = a.list {
            match self.view {
                View::Work => self.draw_list(f, list, &rows),
                View::Graph => self.draw_graph(f, list),
                View::Events => self.draw_events(f, list),
                View::CatchUp => self.draw_catch_up(f, list),
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
            // The arrowed-to row wears a caret and the accent; the rest are quiet.
            // Only `h` of them fit at once, so the window slides to keep the
            // arrowed-to one in view rather than highlighting it off-screen.
            let h = area.height as usize;
            let pick = self.pick;
            let lead = |i: usize| {
                if pick == Some(i) {
                    ("❯ ", theme::accent())
                } else {
                    ("  ", theme::dim())
                }
            };
            let lines: Vec<Line> = match (&slash, &self.completing) {
                (Some(cs), _) => cs
                    .iter()
                    .enumerate()
                    .skip(cand_window(cs.len(), h, pick))
                    .take(h)
                    .map(|(i, c)| {
                        let (mark, style) = lead(i);
                        let name = if c.args.is_empty() {
                            format!("/{}", c.name)
                        } else {
                            format!("/{} {}", c.name, c.args)
                        };
                        let name = if pick == Some(i) {
                            Span::styled(format!("{name:<22}"), style)
                        } else {
                            Span::raw(format!("{name:<22}"))
                        };
                        // The key shortcut, when the command has one (^x, ^c).
                        let key = Span::raw(format!("{:<4}", palette::key(c.cmd))).dim();
                        Line::from(vec![
                            Span::styled(mark, style),
                            name,
                            key,
                            Span::raw(c.what.to_string()).dim(),
                        ])
                    })
                    .collect(),
                (None, Some(c)) => c
                    .cands
                    .iter()
                    .enumerate()
                    .skip(cand_window(c.cands.len(), h, pick))
                    .take(h)
                    .map(|(i, x)| {
                        let (mark, style) = lead(i);
                        Line::from(vec![
                            Span::styled(mark, style),
                            Span::styled(x.text.clone(), if pick == Some(i) { style } else { theme::dim() }),
                        ])
                    })
                    .collect(),
                (None, None) => vec![],
            };
            f.render_widget(Paragraph::new(lines), area);
        }
        let [line_area, keys_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(keys.len() as u16)]).areas(a.line);
        match self.mode {
            Mode::Normal => f.render_widget(Paragraph::new(self.line_lines(line_area.width)), line_area),
            // The line waits under a name being changed, quiet and with no
            // caret: the keys are the name's.
            Mode::Rename(_) => f.render_widget(Paragraph::new(format!("❯ {}", self.line.text())).dim(), line_area),
            _ => {}
        }
        if !keys.is_empty() {
            let keys_area = Rect {
                x: keys_area.x + TEXT_INDENT,
                width: keys_area.width.saturating_sub(TEXT_INDENT),
                ..keys_area
            };
            f.render_widget(Paragraph::new(keys), keys_area);
        }
        let footer = Rect {
            x: a.footer.x + TEXT_INDENT,
            width: a.footer.width.saturating_sub(TEXT_INDENT),
            ..a.footer
        };
        f.render_widget(Paragraph::new(self.footer(footer.width as usize)).dim(), footer);
    }

    /// The lens, at the right of the header: the slices of the list, the one
    /// in force accented, the rest quiet. `tab` on an empty line steps it.
    fn lens_line(&self) -> Line<'static> {
        let mut spans = Vec::new();
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
        Line::from(spans)
    }

    /// The live line: what is typed, where the cursor is, and — at the right
    /// edge, before you commit — what `enter` is about to do with it. It grows
    /// down a row per shift-enter; past `LINE_MAX` it keeps the cursor's row in
    /// view. The `enter` hint rides the last row shown.
    fn line_lines(&self, width: u16) -> Vec<Line<'static>> {
        if let Some(s) = &self.search {
            let hint = if s.line.is_empty() {
                "words · in: is: kind: by:"
            } else {
                "narrowing"
            };
            let mut spans = vec![Span::styled("⌕ ", theme::accent())];
            spans.extend(theme::with_caret(s.line.text(), s.line.cursor(), Style::default()));
            let used: usize = spans.iter().map(Span::width).sum();
            let room = (width as usize).saturating_sub(used + hint.chars().count() + 1);
            spans.push(Span::raw(" ".repeat(room)));
            spans.push(Span::styled(hint, theme::dim()));
            return vec![Line::from(spans)];
        }
        if self.line.is_empty() {
            let mut spans = vec![Span::styled("❯ ", theme::accent())];
            spans.extend(theme::with_caret(
                "what do you want done here? / for commands · shift-enter for a new line",
                0,
                theme::dim(),
            ));
            return vec![Line::from(spans)];
        }
        let folder = self.folder_here();
        let what = {
            // Runnable where it will run: a `./build.sh` in the folder the
            // warm shell has `cd`'d into is a shell line.
            let at = self.line_folder(self.line.text());
            let here = self.here(&at);
            line::what(self.line.text(), &self.agents, &|h| complete::runnable(h, &here))
        };
        let answer = self.question_here().and_then(|q| pane::answer_of(q, self.line.text()));
        // A glyph and a colour say what `enter` will do: teal when it will run
        // something here, dim when the words are only asked or a command.
        let (glyph, tail, style) = match (&answer, &what) {
            (Some((keys, note)), _) => (
                "✱",
                format!(
                    " answers {}{}",
                    keys.join(", "),
                    if note.is_some() { " with that note" } else { "" }
                ),
                theme::needs(),
            ),
            (None, what) => match what {
                line::What::Empty => ("↵", String::new(), theme::dim()),
                line::What::Slash(_) => ("↵", " a command".into(), theme::dim()),
                line::What::Shell(_) => (
                    "❯",
                    format!(" runs in {}", self.names.workspace(&folder)),
                    theme::accent(),
                ),
                line::What::Agent { agent, .. } => ("✦", format!(" {agent} here"), theme::accent()),
                line::What::Ask(_) => ("…", " asks the assistant".into(), theme::dim()),
            },
        };
        let hint = format!("{glyph}{tail}");
        let text = self.line.text();
        let cursor = self.line.cursor();
        // Rows and their byte offsets; the caret rides the row the cursor is in.
        let rows: Vec<(usize, &str)> = {
            let mut v = Vec::new();
            let mut off = 0;
            for part in text.split('\n') {
                v.push((off, part));
                off += part.len() + 1;
            }
            v
        };
        let cur_row = rows.iter().rposition(|(o, _)| *o <= cursor).unwrap_or(0);
        // A window of at most LINE_MAX rows, keeping the cursor's row in view.
        let max = LINE_MAX as usize;
        let start = if rows.len() <= max {
            0
        } else {
            cur_row.saturating_sub(max - 1).min(rows.len() - max)
        };
        let end = (start + max).min(rows.len());
        let mut out = Vec::new();
        for (idx, (o, part)) in rows.iter().enumerate().take(end).skip(start) {
            let prefix = if idx == 0 { "❯ " } else { "  " };
            let mut spans = vec![Span::raw(prefix)];
            if idx == cur_row {
                spans.extend(theme::with_caret(part, cursor - o, Style::default()));
            } else {
                spans.push(Span::raw(part.to_string()));
            }
            if idx == end - 1 {
                let used: usize = spans.iter().map(Span::width).sum();
                let room = (width as usize).saturating_sub(used + hint.chars().count() + 1);
                spans.push(Span::raw(" ".repeat(room)));
                spans.push(Span::styled(hint.clone(), style));
            }
            out.push(Line::from(spans));
        }
        out
    }

    fn draw_list(&self, f: &mut Frame, area: Rect, rows: &[Row]) {
        self.list_h.set(area.height);
        // Cleared here and filled in once the window is known, so a click can
        // never land on a map left behind by a draw that returned early.
        *self.hit.borrow_mut() = ListHit::default();
        if rows.is_empty() {
            let msg = if !self.filter.is_empty() {
                " nothing matches".into()
            } else if self.lens == rows::Lens::Hidden {
                " nothing hidden · /hide takes a row off the list".into()
            } else if self.lens != rows::Lens::All {
                format!(" no {} · tab for the rest", self.lens.label())
            } else {
                " nothing running · rl run -- <command>".into()
            };
            let nothing = self.filter.is_empty() && self.lens == rows::Lens::All;
            f.render_widget(empty_list(&msg, area, nothing), area);
            return;
        }
        let now = Timestamp::now();
        let w = area.width as usize;
        // Columns: icon, title, state, the summary to the edge, ref and age
        // at the right — the title column is the screen's share, bounded.
        let tw = (w * 35 / 100).clamp(14, 44);
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        // One entry per line, in step with `lines`: the row that line belongs
        // to, for the mouse to find.
        let mut keys: Vec<Option<String>> = Vec::with_capacity(rows.len());
        let mut sel_line = 0usize;
        // What the search's words hit is lit up where it shows.
        let words = query::Query::parse(&self.filter).words;
        // A name being changed is typed where it stands, in a cell `w` wide.
        let renamed = |row: &Row, w: usize| match &self.mode {
            Mode::Rename(r) if row.key().as_deref() == Some(r.key.as_str()) => {
                let (spans, from) = name_cell(&r.field, r.from.get(), w);
                r.from.set(from);
                Some(spans)
            }
            _ => None,
        };
        for (n, row) in rows.iter().enumerate() {
            let selected = row.key().is_some() && row.key() == self.sel;
            let mut line = match row {
                Row::Group(h) => {
                    if n > 0 {
                        lines.push(Line::raw(""));
                        keys.push(None);
                    }
                    // At the edge, so the rows under it — indented by their
                    // icon's leading space — read as its own.
                    match renamed(row, place_w(tw)) {
                        Some(name) => header_with(name, h, w, tw, self.header_broken(h)),
                        None => header_line(h, w, tw, self.header_broken(h)),
                    }
                }
                Row::Question(q) => {
                    // One line of it: the summary's first, or else the options.
                    let summary = q
                        .summary
                        .as_deref()
                        .and_then(|s| s.lines().find(|l| !l.trim().is_empty()))
                        .map(str::to_string)
                        .unwrap_or_else(|| q.options.iter().map(|o| o.text()).collect::<Vec<_>>().join(" / "));
                    // Above every project, so it says which one asked.
                    let title = format!("{} › {}", self.names.workspace(&q.workspace), q.title);
                    list_line(
                        w,
                        tw,
                        icon_span(rows::QUESTION, false),
                        vec![Span::styled(fit(&title, tw), theme::name())],
                        vec![Span::styled("to decide", theme::needs())],
                        &summary,
                        vec![],
                        None,
                        fmt_age(q.created_at, now),
                    )
                }
                Row::Run(i) => {
                    let r = &i.run;
                    let under = false;
                    let style = state_style(r.state);
                    // Hidden, and on the list only because it came to need
                    // you: it stands above every folder, and says it is hidden.
                    let through = self.lens != rows::Lens::Hidden
                        && rows::hidden_since(r, &self.names, &self.rules, &self.digest.hidden).is_some();
                    // A pinned run stands above every folder, so it says
                    // which one it is from.
                    let title = if under {
                        format!("  {}", r.title)
                    } else if r.pinned_at.is_some() || through {
                        format!("{} › {}", self.names.workspace(&r.workspace), r.title)
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
                    let title = renamed(row, tw).unwrap_or_else(|| vec![title]);
                    // Where a run says what it last did, a shell says where it
                    // is, when its prompt tells (OSC 7): from its project's
                    // folder down, nothing at the folder itself. One that has
                    // not told says nothing — unless its prompt marks what it
                    // runs, and the detail is the last command, worth reading.
                    let shell = rows::live_shell(r);
                    let place = i
                        .cwd
                        .as_deref()
                        .filter(|_| shell)
                        .map(|c| rows::shell_place(c, &r.workspace));
                    let quiet = shell && r.detail.is_none();
                    let summary = match &place {
                        Some(p) => p.as_str(),
                        None if quiet => "",
                        None => rows::said(i),
                    };
                    // Its ref, and its branch where the folder has moved on.
                    let aside = [r.ref_.clone(), rows::branch_aside(r, &self.digest.checkouts)]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("  ");
                    list_line(
                        w,
                        tw,
                        icon_span(rows::icon(r), r.state.is_terminal()),
                        title,
                        vec![Span::styled(rows::run_words(r), style)],
                        summary,
                        [
                            through.then(|| Span::raw("hidden  ").dim()).into_iter().collect(),
                            joined_lead(i.joined_by.as_deref()),
                        ]
                        .concat(),
                        (!aside.is_empty()).then_some(aside.as_str()),
                        fmt_age(i.at, now),
                    )
                }
                Row::Job(j) => {
                    let last = j.last();
                    let title = Span::raw(fit(&j.name, tw));
                    let title = vec![if j.unseen() { title.bold() } else { title }];
                    // Its state is how the last few went, oldest first; the
                    // tally and the cadence are the pane's.
                    list_line(
                        w,
                        tw,
                        icon_span(rows::JOB, false),
                        title,
                        job_marks(j),
                        &j.summary(),
                        vec![],
                        None,
                        fmt_age(last.at, now),
                    )
                }
            };
            if !words.is_empty() {
                line = light(line, &words);
            }
            if selected {
                // The highlight runs to the edge, however little the row says.
                let short = w.saturating_sub(line.width());
                if short > 0 {
                    line.spans.push(Span::raw(" ".repeat(short)));
                }
                line = line.style(theme::selected());
                sel_line = lines.len();
            }
            lines.push(line);
            keys.push(row.key());
        }
        self.draw_window(f, area, lines, keys, sel_line);
    }

    /// Lines in the list's place, a window of them following the selection;
    /// when there is more either side, the edge line says how much. `keys`
    /// is in step with `lines`: the row each belongs to, for the mouse.
    fn draw_window(
        &self,
        f: &mut Frame,
        area: Rect,
        lines: Vec<Line<'static>>,
        mut keys: Vec<Option<String>>,
        sel_line: usize,
    ) {
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
            // An edge line gave its place to a count, so it carries no row.
            keys[top] = None;
        }
        if more_below {
            let left = n - (top + h);
            let i = shown.len() - 1;
            shown[i] = Line::from(Span::raw(format!(" ▼ {left} more")).dim());
            keys[top + i] = None;
        }
        f.render_widget(Paragraph::new(shown), area);
        *self.hit.borrow_mut() = ListHit {
            area,
            scroll: top,
            keys,
        };
    }

    /// The wiring, a window of it at a time, following the selection.
    fn draw_graph(&self, f: &mut Frame, area: Rect) {
        // Until the mouse knows the graph, it must not act on the list's map.
        *self.hit.borrow_mut() = ListHit::default();
        let Some((g, plan)) = self.graph_plan() else {
            f.render_widget(Paragraph::new(" reading the wiring…").dim(), area);
            return;
        };
        if plan.boxes.is_empty() {
            let msg = if self.filter.is_empty() {
                " nothing wired yet · rl every or rl on, or rl up in a folder with .relay/"
            } else {
                " nothing matches"
            };
            f.render_widget(Paragraph::new(msg).dim(), area);
            return;
        }
        let (w, h) = (area.width as usize, area.height as usize);
        let at = graph::follow(&plan, self.sel.as_deref(), self.graph_at.get(), w, h);
        self.graph_at.set(at);
        let canvas = graph::draw(g, &plan, &self.names, self.sel.as_deref(), Timestamp::now());
        f.render_widget(Paragraph::new(canvas.lines(at.0, at.1, w, h)), area);
    }

    /// The events, newest first, a window of them following the selection.
    fn draw_events(&self, f: &mut Frame, area: Rect) {
        self.list_h.set(area.height);
        *self.hit.borrow_mut() = ListHit::default();
        let items = self.event_items();
        if items.is_empty() {
            let msg = if !self.feed.read {
                " reading the ledger…"
            } else if !self.filter.is_empty() {
                " nothing matches"
            } else {
                " nothing has happened yet"
            };
            f.render_widget(Paragraph::new(msg).dim(), area);
            return;
        }
        let w = area.width as usize;
        let (lines, keys) = events::lines(&items, &self.feed, w, Timestamp::now());
        let words = query::Query::parse(&self.filter).words;
        let mut sel_line = 0;
        let mut shown = Vec::with_capacity(lines.len() + 1);
        for (n, (mut line, key)) in lines.into_iter().zip(&keys).enumerate() {
            if !words.is_empty() {
                line = light(line, &words);
            }
            if self.sel.as_ref() == Some(key) {
                let short = w.saturating_sub(line.width());
                if short > 0 {
                    line.spans.push(Span::raw(" ".repeat(short)));
                }
                line = line.style(theme::selected());
                sel_line = n;
            }
            shown.push(line);
        }
        let mut keys: Vec<Option<String>> = keys.into_iter().map(Some).collect();
        if self.feed.more {
            shown.push(Line::from(Span::raw(" ··· older ones come as the cursor gets to the end")).dim());
            keys.push(None);
        }
        self.draw_window(f, area, shown, keys, sel_line);
    }

    /// The catch-up: its top, a rule, then its projects and rows, a window
    /// of them following the selection.
    fn draw_catch_up(&self, f: &mut Frame, area: Rect) {
        self.list_h.set(area.height);
        *self.hit.borrow_mut() = ListHit::default();
        let Some(cu) = self.catch_up() else {
            f.render_widget(Paragraph::new(" reading what happened…").dim(), area);
            return;
        };
        let w = area.width as usize;
        let mut top = match &self.top {
            Some(s) => catchup::written(s, w),
            None => catchup::top(&cu, w),
        };
        if self.topping.is_some() && self.top.is_none() {
            let words = "writing the top…";
            let pad = w.saturating_sub(words.chars().count() + 1);
            top.push(Line::styled(format!("{}{words}", " ".repeat(pad)), theme::dim()));
        }
        if self.sel.as_deref() == Some(TOP) {
            top = top
                .into_iter()
                .map(|mut l| {
                    let short = w.saturating_sub(l.width());
                    l.spans.push(Span::raw(" ".repeat(short)));
                    l.style(theme::selected())
                })
                .collect();
        }
        // The top stands still above the rows, and never takes more than
        // half the room.
        let top_h = (top.len() as u16).min(area.height / 2);
        let [top_area, rule, rest] = Layout::vertical([
            Constraint::Length(top_h),
            Constraint::Length(u16::from(top_h > 0)),
            Constraint::Fill(1),
        ])
        .areas(area);
        f.render_widget(Paragraph::new(top), top_area);
        f.render_widget(Paragraph::new("─".repeat(rule.width as usize)).dim(), rule);
        self.list_h.set(rest.height);
        let (lines, keys) = catchup::lines(&cu, w, Timestamp::now());
        let words = query::Query::parse(&self.filter).words;
        let mut sel_line = 0;
        let mut shown = Vec::with_capacity(lines.len());
        for (n, (mut line, key)) in lines.into_iter().zip(&keys).enumerate() {
            if !words.is_empty() {
                line = light(line, &words);
            }
            if key.is_some() && *key == self.sel {
                let short = w.saturating_sub(line.width());
                if short > 0 {
                    line.spans.push(Span::raw(" ".repeat(short)));
                }
                line = line.style(theme::selected());
                sel_line = n;
            }
            shown.push(line);
        }
        self.draw_window(f, rest, shown, keys, sel_line);
    }

    fn draw_pane(&mut self, f: &mut Frame, area: Rect) {
        // Filled in again only if a question is drawn, so a click never takes
        // a choice that is no longer on screen.
        *self.pane_hit.borrow_mut() = PaneHit::default();
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            // As a tile the term lines up with the facts pane, whose lines
            // carry a leading space; full screen it fills the pane edge to edge.
            let area = if self.wide {
                area
            } else {
                Rect {
                    x: area.x + 1,
                    width: area.width.saturating_sub(1),
                    ..area
                }
            };
            // Whose terminal this is, on the first row: browse away and the
            // tile stays on its run, so it has to say which one.
            let (word, style) = if t.lost {
                ("lost", ratatui::style::Style::default().fg(theme::BAD))
            } else if t.ended.is_some() {
                ("ended", theme::dim())
            } else {
                ("watching", ratatui::style::Style::default().fg(theme::LIVE))
            };
            let title = self
                .run_by_id(&t.id)
                .map(|r| r.title.clone())
                .unwrap_or_else(|| t.info.run.title.clone());
            let head = Rect { height: 1, ..area };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{word} "), style),
                    Span::styled(
                        fit(&title, area.width.saturating_sub(word.len() as u16 + 1) as usize),
                        theme::name(),
                    ),
                ])),
                head,
            );
            let body = Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(1),
                ..area
            };
            screen::draw(t.screen(), body, f.buffer_mut());
            return;
        }
        let now = Timestamp::now();
        // What a reload just did not load, when there was more than the flash
        // could say: whose, which file and line, and why, until the next key.
        if let Some(list) = &self.reload_list {
            let list: Vec<&NotLoaded> = list.iter().collect();
            let mut lines = pane::not_loaded_lines(&list, Some(&self.names));
            lines.push(Line::from(
                Span::raw(" /edit on a project's header opens its file at the line").dim(),
            ));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        if let (Some(g), Some(n)) = (self.graph.as_ref(), self.selected_node()) {
            let rule = self.selected_rule();
            let last = rule
                .and_then(|r| r.last_run())
                .and_then(|id| self.run_by_id(id))
                .map(|x| x.title.as_str());
            // The one being answered, or else the first still waiting.
            let asking = match &self.mode {
                Mode::Answer(id) => Some(id.as_str()),
                _ => n.waiting.first().map(String::as_str),
            };
            let q = asking
                .and_then(|id| self.digest.decisions.iter().find(|d| d.id == id))
                .map(|q| (q, q.run.as_deref().and_then(|r| self.run_by_id(r))));
            let at = matches!(self.mode, Mode::Answer(_)).then_some(self.answer_at);
            let lines = graph::pane_lines(
                g,
                n,
                rule,
                last,
                q,
                &self.picked,
                at,
                area.width as usize,
                &self.names,
                now,
            );
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        if self.view == View::Events {
            let items = self.event_items();
            let lines = match self.sel.as_deref().and_then(|k| events::item_of(&items, k)) {
                Some(it) => events::pane_lines(it, &self.feed, now),
                None => vec![Line::from(Span::raw(" nothing selected").dim())],
            };
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        let lines = match self.selected() {
            Some(Row::Run(i)) => pane::run_lines(
                i,
                &self.events,
                self.summary_of_row(&Row::Run(i)),
                &self.names,
                area.width as usize,
                now,
            ),
            Some(Row::Job(j)) => pane::job_lines(
                &j,
                self.rule_of_job(&j),
                self.summary_of_row(&Row::Job(j.clone())),
                &self.names,
                area.width as usize,
                now,
            ),
            Some(Row::Question(q)) => {
                let asker = q.run.as_deref().and_then(|id| self.run_by_id(id));
                let at = matches!(&self.mode, Mode::Answer(id) if *id == q.id).then_some(self.answer_at);
                let said = self.summary_of_row(&Row::Question(q));
                let p = pane::question_lines(q, asker, said, &self.picked, at, area.width as usize, now);
                // Laid out to the width already, so row for row with the map.
                *self.pane_hit.borrow_mut() = PaneHit {
                    area,
                    question: q.id.clone(),
                    hits: p.hits,
                };
                f.render_widget(Paragraph::new(p.lines), area);
                return;
            }
            // The pane is the project — the folder — so it is titled by the
            // folder's name (what `^r` renames), not the branch the list row
            // may be grouped under.
            Some(Row::Group(h)) if h.is_everywhere() => {
                let mine: Vec<&rl_proto::methods::DigestItem> = self
                    .digest
                    .items
                    .iter()
                    .filter(|i| rows::everywhere_run(&i.run, &self.names, &self.rules))
                    .collect();
                let kept = self.summary_of_row(&Row::Group(h.clone()));
                pane::everywhere_lines(
                    &mine,
                    &self.rules,
                    &self.broken_in(None),
                    kept,
                    area.width as usize,
                    now,
                )
            }
            Some(Row::Group(h)) => pane::project_lines(
                &self.names.workspace(&h.workspace),
                &h.place,
                &h.workspace,
                &self.digest.items,
                &self.rules,
                &self.broken_in(Some(&h.workspace)),
                self.summary_of_row(&Row::Group(h.clone())),
                area.width as usize,
                now,
            ),
            _ => vec![Line::from(Span::raw(" nothing selected").dim())],
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    /// The foot of the panel: a question being asked, a flash, what is
    /// under way, else the keys for the row — with `? shortcuts` at the end
    /// while `?` would show them and there is room.
    fn footer(&self, width: usize) -> String {
        let title_of = |id: &str| self.run_by_id(id).map(|r| r.title.clone()).unwrap_or_default();
        match &self.mode {
            Mode::Stop(id) => return format!("stop {}? y / any key", title_of(id)),
            Mode::Take(id, who) => return format!("{who} has {}'s keyboard · y takes it / any key", title_of(id)),
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
            Mode::SummaryCost { runs, calls } => {
                let (runs, calls) = (*runs, *calls);
                return match self.summary_ask.as_ref().map(|p| &p.target) {
                    Some(SummaryTarget::Run { id }) => {
                        format!("deep read of {} in parts · {calls} calls · y / any key", title_of(id))
                    }
                    _ => {
                        let s = if runs == 1 { "" } else { "s" };
                        format!("deep over {runs} run{s} · {calls} calls · y / any key")
                    }
                };
            }
            Mode::Remove(r) => return r.question.clone(),
            Mode::Mark(id) => return format!("mark {}: d done · b blocked · w working · esc", title_of(id)),
            Mode::Answer(id) => {
                let q = self.digest.decisions.iter().find(|d| &d.id == id);
                let n = q.map_or(0, |q| q.options.len().min(pane::MAX_KEYS));
                return if q.is_some_and(|q| q.multi) {
                    format!("↑↓ choose · space or 1-{n} picks · enter takes it · esc")
                } else {
                    format!("↑↓ choose · enter takes it · 1-{n} answers · esc")
                };
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
            Mode::Rename(_) => {
                if let Some((m, at)) = &self.flash
                    && at.elapsed() < FLASH
                {
                    return m.clone();
                }
                return "enter saves · esc cancels".into();
            }
            Mode::Normal => {}
        }
        if let Some((m, at)) = &self.flash
            && at.elapsed() < FLASH
        {
            return m.clone();
        }
        if self.search.is_some() {
            let tokens = match self.view {
                View::Events => "in: kind: by:",
                _ => "in: is: kind: by:",
            };
            return format!(
                "type to narrow · tab completes {tokens}, or next match · ↑↓ move · enter keeps · esc puts back"
            );
        }
        if let Some(a) = &self.asking {
            return format!("asking about \"{}\"… · its run is in the list · /stop there", a.words);
        }
        if let Some(s) = &self.summarising {
            let deep = if s.deep { ", deep" } else { "" };
            let since = s
                .from
                .map(|f| format!(", since {}", rl_core::time::fmt_when(f, Timestamp::now())))
                .unwrap_or_default();
            return format!(
                "summarising `{}`{deep}{since}… · its run is in the list · /stop there",
                s.title
            );
        }
        if let Some(r) = &self.removing {
            return format!("removing {}… · stopping its runs", r.name);
        }
        let hints = self.hints();
        if self.shortcuts || !self.line.is_empty() || self.shortcuts_place().is_none() {
            return hints;
        }
        shortcuts::with_hint(hints, width)
    }

    /// The keys for what the cursor is on, in the footer.
    fn hints(&self) -> String {
        // The tile's own footer, but only while the cursor is on the run it
        // shows — browse away and the selected row's actions take over, since
        // that is what `enter` would act on.
        if self.pane == Pane::Term
            && let Some(t) = &self.term
            && (self.wide || self.selected_run().is_some_and(|i| i.run.id == t.id))
        {
            let mut s = String::new();
            // A full-screen app keeps no lines above its screen: nothing to
            // read back, so the keys are not offered.
            let back = if t.screen().alternate_screen() {
                ""
            } else {
                " · pgup/pgdn reads back"
            };
            if t.lost {
                s.push_str(&format!("lost · the last that was heard{back} · esc back"));
            } else if t.ended.is_some() {
                s.push_str(&format!("ended{back} · esc back"));
            } else if let Some(who) = &t.info.joiner {
                s.push_str(&format!(
                    "watching · {who} is driving · enter takes the terminal · esc back"
                ));
            } else {
                s.push_str(&format!("watching · enter takes the terminal{back} · esc back"));
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
        if self.view == View::Graph {
            let pane = if self.show_pane {
                "^o hides details"
            } else {
                "^o details"
            };
            let mut s = String::new();
            if self.selected_node().is_some_and(|n| !n.waiting.is_empty()) {
                s.push_str("enter or `1 and a note` on the line answers it · ");
            } else if let Some(r) = self.selected_rule() {
                if r.status() == RuleStatus::NeedsApproval {
                    s.push_str("/approve · ");
                }
                if r.kind() == RuleKind::Schedule {
                    s.push_str("enter runs it now · ");
                }
                s.push_str(if r.paused() {
                    "/pause lets it run · "
                } else {
                    "/pause · "
                });
                s.push_str("/edit · ");
            }
            s.push_str(&format!("←→ follows the wiring · {pane} · ^g back"));
            return s;
        }
        if self.view == View::Events {
            let pane = if self.show_pane {
                "^o hides details"
            } else {
                "^o details"
            };
            let go = match self.event_target() {
                Some(events::Target::Run(_)) => "enter its run on the list · ",
                Some(events::Target::Rule(..)) => "enter its rule in the graph · ",
                None => "",
            };
            return format!("{go}^f kind: in: by: · {pane} · esc back");
        }
        let pane = if self.show_pane {
            "^o hides details"
        } else {
            "^o details"
        };
        if self.on_top() {
            return format!("/summary [deep] [again] writes it · {pane} · esc the list");
        }
        if self.view == View::CatchUp {
            let go = match self.selected() {
                Some(Row::Question(_)) => "`1 and a note` on the line answers it · enter it on the list · ",
                Some(_) => "enter it on the list · ",
                None => "",
            };
            return format!("{go}/summary · {pane} · esc the list");
        }
        match self.selected() {
            Some(Row::Question(_)) => format!("enter or `1 and a note` on the line answers it · ^x not this · {pane}"),
            Some(Row::Job(_)) => {
                format!("enter its firings · /edit its rule · ^x archives · {pane}")
            }
            Some(Row::Run(i)) if i.run.state.is_terminal() => {
                format!("enter output · /summary · ^x archives · ^t pins · {pane}")
            }
            Some(Row::Run(_)) => format!("enter joins it · ^x stops it · ^t pins · {pane}"),
            // A file here that did not load is what `/edit` opens, at the line.
            Some(Row::Group(h)) if h.is_everywhere() => {
                let edit = if self.header_broken(&h) {
                    "what broke"
                } else {
                    "their schedules"
                };
                format!("enter its stats · ^g the global rules' wiring · /edit {edit} · /new a global rule · {pane}")
            }
            Some(Row::Group(h)) => {
                let edit = if self.header_broken(&h) {
                    "what broke"
                } else {
                    "its schedules"
                };
                format!("enter its stats · /shell · ^g its wiring · /edit {edit} · ^r renames · ^x removes · {pane}")
            }
            _ => String::new(),
        }
    }
}

/// `/summary`'s words: `deep` and `again`, either or both in either
/// order, then a stretch (`3d`, a date, an instant), as `rl summary` takes
/// it. `again` makes a new one even when the one kept is up to date.
fn summary_args(arg: &str, now: Timestamp) -> Result<SummaryArgs> {
    let mut a = SummaryArgs::default();
    let mut rest = arg.trim();
    loop {
        let (word, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        match word {
            "deep" if !a.deep => a.deep = true,
            "again" if !a.again => a.again = true,
            _ => break,
        }
        rest = after.trim_start();
    }
    if !rest.is_empty() {
        a.from = Some(rl_core::time::parse_since(rest, now)?);
    }
    Ok(a)
}

/// What `/summary` was told: how deep, whether afresh, from when.
#[derive(Debug, Default, PartialEq, Eq)]
struct SummaryArgs {
    deep: bool,
    again: bool,
    from: Option<Timestamp>,
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

/// The word for a failed join: the one that matters is someone else holding
/// the keyboard, which `/join` can take.
/// Who holds the keyboard, when that is why a join was refused.
fn held_by(e: &anyhow::Error) -> Option<String> {
    let rpc = e.downcast_ref::<rl_proto::rpc::RpcError>()?;
    if rpc.code != rl_proto::rpc::RpcError::BUSY {
        return None;
    }
    Some(
        rpc.data
            .as_ref()
            .and_then(|d| d["joiner"].as_str())
            .unwrap_or("someone")
            .to_string(),
    )
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

/// What was seen is no longer news, on screen at once rather than at the
/// next fetch.
fn mark_seen(digest: &mut Digest, ids: &[String]) {
    for i in digest.items.iter_mut().filter(|i| ids.contains(&i.run.id)) {
        i.unseen = false;
    }
}

/// The size a run from the panel starts at: the whole terminal, which is
/// what a join hands it. Started any smaller, its first join is a resize —
/// and one that lands before a just-started agent listens for it (Node and
/// Bun take half a second or so) is lost, so the agent draws for the rows it
/// started with and the bottom of the screen stays empty.
fn spawn_size(area: Rect) -> (u16, u16) {
    (area.width, area.height)
}

/// With nothing typed, `^j` and `^k` move as the arrows do, the way `j` and
/// `k` do in vi. With text, they are the line's own: `^j` adds a line (it is
/// what a shift-return mapped to a line feed sends) and `^k` cuts to the end.
fn vi_step(k: KeyEvent, line_empty: bool) -> KeyEvent {
    if !line_empty || k.modifiers != KeyModifiers::CONTROL {
        return k;
    }
    match k.code {
        KeyCode::Char('j') => KeyEvent::from(KeyCode::Down),
        KeyCode::Char('k') => KeyEvent::from(KeyCode::Up),
        _ => k,
    }
}

/// Shift-↑ and shift-↓ move the row under the cursor one place up (`true`)
/// or down within its project. A terminal that does not report shift on the
/// arrows sends them plain, and they just move the cursor.
fn reorder_step(k: &KeyEvent) -> Option<bool> {
    match (k.code, k.modifiers) {
        (KeyCode::Up, KeyModifiers::SHIFT) => Some(true),
        (KeyCode::Down, KeyModifiers::SHIFT) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ListHit, Said, areas, cand_window, empty_list, group_count, head_h, hit_row, home_note, mark_seen,
        reorder_step, spawn_size, term_size, vi_step, viewport,
    };
    use crate::tui::rows::Chip;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use rl_proto::rule::{RuleKind, RuleStatus};
    use rl_proto::types::SummaryTarget;

    #[test]
    fn what_is_seen_stops_being_news_on_screen_at_once() {
        use rl_proto::methods::{Digest, DigestItem};
        use rl_proto::types::{Run, RunKind};
        let now: jiff::Timestamp = "2026-09-28T10:00:00Z".parse().unwrap();
        let item = |id: &str| DigestItem {
            run: Run::new(id, RunKind::Command, "t", vec!["true".into()], "/w", "/w", now),
            summary: String::new(),
            at: now,
            unseen: true,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: None,
        };
        let mut d = Digest {
            since: now,
            items: vec![item("a"), item("b")],
            decisions: vec![],
            checkouts: vec![],
            lost: vec![],
            hidden: Vec::new(),
            order: Vec::new(),
        };
        mark_seen(&mut d, &["a".to_string()]);
        assert_eq!(d.items.iter().map(|i| i.unseen).collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn a_run_starts_at_the_size_a_join_gives_it() {
        assert_eq!(spawn_size(Rect::new(0, 0, 120, 40)), (120, 40));
    }

    #[test]
    fn ctrl_j_and_k_move_when_nothing_is_typed() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(vi_step(ctrl('j'), true).code, KeyCode::Down);
        assert_eq!(vi_step(ctrl('k'), true).code, KeyCode::Up);
        // Typed text keeps them: a new line, and a cut to the end.
        assert_eq!(vi_step(ctrl('j'), false), ctrl('j'));
        assert_eq!(vi_step(ctrl('k'), false), ctrl('k'));
        // Plain j and k are letters, and other ctrl keys are left alone.
        let j = KeyEvent::from(KeyCode::Char('j'));
        assert_eq!(vi_step(j, true), j);
        assert_eq!(vi_step(ctrl('x'), true), ctrl('x'));
    }

    #[test]
    fn shift_and_an_arrow_moves_the_row_and_the_arrow_alone_the_cursor() {
        let shift = |c| KeyEvent::new(c, KeyModifiers::SHIFT);
        assert_eq!(reorder_step(&shift(KeyCode::Up)), Some(true));
        assert_eq!(reorder_step(&shift(KeyCode::Down)), Some(false));
        // Plain arrows, other chords on them, and other shifted keys are not it.
        assert_eq!(reorder_step(&KeyEvent::from(KeyCode::Up)), None);
        assert_eq!(reorder_step(&KeyEvent::from(KeyCode::Down)), None);
        assert_eq!(
            reorder_step(&KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT | KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(reorder_step(&shift(KeyCode::Left)), None);
        assert_eq!(reorder_step(&shift(KeyCode::Char('K'))), None);
    }

    #[test]
    fn a_click_finds_the_row_on_the_line_it_fell_on() {
        let hit = ListHit {
            area: Rect::new(2, 3, 20, 4),
            scroll: 1,
            keys: vec![
                Some("j:one".into()), // scrolled out of sight
                Some("r:two".into()), // the top line drawn
                None,                 // the blank a group leaves above itself
                Some("g:three".into()),
            ],
        };
        assert_eq!(hit_row(&hit, 5, 3), Some("r:two".into()));
        assert_eq!(hit_row(&hit, 5, 4), None, "the blank above a group owns no row");
        assert_eq!(hit_row(&hit, 5, 5), Some("g:three".into()));
        // Off the list on every side.
        assert_eq!(hit_row(&hit, 1, 3), None);
        assert_eq!(hit_row(&hit, 22, 3), None);
        assert_eq!(hit_row(&hit, 5, 2), None);
        assert_eq!(hit_row(&hit, 5, 7), None);
        // Inside the area, but past the last line the list drew.
        assert_eq!(hit_row(&hit, 5, 6), None);
    }

    fn text(l: &ratatui::text::Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_header_stands_tall_where_there_is_room_and_is_one_line_elsewhere() {
        let screen = |w, h| Rect::new(0, 0, w, h);
        assert_eq!(head_h(screen(160, 45), false), 6);
        assert_eq!(head_h(screen(100, 30), false), 6);
        assert_eq!(head_h(screen(80, 24), false), 6);
        // The full-screen terminal wants every cell.
        assert_eq!(head_h(screen(160, 45), true), 1);
        // Too short, or too narrow, to spare the rows for the mark.
        assert_eq!(head_h(screen(160, 23), false), 1);
        assert_eq!(head_h(screen(80, 20), false), 1);
        assert_eq!(head_h(screen(79, 45), false), 1);
        // The list starts under the header and its rule, whichever it is.
        let list = |w, h| {
            areas(screen(w, h), false, false, super::LIST_SHARE, 0, 0, 1)
                .list
                .unwrap()
        };
        assert_eq!(list(100, 30).y, 7);
        assert_eq!(list(80, 20).y, 2);
    }

    #[test]
    fn a_click_under_the_tall_header_finds_its_row() {
        let a = areas(Rect::new(0, 0, 100, 30), false, false, super::LIST_SHARE, 0, 0, 1);
        let list = a.list.unwrap();
        let hit = ListHit {
            area: list,
            scroll: 0,
            keys: vec![Some("g:one".into()), Some("r:two".into())],
        };
        assert_eq!(hit_row(&hit, 3, 7), Some("g:one".into()));
        assert_eq!(hit_row(&hit, 3, 8), Some("r:two".into()));
        // The header's rows and the rule under it are nobody's.
        for row in 0..7 {
            assert_eq!(hit_row(&hit, 3, row), None, "row {row}");
        }
    }

    #[test]
    fn a_terminal_in_the_pane_is_sized_under_the_header_it_has() {
        // 45 rows, less the header (6) and its rule, the rules above the line
        // and the footer, the line, the footer, and the row naming the run.
        assert_eq!(term_size(Rect::new(0, 0, 160, 45), false, 1), (79, 33));
        // Full screen the header is one line and there are no rules.
        assert_eq!(term_size(Rect::new(0, 0, 160, 45), true, 1), (160, 41));
        // A short screen keeps the one-line header beside a tile too.
        assert_eq!(term_size(Rect::new(0, 0, 80, 20), false, 1), (80, 13));
    }

    #[test]
    fn the_header_says_the_same_on_one_line_and_on_three() {
        use crate::tui::theme;
        use ratatui::text::Span;
        let notes = || vec![Span::raw("↻ 2 everywhere"), Span::styled("1 lost", theme::needs())];
        let said = Said::new("relay · 2 working · 1 new".into(), notes());
        assert_eq!(
            text(&said.line()),
            "relay · 2 working · 1 new · ↻ 2 everywhere · 1 lost"
        );
        let v = env!("CARGO_PKG_VERSION");
        let [first, tally, rest] = said.lines(None);
        assert_eq!(text(&first), format!("relay {v}"));
        assert_eq!(text(&tally), "2 working · 1 new", "the first line says relay already");
        assert_eq!(text(&rest), "↻ 2 everywhere · 1 lost");
        assert_eq!(rest.spans[2].style, theme::needs(), "a note keeps its tone");
        let [first, ..] = said.lines(Some("/tmp/x"));
        assert_eq!(text(&first), format!("relay {v} · /tmp/x"));
        // The graph's headline leads with its own word, and keeps it.
        let said = Said::new("wiring · 3 rules".into(), vec![]);
        let [_, tally, rest] = said.lines(None);
        assert_eq!(text(&tally), "wiring · 3 rules");
        assert_eq!(
            tally.spans[0].style,
            theme::accent().add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(text(&rest), "");
    }

    #[test]
    fn the_home_is_said_only_when_it_is_not_the_usual_one() {
        use std::path::Path;
        let user = Some(Path::new("/Users/me"));
        assert_eq!(home_note(Path::new("/Users/me/.relay"), user), None);
        assert_eq!(home_note(Path::new("/Users/me/.relay/"), user), None);
        assert_eq!(home_note(Path::new("/tmp/x"), user), Some("/tmp/x".into()));
        assert_eq!(home_note(Path::new("/tmp/x"), None), Some("/tmp/x".into()));
    }

    #[test]
    fn an_empty_list_shows_the_mark_where_it_fits() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;
        let shown = |nothing: bool, w: u16, h: u16| {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            empty_list(" nothing running · rl run -- <command>", area, nothing).render(area, &mut buf);
            (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let big = shown(true, 60, 20);
        assert!(big.contains("██▀▀████▀▀██"), "{big}");
        assert!(big.contains("nothing running"));
        // Too small for it, or a lens or a search that came up empty: words.
        for s in [shown(true, 60, 9), shown(true, 30, 20), shown(false, 60, 20)] {
            assert!(!s.contains('█'), "{s}");
            assert!(s.starts_with(" nothing running"), "{s}");
        }
    }

    #[test]
    fn the_candidate_window_follows_the_pick() {
        // Everything fits: no window at all.
        assert_eq!(cand_window(4, 6, Some(3)), 0);
        // More than fits, but the pick is still on the first screenful.
        assert_eq!(cand_window(20, 6, None), 0);
        assert_eq!(cand_window(20, 6, Some(5)), 0);
        // Past the last visible row, the window slides and the pick sits at
        // the foot of it.
        assert_eq!(cand_window(20, 6, Some(6)), 1);
        assert_eq!(cand_window(20, 6, Some(7)), 2);
        // The end of the list, and no further.
        assert_eq!(cand_window(20, 6, Some(19)), 14);
        // Wrapping back to the top brings the window home.
        assert_eq!(cand_window(20, 6, Some(0)), 0);
    }

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

    /// The column a piece of text starts at on a drawn line, counting cells.
    fn col_of(line: &ratatui::text::Line, text: &str) -> usize {
        let mut s = String::new();
        let mut cols = 0;
        for span in &line.spans {
            s.push_str(&span.content);
        }
        let at = s.find(text).unwrap_or_else(|| panic!("{text:?} not in {s:?}"));
        for c in s[..at].chars() {
            cols += ratatui::text::Span::raw(c.to_string()).width();
        }
        cols
    }

    #[test]
    fn headers_and_rows_keep_their_columns_whatever_the_lengths() {
        use super::{header_line, icon_span, list_line, state_col};
        use crate::tui::rows::{self, Head};
        use ratatui::text::Span;
        use rl_proto::types::Checkout;
        let (w, tw) = (100, 30);
        let co = |b: &str, n| Checkout {
            path: "/w".into(),
            branch: Some(b.into()),
            detached: None,
            worktrees: n,
            badges: vec![],
        };
        let head = |place: &str, own: Option<&str>, checkout, rules| Head {
            place: place.into(),
            workspace: "/w".into(),
            name: "w".into(),
            own: own.map(Into::into),
            rules,
            checkout,
        };
        let a = header_line(
            &head("~/Projects/flint-platform", None, Some(co("main", 24)), vec![]),
            w,
            tw,
            false,
        );
        let b = header_line(
            &head(
                "~/tmp/foodai",
                Some("food"),
                Some(co("a-long-branch", 4)),
                vec![chip(RuleKind::Schedule)],
            ),
            w,
            tw,
            false,
        );
        let c = header_line(
            &head(
                "~/Downloads/some/very/deep/folder/Finance & Real Estate",
                None,
                Some(co("main", 0)),
                vec![],
            ),
            w,
            tw,
            false,
        );
        // The checkout starts in the state column on every header.
        assert_eq!(col_of(&a, "⎇"), state_col(tw));
        assert_eq!(col_of(&b, "⎇"), state_col(tw));
        assert_eq!(col_of(&c, "⎇"), state_col(tw));
        assert_eq!(col_of(&b, "tmp/"), 2, "the parent, then the folder");
        assert!(b.to_string().contains("foodai · food"));
        // A long path lost its start and kept its folder.
        assert!(c.to_string().starts_with("…/"), "{c}");
        assert!(c.to_string().contains("Finance & Real Estate"));
        // The tally ends at the edge.
        assert_eq!(b.width(), w);
        assert!(b.to_string().ends_with("1 job"));

        // A folder whose rules file did not load says so in red, before a
        // rule waiting for a yes in amber; the tally still ends at the edge.
        let waiting = Chip {
            name: "deploy".into(),
            status: RuleStatus::NeedsApproval,
            ..chip(RuleKind::Reaction)
        };
        let rules = vec![waiting, chip(RuleKind::Schedule)];
        let broken = header_line(
            &head("~/tmp/foodai", None, Some(co("main", 0)), rules.clone()),
            w,
            tw,
            true,
        );
        assert!(
            broken.to_string().ends_with("! rules  ! deploy  1 job · 1 reaction"),
            "{broken}"
        );
        assert_eq!(broken.width(), w);
        let red = broken.spans.iter().find(|s| s.content == "! rules").unwrap();
        assert_eq!(red.style, crate::tui::theme::bad());
        let fine = header_line(&head("~/tmp/foodai", None, Some(co("main", 0)), rules), w, tw, false);
        assert!(!fine.to_string().contains("! rules"), "{fine}");
        // Nothing else on it: the chip alone, at the edge.
        let alone = header_line(&head("~/tmp/foodai", None, None, vec![]), w, tw, true);
        assert!(alone.to_string().ends_with("! rules"), "{alone}");
        assert_eq!(alone.width(), w);

        // A run's state word starts where a header's checkout does, and every
        // title in the same column, whatever icon it wears.
        let row = |icon: &str, word: &str| {
            list_line(
                w,
                tw,
                icon_span(icon, false),
                vec![Span::raw(crate::tui::rows::fit("title", tw))],
                vec![Span::raw(word.to_string())],
                "summary",
                vec![],
                None,
                "2m".into(),
            )
        };
        for (icon, word) in [(rows::AGENT, "needs you"), (rows::SHELL, ""), (rows::JOB, "·×··")] {
            let l = row(icon, word);
            assert_eq!(col_of(&l, "title"), 3, "{l}");
            assert_eq!(col_of(&l, "summary"), state_col(tw) + 11, "{l}");
            assert_eq!(l.width(), w, "{l}");
        }
        assert_eq!(col_of(&row(rows::AGENT, "needs you"), "needs"), state_col(tw));
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

    #[test]
    fn a_long_name_scrolls_sideways_to_keep_the_caret_in_its_cell() {
        use super::{line, name_cell};
        use ratatui::style::Modifier;
        let shown = |spans: &[ratatui::text::Span]| spans.iter().map(|s| s.content.to_string()).collect::<String>();
        let caret = |spans: &[ratatui::text::Span]| {
            let at = spans
                .iter()
                .position(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                .unwrap();
            spans[..at].iter().map(|s| s.content.chars().count()).sum::<usize>()
        };
        let mut f = line::Field::selected("the nightly build of everything we have");
        // As it opens: the start, the caret on its first letter, all of it lit.
        let (spans, from) = name_cell(&f, 0, 12);
        assert_eq!((shown(&spans).as_str(), from, caret(&spans)), ("the nightly ", 0, 0));
        assert_eq!(spans[2].style, crate::tui::theme::picked());
        // At the end: the cell shows the tail, the caret in its last column.
        f.key(&KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let (spans, from) = name_cell(&f, from, 12);
        assert_eq!((shown(&spans).as_str(), from, caret(&spans)), ("ing we have ", 28, 11));
        assert!(!spans.iter().any(|s| s.style == crate::tui::theme::picked()), "let go");
        // Back a few: the cell stays put while the caret is in it.
        for _ in 0..4 {
            f.key(&KeyEvent::from(KeyCode::Left));
        }
        let (spans, from) = name_cell(&f, from, 12);
        assert_eq!((from, caret(&spans)), (28, 7));
        // Every cell is the column's width, whatever it holds.
        for w in [1, 5, 12, 60] {
            let (spans, _) = name_cell(&f, 0, w);
            assert_eq!(shown(&spans).chars().count(), w, "{w}");
        }
    }

    /// A panel on a daemon of its own, in a home of its own: one project,
    /// registered as `api` from a folder called `proj`, with one ended run in
    /// it, `make test`. Nothing is started, and no agent is anywhere near.
    async fn panel() -> (
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    ) {
        panel_with_runs(1).await
    }

    /// `panel()` with `n` ended runs in the project: `make test` first, then
    /// `sh -c 'echo 2'` and on.
    async fn panel_with_runs(
        n: usize,
    ) -> (
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    ) {
        panel_of(move |folder, now| {
            (2..=n)
                .map(|i| {
                    let echo = format!("echo {i}");
                    rl_proto::types::Run::new(
                        format!("run{i:05}"),
                        rl_proto::types::RunKind::Command,
                        &echo,
                        vec!["sh".into(), "-c".into(), echo.clone()],
                        folder,
                        folder,
                        now,
                    )
                })
                .collect()
        })
        .await
    }

    /// `panel()`, with the ended runs `more` gives (in the project's folder,
    /// at the instant given) as well. Should anything ask the summariser, it
    /// is `fake-agent`, never a real one.
    async fn panel_of(
        more: impl FnOnce(&std::path::Path, jiff::Timestamp) -> Vec<rl_proto::types::Run>,
    ) -> (
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    ) {
        panel_set(Setup::default(), more).await
    }

    /// What a test panel's daemon is told beyond `panel()`'s.
    #[derive(Default)]
    struct Setup {
        /// More of `config.yaml`, after `assistants: summary: fakeai`.
        config: &'static str,
        /// What `fakeai` answers; None, the fixture's own draft.
        answer: Option<&'static str>,
        /// `rl-session`, so an assistant's run really runs; without it the
        /// daemon cannot start one.
        session: Option<std::path::PathBuf>,
    }

    /// `rl-session` beside this test binary's folder, as `cargo test
    /// --workspace` builds it.
    fn session_bin() -> Option<std::path::PathBuf> {
        let p = std::env::current_exe().ok()?.parent()?.parent()?.join("rl-session");
        p.is_file().then_some(p)
    }

    /// A panel whose summariser, `fake-agent`, really runs and answers
    /// `answer`. None (and the test says it is skipping) when `rl-session`
    /// is not built.
    async fn panel_with_fake_summariser(
        answer: &'static str,
    ) -> Option<(
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    )> {
        let Some(session) = session_bin() else {
            eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
            return None;
        };
        let setup = Setup {
            answer: Some(answer),
            session: Some(session),
            ..Default::default()
        };
        Some(panel_set(setup, |_, _| vec![]).await)
    }

    /// `panel()` with more of the config: `fakeai` is the summariser, but no
    /// assistant's run can start.
    async fn panel_with_config(
        config: &'static str,
    ) -> (
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    ) {
        let setup = Setup {
            config,
            ..Default::default()
        };
        panel_set(setup, |_, _| vec![]).await
    }

    async fn panel_set(
        set: Setup,
        more: impl FnOnce(&std::path::Path, jiff::Timestamp) -> Vec<rl_proto::types::Run>,
    ) -> (
        tempfile::TempDir,
        tokio_util::sync::CancellationToken,
        super::App,
        std::path::PathBuf,
    ) {
        use rl_ledger::events::{self, NewEvent};
        use rl_ledger::{Ledger, runs, session_io, workspaces};
        use rl_proto::types::{Run, RunKind, RunState, Source, event_kind};
        let tmp = tempfile::tempdir().unwrap();
        let home = rl_core::Home::at(tmp.path().join("h"));
        home.ensure_dirs().unwrap();
        let fake =
            std::fs::canonicalize(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent"))
                .unwrap();
        std::fs::create_dir_all(home.detect_dir()).unwrap();
        // The fixture is told what to answer through its environment: `env`
        // sets it for this test's assistant runs alone.
        let mut vars = Vec::new();
        if let Some(a) = set.answer {
            let file = tmp.path().join("answer.txt");
            std::fs::write(&file, a).unwrap();
            vars.push(format!("FAKE_AGENT_ANSWER={}", file.display()));
        }
        let profile = if vars.is_empty() {
            format!(
                "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
                fake.display()
            )
        } else {
            let mut args: Vec<String> = vars.iter().map(|v| format!("\"{v}\"")).collect();
            args.push(format!("\"{}\"", fake.display()));
            args.push("\"--headless\"".into());
            format!(
                "kind: agent\nbins: [\"/usr/bin/env\"]\nheadless: [{}]\n",
                args.join(", ")
            )
        };
        std::fs::write(home.detect_dir().join("fakeai.yaml"), profile).unwrap();
        std::fs::write(
            home.config_file(),
            format!("assistants:\n  summary: fakeai\n{}", set.config),
        )
        .unwrap();
        let folder = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        let now = jiff::Timestamp::now();
        let run = Run::new(
            "run00001",
            RunKind::Command,
            "make test",
            vec!["make".into(), "test".into()],
            &folder,
            &folder,
            now,
        );
        let mut all = vec![run];
        all.extend(more(&folder, now));
        let ledger = Ledger::open(&home.ledger()).unwrap();
        ledger
            .call_blocking({
                let folder = folder.clone();
                move |c| {
                    workspaces::register(c, &folder, "api", now)?;
                    for run in &all {
                        runs::insert(c, run)?;
                        // The list shows what has news since it last looked.
                        events::append(c, &NewEvent::for_run(run, now, event_kind::RUN_STARTED, "started"))?;
                        // Each said something, so a deep read of it asks.
                        let said = format!("{} said this\r\n", run.title);
                        session_io::append(c, &run.id, session_io::Direction::Out, said.as_bytes(), now)?;
                        runs::finish(c, &run.id, RunState::Finished, Source::Pty, Some(0), now)?;
                    }
                    Ok(())
                }
            })
            .unwrap();
        drop(ledger);
        let token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(rl_daemon::serve_with(
            home.clone(),
            token.clone(),
            rl_daemon::ServeOptions {
                session_bin: set.session,
                ..Default::default()
            },
        ));
        let rpc = loop {
            match crate::client::Client::connect(&home).await {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };
        let mut app = super::App::new(&home, rpc, "test".into(), vec![]);
        app.refresh().await;
        (tmp, token, app, folder)
    }

    /// The cursor on the first header.
    fn select_header(app: &mut super::App) {
        let key = app
            .rows()
            .iter()
            .find(|r| matches!(r, super::Row::Group(_)))
            .and_then(|r| r.key());
        app.select(key);
    }

    async fn type_in(app: &mut super::App, text: &str) {
        for c in text.chars() {
            app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), AREA)
                .await
                .unwrap();
        }
    }

    async fn press(app: &mut super::App, code: KeyCode, m: KeyModifiers) {
        app.key(KeyEvent::new(code, m), AREA).await.unwrap();
    }

    const AREA: Rect = Rect::new(0, 0, 120, 40);
    const RUN: &str = "r:run00001";

    fn renaming(app: &super::App) -> Option<(String, bool)> {
        match &app.mode {
            super::Mode::Rename(r) => Some((r.field.text().to_string(), r.field.is_selected())),
            _ => None,
        }
    }

    fn title(app: &super::App) -> String {
        app.run_by_id("run00001").unwrap().title.clone()
    }

    #[tokio::test]
    async fn a_run_is_renamed_where_it_stands_and_only_enter_saves() {
        let (_tmp, token, mut app, folder) = panel().await;
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        app.select(Some(RUN.into()));
        // Something half typed on the line below, to be left as it is.
        type_in(&mut app, "echo hi").await;

        // ^r: the name, all of it selected; typing replaces it, enter saves.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        assert_eq!(renaming(&app), Some(("make test".into(), true)));
        type_in(&mut app, "nightly").await;
        assert_eq!(renaming(&app), Some(("nightly".into(), false)));
        // The line's own keys: to the start, a word in, and on.
        press(&mut app, KeyCode::Char('a'), ctrl).await;
        type_in(&mut app, "the ").await;
        press(&mut app, KeyCode::Char('e'), ctrl).await;
        type_in(&mut app, " build").await;
        assert_eq!(renaming(&app), Some(("the nightly build".into(), false)));
        press(&mut app, KeyCode::Enter, none).await;
        assert!(renaming(&app).is_none());
        assert_eq!(title(&app), "the nightly build");
        assert_eq!(app.sel.as_deref(), Some(RUN), "the cursor stays on it");

        // esc lets it go unsaved.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        press(&mut app, KeyCode::Esc, none).await;
        assert!(renaming(&app).is_none());
        assert_eq!(title(&app), "the nightly build");

        // So does moving off the row — the arrows, ^k with nothing to cut,
        // ^j — and the key still moves.
        let group = format!("g:{}", folder.display());
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        press(&mut app, KeyCode::Up, none).await;
        assert!(renaming(&app).is_none());
        assert_eq!(app.sel.as_deref(), Some(group.as_str()));
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        press(&mut app, KeyCode::Char('k'), ctrl).await;
        assert_eq!(renaming(&app), Some((String::new(), false)), "^k cuts first");
        press(&mut app, KeyCode::Char('k'), ctrl).await;
        assert!(renaming(&app).is_none());
        assert_eq!(app.sel.as_deref(), Some(group.as_str()));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        press(&mut app, KeyCode::Char('j'), ctrl).await;
        assert!(renaming(&app).is_none());
        assert_eq!(app.sel.as_deref(), Some(RUN));
        assert_eq!(title(&app), "the nightly build");

        // A paste replaces the selection too, as one line.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        app.paste("from the\nclipboard".into(), AREA).await.unwrap();
        assert_eq!(renaming(&app), Some(("from the clipboard".into(), false)));
        press(&mut app, KeyCode::Esc, none).await;

        // Nothing for a name: back to the one Relay gave it.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        press(&mut app, KeyCode::Backspace, none).await;
        assert_eq!(renaming(&app), Some((String::new(), false)));
        press(&mut app, KeyCode::Enter, none).await;
        assert!(renaming(&app).is_none());
        assert_eq!(title(&app), "make test");

        // Shift-enter saves as well, never a new line in a name.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "tests").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT).await;
        assert_eq!(title(&app), "tests");

        // None of it reached the line, which is as it was and still its own.
        assert_eq!(app.line.text(), "echo hi");
        press(&mut app, KeyCode::Char('j'), ctrl).await;
        assert_eq!(app.line.text(), "echo hi\n", "^j on the line is a new line, as ever");
        token.cancel();
    }

    #[tokio::test]
    async fn a_project_is_renamed_in_its_header_and_an_empty_name_is_its_folders() {
        let (_tmp, token, mut app, folder) = panel().await;
        let (none, ctrl) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        app.select(Some(format!("g:{}", folder.display())));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        assert_eq!(renaming(&app), Some(("api".into(), true)));
        type_in(&mut app, "shiny").await;
        press(&mut app, KeyCode::Enter, none).await;
        assert!(renaming(&app).is_none());
        assert_eq!(app.names.workspace(&folder), "shiny");

        // A click on another row lets it go, and lands there; passing over
        // the list does not.
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let (x, y) = {
            let hit = app.hit.borrow();
            let at = hit.keys.iter().position(|k| k.as_deref() == Some(RUN)).unwrap();
            (hit.area.x + 5, hit.area.y + (at - hit.scroll) as u16)
        };
        let mouse = |kind| crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: none,
        };
        app.mouse(mouse(crossterm::event::MouseEventKind::Moved), AREA)
            .await
            .unwrap();
        assert_eq!(renaming(&app), Some(("x".into(), false)));
        let left = crossterm::event::MouseButton::Left;
        app.mouse(mouse(crossterm::event::MouseEventKind::Down(left)), AREA)
            .await
            .unwrap();
        assert!(renaming(&app).is_none());
        assert_eq!(app.sel.as_deref(), Some(RUN));
        assert_eq!(app.names.workspace(&folder), "shiny");

        // `tab` steps the lens, and lets the name go as well.
        app.select(Some(format!("g:{}", folder.display())));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        press(&mut app, KeyCode::Tab, none).await;
        assert!(renaming(&app).is_none());
        assert_ne!(app.lens, crate::tui::rows::Lens::All);
        app.set_lens(crate::tui::rows::Lens::All);
        app.select(Some(format!("g:{}", folder.display())));
        assert_eq!(app.names.workspace(&folder), "shiny");

        press(&mut app, KeyCode::Char('r'), ctrl).await;
        press(&mut app, KeyCode::Delete, none).await;
        press(&mut app, KeyCode::Enter, none).await;
        assert_eq!(app.names.workspace(&folder), "proj", "its folder's own name");
        token.cancel();
    }

    #[tokio::test]
    async fn the_name_is_typed_in_its_cell_with_the_caret_there() {
        use ratatui::style::Modifier;
        let (_tmp, token, mut app, _) = panel().await;
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL).await;
        let long = "a name far too long for the column it is typed into, end";
        app.paste(long.into(), AREA).await.unwrap();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        // One caret on the screen: the line below has none while the name
        // has the keys.
        let carets: Vec<(u16, u16)> = (0..40)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(carets.len(), 1, "{carets:?}");
        let (x, y) = carets[0];
        let row: String = (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect();
        let before: String = (0..x).map(|x| buf[(x, y)].symbol().to_string()).collect();
        // The name's end, up against the caret; its start scrolled away.
        assert!(before.ends_with("typed into, end"), "{row}");
        assert!(!row.contains("a name far"), "{row}");
        assert!(buf_has(&buf, "enter saves · esc cancels"));
        token.cancel();
    }

    #[tokio::test]
    async fn summary_on_a_header_is_the_projects() {
        let (_tmp, token, mut app, _folder) = panel().await;
        select_header(&mut app);
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Project {
                workspace: app.selected_folder().unwrap()
            })
        );
        token.cancel();
    }

    #[tokio::test]
    async fn each_row_says_what_its_summary_is_of() {
        use rl_proto::types::{Cause, Run, RunKind};
        let (_tmp, token, mut app, folder) = panel_of(|folder, now| {
            let job = |id: &str, name: &str, at: &std::path::Path| {
                let mut r = Run::new(id, RunKind::Command, name, vec!["true".into()], at, at, now);
                r.cause = Cause::Schedule(name.into());
                r
            };
            let elsewhere = folder.parent().unwrap().join("elsewhere");
            vec![job("run00002", "tidy", folder), job("run00003", "nightly", &elsewhere)]
        })
        .await;
        app.select(Some(RUN.into()));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Run { id: "run00001".into() }));
        let job_key = |app: &super::App, name: &str| {
            app.rows().iter().find_map(|r| match r {
                super::Row::Job(j) if j.name == name => Some(j.key.clone()),
                _ => None,
            })
        };
        // A project's job is in its folder; a global one's is anywhere.
        app.select(job_key(&app, "tidy"));
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Job {
                rule: RuleKind::Schedule,
                name: "tidy".into(),
                workspace: Some(folder.clone())
            })
        );
        app.select(job_key(&app, "nightly"));
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Job {
                rule: RuleKind::Schedule,
                name: "nightly".into(),
                workspace: None
            })
        );
        app.select(Some(format!("g:{}", folder.display())));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Project { workspace: folder }));
        app.select(Some("g:everywhere".into()));
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::All),
            "everywhere is everything"
        );
        token.cancel();
    }

    #[test]
    fn summary_takes_deep_and_again_then_a_stretch() {
        use super::{SummaryArgs, summary_args};
        let now = jiff::Timestamp::now();
        let h3 = now - jiff::SignedDuration::from_hours(3);
        let a = |deep, again, from| SummaryArgs { deep, again, from };
        assert_eq!(summary_args("", now).unwrap(), a(false, false, None));
        assert_eq!(summary_args("deep", now).unwrap(), a(true, false, None));
        assert_eq!(summary_args("again", now).unwrap(), a(false, true, None));
        assert_eq!(summary_args("3h", now).unwrap(), a(false, false, Some(h3)));
        assert_eq!(summary_args(" deep  3h ", now).unwrap(), a(true, false, Some(h3)));
        assert_eq!(summary_args("again deep 3h", now).unwrap(), a(true, true, Some(h3)));
        assert_eq!(summary_args("deep again", now).unwrap(), a(true, true, None));
        assert!(summary_args("soon", now).is_err());
        assert!(summary_args("3h deep", now).is_err(), "the words come first");
        assert!(summary_args("deep deep", now).is_err(), "a word once");
    }

    #[tokio::test]
    async fn deep_over_many_rows_asks_first() {
        let (_tmp, token, mut app, _folder) = panel_with_runs(3).await; // three ended runs in one project
        select_header(&mut app);
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(
            matches!(app.mode, super::Mode::SummaryCost { runs: 3, calls: 4 }),
            "three deep reads and one to join"
        );
        assert!(
            app.footer(120).starts_with("deep over 3 runs · 4 calls · y"),
            "{}",
            app.footer(120)
        );
        // Not away from anywhere yet: the last day.
        let from = app.summary_ask.as_ref().and_then(|p| p.from).unwrap();
        let day = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24);
        assert!(
            day.duration_since(from).abs() < jiff::SignedDuration::from_secs(60),
            "{from}"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(matches!(app.mode, super::Mode::Normal));
        assert!(app.summarising.is_none(), "esc asks no one");
        assert!(app.summary_ask.is_none());

        // Away since an hour ago: since then. `y` asks.
        let away = jiff::Timestamp::from_millisecond(jiff::Timestamp::now().as_millisecond() - 3_600_000).unwrap();
        app.away_since = Some(away);
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(app.mode, super::Mode::SummaryCost { calls: 4, .. }));
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).await;
        assert!(matches!(app.mode, super::Mode::Normal));
        let s = app.summarising.as_ref().expect("y asks");
        assert!(matches!(s.target, SummaryTarget::Project { .. }));
        assert_eq!((s.deep, s.from), (true, Some(away)));
        token.cancel();
    }

    #[tokio::test]
    async fn a_deep_read_says_what_it_costs() {
        let (tmp, token, mut app, _folder) = panel().await;
        // One run, long enough to be read in three parts and joined.
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(|c| {
                let long = "a line of output to fill the journal\r\n".repeat(3_200);
                rl_ledger::session_io::append(
                    c,
                    "run00001",
                    rl_ledger::session_io::Direction::Out,
                    long.as_bytes(),
                    jiff::Timestamp::now(),
                )?;
                Ok(())
            })
            .unwrap();
        select_header(&mut app);
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(
            app.footer(120).starts_with("deep over 1 run · 5 calls · y"),
            "three parts, their join, and one over the runs: {}",
            app.footer(120)
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        app.select(Some(RUN.into()));
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(
            app.footer(120)
                .starts_with("deep read of make test in parts · 4 calls · y"),
            "{}",
            app.footer(120)
        );
        token.cancel();
    }

    #[tokio::test]
    async fn a_jobs_summary_starts_at_the_firing_you_had_not_seen() {
        use rl_proto::types::{Cause, Run, RunKind};
        let (_tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let firing = |id: &str, hours| {
                let at = now - jiff::SignedDuration::from_hours(hours);
                let mut r = Run::new(id, RunKind::Command, "tidy", vec!["true".into()], folder, folder, at);
                r.cause = Cause::Schedule("tidy".into());
                r
            };
            vec![firing("run00002", 2), firing("run00003", 1)]
        })
        .await;
        let oldest = app.run_by_id("run00002").unwrap().started_at;
        let key = app.rows().iter().find_map(|r| match r {
            super::Row::Job(j) if j.name == "tidy" => Some(j.key.clone()),
            _ => None,
        });
        // The cursor lands on it: that is looking, and its firings are seen.
        app.select(key);
        app.moved().await;
        assert!(app.selected_job().is_some_and(|j| !j.unseen()));
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let s = app.summarising.as_ref().expect("asked");
        assert!(matches!(s.target, SummaryTarget::Job { .. }), "{:?}", s.target);
        assert_eq!(
            s.from,
            Some(oldest),
            "since the firing you had not seen, not the last day"
        );
        assert!(app.unseen_from.is_empty(), "used once asked");
        token.cancel();
    }

    #[tokio::test]
    async fn a_questions_pane_shows_what_was_said_of_its_asker() {
        use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
        let (tmp, token, mut app, folder) = panel().await;
        let now = jiff::Timestamp::now();
        let asked = Decision {
            id: "dec00001".into(),
            workspace: folder.clone(),
            run: Some("run00001".into()),
            flow: "f".into(),
            group: None,
            title: "ship it?".into(),
            summary: None,
            options: vec![DecisionOption {
                key: "ok".into(),
                ..Default::default()
            }],
            multi: false,
            status: DecisionStatus::Open,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        };
        let recap = rl_proto::types::Summary {
            run: "run00001".into(),
            agent: "fakeai".into(),
            by: "a1".into(),
            at: now,
            text: "Two tests failed.".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 1,
            runs: 1,
        };
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(move |c| {
                rl_ledger::decisions::insert(c, &asked)?;
                rl_ledger::summaries::put(c, &recap)
            })
            .unwrap();
        app.refresh().await;
        app.select(Some("q:dec00001".into()));
        app.moved().await;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        assert!(buf_has(&buf, "ship it?"));
        assert!(buf_has(&buf, "Two tests failed."), "under the asker");
        token.cancel();
    }

    #[tokio::test]
    async fn again_asks_for_a_new_one() {
        let (_tmp, token, mut app, _folder) = panel_with_runs(3).await;
        select_header(&mut app);
        type_in(&mut app, "/summary deep again").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(app.mode, super::Mode::SummaryCost { calls: 4, .. }));
        let p = app.summary_ask.as_ref().unwrap();
        assert!(p.deep && p.again && !p.dry, "{p:?}");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        // Without it, the one kept will do.
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(!app.summary_ask.as_ref().unwrap().again);
        token.cancel();
    }

    #[tokio::test]
    async fn a_question_is_told_by_the_run_that_asked_it() {
        use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
        let (tmp, token, mut app, folder) = panel().await;
        let now = jiff::Timestamp::now();
        let ask = |id: &str, run: Option<&str>| Decision {
            id: id.into(),
            workspace: folder.clone(),
            run: run.map(Into::into),
            flow: id.into(),
            group: None,
            title: format!("question {id}"),
            summary: None,
            options: vec![DecisionOption {
                key: "ok".into(),
                ..Default::default()
            }],
            multi: false,
            status: DecisionStatus::Open,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        };
        let (asked, alone) = (ask("dec00001", Some("run00001")), ask("dec00002", None));
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(move |c| {
                rl_ledger::decisions::insert(c, &asked)?;
                rl_ledger::decisions::insert(c, &alone)?;
                Ok(())
            })
            .unwrap();
        app.refresh().await;

        app.select(Some("q:dec00001".into()));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Run { id: "run00001".into() }));

        app.select(Some("q:dec00002".into()));
        assert_eq!(app.summary_target(), None);
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.summarising.is_none());
        assert_eq!(
            app.flash.as_ref().map(|(m, _)| m.as_str()),
            Some("this question has no run to summarise")
        );
        token.cancel();
    }

    #[tokio::test]
    async fn brief_over_a_project_asks_nothing_first() {
        let (_tmp, token, mut app, folder) = panel_with_runs(3).await;
        select_header(&mut app);
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(app.mode, super::Mode::Normal));
        let s = app.summarising.as_ref().expect("brief is one call: straight on");
        assert_eq!(s.target, SummaryTarget::Project { workspace: folder });
        assert!(!s.deep);
        token.cancel();
    }

    #[tokio::test]
    async fn a_headers_kept_summary_is_in_its_pane() {
        let (tmp, token, mut app, folder) = panel().await;
        let away = jiff::Timestamp::from_millisecond(jiff::Timestamp::now().as_millisecond() - 3_600_000).unwrap();
        let kept = rl_proto::types::Summary {
            run: String::new(),
            agent: "fakeai".into(),
            by: "run09999".into(),
            at: jiff::Timestamp::now(),
            text: "- make test passed".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: SummaryTarget::Project {
                workspace: folder.clone(),
            }
            .key(),
            deep: false,
            from: Some(away),
            calls: 1,
            runs: 1,
        };
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger.call_blocking(move |c| rl_ledger::kept::put(c, &kept)).unwrap();
        app.away_since = Some(away);
        app.select(Some(format!("g:{}", folder.display())));
        app.moved().await;
        app.show_pane = true;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        assert!(buf_has(&buf, "- make test passed"));
        // What it is of first; the folder is long here, so the rest wraps
        // where it may.
        assert!(buf_has(&buf, " summary  of the project "));
        assert!(buf_has(&buf, "fakeai") && buf_has(&buf, "since "));
        token.cancel();
    }

    /// The screen as drawn at 120×40, its rows joined by newlines.
    fn draw_text(app: &mut super::App) -> String {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn away_is_the_presence_before_this_open() {
        use jiff::{SignedDuration, Timestamp};
        let now = Timestamp::from_second(100_000).unwrap();
        let h = |n| SignedDuration::from_hours(n);
        assert_eq!(super::away(Some(now - h(9)), now, Some(h(8))), Some(now - h(9)));
        assert_eq!(
            super::away(Some(now - h(7)), now, Some(h(8))),
            None,
            "not away long enough"
        );
        assert_eq!(
            super::away(None, now, Some(h(8))),
            None,
            "a first open has nothing to catch up on"
        );
        assert_eq!(super::away(Some(now - h(90)), now, None), None, "after: off");
    }

    #[tokio::test]
    async fn the_catch_up_shows_and_esc_goes_to_the_list() {
        use jiff::{SignedDuration, Timestamp};
        let (_tmp, token, mut app, _folder) = panel().await;
        assert!(app.digest.items.iter().any(|i| i.unseen), "news before it is looked at");
        let since = Timestamp::now() - SignedDuration::from_hours(9);
        app.open_catch_up(since).await;
        assert!(app.view == super::View::CatchUp);
        assert_eq!(app.away_since, Some(since), "a project's /summary starts there");
        let frame = draw_text(&mut app);
        assert!(frame.contains("while you were away"), "{frame}");
        assert!(frame.contains("api"), "{frame}");
        assert!(frame.contains("make test"), "{frame}");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        assert!(
            app.digest.items.iter().all(|i| !i.unseen),
            "what the catch-up showed is seen once you leave it"
        );
        token.cancel();
    }

    #[tokio::test]
    async fn enter_in_the_catch_up_goes_to_the_row_on_the_list() {
        use jiff::{SignedDuration, Timestamp};
        let (_tmp, token, mut app, _folder) = panel().await;
        app.open_catch_up(Timestamp::now() - SignedDuration::from_hours(9))
            .await;
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some(RUN), "under the project's line");
        assert!(
            app.digest.items.iter().any(|i| i.unseen),
            "moving about the catch-up is not leaving it"
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        assert_eq!(app.sel.as_deref(), Some(RUN));
        assert!(app.digest.items.iter().all(|i| !i.unseen));
        token.cancel();
    }

    /// An open question in the project, `ship it?`, asked by no run.
    fn ask_in(tmp: &tempfile::TempDir, folder: &std::path::Path) {
        use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
        let now = jiff::Timestamp::now();
        let asked = Decision {
            id: "dec00001".into(),
            workspace: folder.to_path_buf(),
            run: None,
            flow: "f".into(),
            group: None,
            title: "ship it?".into(),
            summary: None,
            options: vec![DecisionOption {
                key: "ok".into(),
                ..Default::default()
            }],
            multi: false,
            status: DecisionStatus::Open,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        };
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(move |c| rl_ledger::decisions::insert(c, &asked))
            .unwrap();
    }

    fn nine_hours_ago() -> jiff::Timestamp {
        jiff::Timestamp::now() - jiff::SignedDuration::from_hours(9)
    }

    fn unseen(app: &super::App, id: &str) -> bool {
        app.digest.items.iter().any(|i| i.run.id == id && i.unseen)
    }

    #[tokio::test]
    async fn a_number_answers_a_question_in_the_catch_up() {
        let (tmp, token, mut app, folder) = panel().await;
        ask_in(&tmp, &folder);
        app.refresh().await;
        app.open_catch_up(nine_hours_ago()).await;
        assert!(draw_text(&mut app).contains("ship it?"));
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(
            app.sel.as_deref(),
            Some("q:dec00001"),
            "what needs you leads its project"
        );
        type_in(&mut app, "1").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp, "answered where it stands");
        assert!(app.digest.decisions.is_empty(), "{:?}", app.flash);
        assert!(!draw_text(&mut app).contains("ship it?"));
        token.cancel();
    }

    #[tokio::test]
    async fn landing_on_a_question_in_the_catch_up_opens_its_pane() {
        let (tmp, token, mut app, folder) = panel().await;
        ask_in(&tmp, &folder);
        app.refresh().await;
        app.open_catch_up(nine_hours_ago()).await;
        assert!(!app.show_pane, "on the project's line");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some("q:dec00001"));
        assert!(app.show_pane, "a question is only worth its details, as on the list");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert!(!app.show_pane, "and off it the pane goes again");
        token.cancel();
    }

    #[tokio::test]
    async fn enter_on_a_job_in_the_catch_up_goes_to_its_row_on_the_list() {
        let (_tmp, token, mut app, folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "nightly",
                vec!["sh".into(), "-c".into(), "true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Schedule("nightly".into());
            vec![r]
        })
        .await;
        app.open_catch_up(nine_hours_ago()).await;
        // Its rule is there: the list is still where enter goes, and `^g`
        // from there is the graph.
        app.rules
            .push(rl_proto::rule::Rule::Schedule(rl_proto::schedule::Schedule {
                scope: folder.to_string_lossy().into_owned(),
                name: "nightly".into(),
                hash: "h".into(),
                source: folder.join(".relay/schedules.yaml"),
                spec: rl_proto::schedule::ScheduleSpec {
                    every: Some("1h".into()),
                    run: Some(rl_proto::action::Task::Shell("true".into())),
                    ..Default::default()
                },
                paused: false,
                last_fired_at: None,
                last_run: None,
                next_at: None,
                removed_at: None,
                approved_hash: Some("h".into()),
            }));
        let job = app
            .catch_rows()
            .iter()
            .find_map(|r| match r {
                super::Row::Job(j) => Some(j.key.clone()),
                _ => None,
            })
            .expect("the nightly job in the catch-up");
        app.select(Some(job.clone()));
        assert!(app.rule_here().is_some());
        assert!(app.hints().starts_with("enter it on the list"), "{}", app.hints());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "the list has job rows of its own");
        assert_eq!(app.sel.as_deref(), Some(job.as_str()));
        assert!(!unseen(&app, "run00002"));
        token.cancel();
    }

    /// Every way out goes through `back_to_list`: here the pane's attach
    /// choice, which starts nothing. (Starting a run from the line takes
    /// the same way out, but needs the `rl-session` binary, which a unit
    /// test does not have.)
    #[tokio::test]
    async fn leaving_the_catch_up_any_way_marks_what_it_showed() {
        let (_tmp, token, mut app, _folder) = panel_with_runs(2).await;
        app.open_catch_up(nine_hours_ago()).await;
        assert!(unseen(&app, "run00001"));
        app.attach_to("run00002".into(), AREA).await;
        assert!(app.view == super::View::Work);
        assert_eq!(
            app.sel.as_deref(),
            Some("r:run00002"),
            "an ended one is read in the pane"
        );
        assert!(
            !unseen(&app, "run00001"),
            "what the catch-up showed is seen, not only the run gone to"
        );
        assert!(app.catch.is_none());
        token.cancel();
    }

    #[tokio::test]
    async fn closing_the_panel_on_the_catch_up_marks_what_it_showed() {
        let (_tmp, token, mut app, _folder) = panel().await;
        app.open_catch_up(nine_hours_ago()).await;
        app.saw_catch_up().await;
        assert!(!unseen(&app, "run00001"));
        token.cancel();
    }

    #[tokio::test]
    async fn the_list_marks_nothing_seen_when_there_was_no_catch_up() {
        let (_tmp, token, mut app, _folder) = panel().await;
        app.saw_catch_up().await;
        assert!(unseen(&app, "run00001"));
        token.cancel();
    }

    #[tokio::test]
    async fn a_search_narrows_the_catch_up_and_only_what_it_shows_is_seen() {
        let (_tmp, token, mut app, _folder) = panel_with_runs(2).await;
        app.open_catch_up(nine_hours_ago()).await;
        app.filter = "echo".into();
        app.settle();
        let keys = app.keys();
        assert!(keys.iter().any(|k| k == "r:run00002"), "{keys:?}");
        assert!(!keys.iter().any(|k| k == RUN), "{keys:?}");
        let frame = draw_text(&mut app);
        assert!(frame.contains("echo 2") && !frame.contains("make test"), "{frame}");
        app.select(Some("r:run00002".into()));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        assert!(!unseen(&app, "run00002"));
        assert!(unseen(&app, "run00001"), "the search kept it out of sight");
        token.cancel();
    }

    /// Hands the top, once it is written, to the panel as the main loop
    /// would.
    async fn wait_topped(app: &mut super::App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while let Some(t) = app.topping.as_mut() {
            if t.task.is_finished() {
                let done = (&mut t.task).await;
                app.topped(done).await;
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the top was never written");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn the_top_starts_counted_and_the_assistants_replaces_it() {
        let Some((_tmp, token, mut app, _folder)) = panel_with_fake_summariser("• api: one run, fine").await else {
            return;
        };
        let since = nine_hours_ago();
        app.open_catch_up(since).await;
        assert!(app.topping.is_some(), "auto: the top is being written");
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: 1 ran"), "counted bullets first: {frame}");
        assert!(frame.contains("writing the top"), "{frame}");
        wait_topped(&mut app).await;
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: one run, fine"), "{frame}");
        assert!(frame.contains("by fakeai · from 1 run"), "{frame}");
        assert!(!frame.contains("• api: 1 ran"), "the bullets gave way: {frame}");
        // Kept: the same stretch again asks no one.
        app.back_to_list().await;
        app.open_catch_up(since).await;
        assert!(app.topping.is_none(), "the one kept is given back");
        assert!(draw_text(&mut app).contains("one run, fine"));
        token.cancel();
    }

    #[tokio::test]
    async fn a_top_that_fails_keeps_the_bullets_and_says_why() {
        let Some((_tmp, token, mut app, _folder)) = panel_with_fake_summariser("").await else {
            return;
        };
        app.open_catch_up(nine_hours_ago()).await;
        wait_topped(&mut app).await;
        let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert!(said.contains("said nothing"), "{said}");
        assert!(draw_text(&mut app).contains("• api: 1 ran"));
        token.cancel();
    }

    #[tokio::test]
    async fn ask_means_no_call_on_open() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        app.open_catch_up(nine_hours_ago()).await;
        assert!(app.topping.is_none());
        assert!(app.summarising.is_none());
        let frame = draw_text(&mut app);
        assert!(
            frame.contains("• api: 1 ran") && !frame.contains("writing the top"),
            "{frame}"
        );
        token.cancel();
    }

    #[tokio::test]
    async fn a_rows_summary_goes_ahead_while_the_top_is_written() {
        let (_tmp, token, mut app, _folder) = panel_with_config("").await;
        app.open_catch_up(nine_hours_ago()).await;
        assert!(app.topping.is_some());
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some(RUN));
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let s = app.summarising.as_ref().expect("not held up by the top");
        assert_eq!(s.target, SummaryTarget::Run { id: "run00001".into() });
        assert!(app.topping.is_some());
        token.cancel();
    }

    #[tokio::test]
    async fn summary_on_the_top_is_of_everything_since_the_catch_up_began() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        let since = nine_hours_ago();
        app.open_catch_up(since).await;
        assert_ne!(app.sel.as_deref(), Some(super::TOP), "the cursor starts on the rows");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some(super::TOP), "up from the first row is the top");
        assert_eq!(app.summary_target(), Some(SummaryTarget::All));
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let s = app.summarising.as_ref().expect("asked");
        assert_eq!(s.target, SummaryTarget::All);
        assert_eq!(s.from, Some(since));
        assert!(!app.show_pane, "it is written in the top, not the pane");
        token.cancel();
    }

    #[tokio::test]
    async fn summary_on_the_top_waits_while_it_is_written() {
        let (_tmp, token, mut app, _folder) = panel_with_config("").await;
        app.open_catch_up(nine_hours_ago()).await;
        app.select(Some(super::TOP.into()));
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.summarising.is_none());
        let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert!(said.contains("still being written"), "{said}");
        token.cancel();
    }

    #[tokio::test]
    async fn since_opens_the_catch_up_from_anywhere() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        let before = jiff::Timestamp::now();
        type_in(&mut app, "/since 1h").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp);
        let since = app.catch.as_ref().unwrap().since;
        let hour = jiff::SignedDuration::from_hours(1);
        assert!(
            since >= before - hour && since <= jiff::Timestamp::now() - hour,
            "{since}"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        // From the graph too; bare, since you were last here.
        let left = nine_hours_ago();
        app.left = Some(left);
        app.toggle_graph().await;
        assert!(app.view == super::View::Graph);
        type_in(&mut app, "/since").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp);
        assert_eq!(app.catch.as_ref().unwrap().since, left);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "esc goes to the list, not the graph");
        type_in(&mut app, "/since soon").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "a stretch it cannot read opens nothing");
        token.cancel();
    }

    fn buf_has(buf: &ratatui::buffer::Buffer, s: &str) -> bool {
        let (w, h) = (buf.area.width, buf.area.height);
        (0..h).any(|y| {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            row.contains(s)
        })
    }
}
