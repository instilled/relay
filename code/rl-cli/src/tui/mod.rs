//! The control panel: one list, one keystroke to anything.
//!
//! Everything ranked by attention — questions, then runs that need you,
//! then what is working and what finished — grouped by the folder it belongs
//! to, whose flows ride the group's header. `enter` is the run's terminal,
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

mod act;
mod canvas;
mod carry;
mod catch_ops;
mod catchup;
mod complete;
mod events;
mod feed_ops;
mod firings;
mod flow_graph;
mod flow_links;
#[cfg(test)]
mod flow_new_tests;
mod flow_ops;
#[cfg(test)]
mod flow_tests;
pub(crate) mod flows;
pub mod form;
mod input;
mod join;
mod keys;
mod line;
mod mark;
mod new_flow;
pub(crate) mod outside;
#[cfg(test)]
mod outside_tests;
mod palette;
mod pane;
mod query;
mod remove;
mod select;
mod shell_ops;
mod summary_ops;

pub use pane::{recap_label, summary_label};
pub(crate) mod rows;
mod screen;
mod setup;
mod shortcuts;
mod term;
mod tested;
mod theme;

use crate::attach::client_name;
use crate::catchup::heads;
use crate::client::Client;
use crate::context::Want;
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
use rl_proto::flow::{
    Firing, FiringFilter, Flow, FlowApproval, FlowApprove, FlowErrors, FlowFilter, FlowFirings, FlowList, FlowName,
    FlowPause, FlowReload, FlowResume, FlowRun, FlowStatus, NotLoaded,
};
use rl_proto::methods::{
    AgentList, DecisionDismiss, DecisionGet, DecisionId, DecisionNote, DecisionTake, DecisionTakeParams, Digest,
    DigestGet, DigestHide, DigestHideParams, DigestItem, DigestOrder, DigestParams, EventFilter, EventList, GcParams,
    GcRun, ImportList, ImportListParams, ImportMove, PresenceClient, PresenceTouch, RunArchive, RunArchiveParams,
    RunFilter, RunGet, RunHide, RunHideParams, RunId, RunIds, RunInput, RunInputParams, RunList, RunMark,
    RunMarkParams, RunPin, RunPinParams, RunRename, RunRenameParams, RunResume, RunResumeParams, RunSeen, RunSpec,
    RunStart, RunStop, RunStopParams, SETUP_STEPS, SetupGet, SummaryGet, SummaryMake, SummaryParams, WorkspaceList,
    WorkspaceRemove, WorkspaceRemoveParams, WorkspaceRemoved, WorkspaceRename, WorkspaceRenameParams, WorkspaceUp,
    WorkspaceUpParams,
};
use rl_proto::types::{Cause, Event as LedgerEvent, Mark, Run, RunKind, RunState, Summary, SummaryTarget};
use rows::{Names, Row, fit};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use term::Output;

const REFRESH: Duration = Duration::from_secs(1);
/// How often an open panel tells presence you are here: a window closed
/// without quitting says so no later than this before it went.
const PRESENCE_EVERY: Duration = Duration::from_secs(60);
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
/// Recent output asked for when joining a pipes run, and when reading an
/// ended one's page, enough to paint the screen on arrival. A live terminal
/// run is drawn from its holder's screen instead. Bytes.
pub(super) const SCROLLBACK: u64 = 256 * 1024;
/// Where the header and footer text starts: level with what is typed on the
/// line, past its `❯ `.
const TEXT_INDENT: u16 = 2;
/// The tall header — the mark, and three lines beside it — wants a screen
/// at least this tall and this wide; on a smaller one the header is one line.
const TALL_H: u16 = 24;
const TALL_W: u16 = 80;
/// The air between the header's mark and its words.
const MARK_GAP: u16 = 3;
/// The air above the tall header, so the mark does not sit on the screen's
/// top edge.
const HEAD_AIR: u16 = 1;

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
    /// A run being written down, by hand or drafted for you.
    Form(Box<Form>),
    /// A question taken out of the list: its options, answered by number.
    Answer(String),
    /// `enter` on a flow whose pane is up: its steps, then its last firings, walked
    /// (`App::flow_pane`).
    Walk,
    /// A join refused because someone holds the keyboard: `y` takes it from
    /// them. The run, and who has it.
    Take(String, String),
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
    /// A flow the assistant drafted, in the pane: `enter` writes it and says yes, `e` opens it in
    /// `$EDITOR` first, `esc` lets it go.
    Draft(Box<new_flow::Drafted>),
    /// `^x` on a project: taking a whole project down is not a double-press
    /// you can slip on — you type `DELETE` and `enter`, `esc` keeps it.
    Confirm(Box<Confirming>),
}

/// A project waiting on you to confirm its removal by typing `DELETE`: the
/// folder and name to remove, a line of what it will take, and what is typed
/// so far. Only the exact word, then `enter`, removes it.
struct Confirming {
    workspace: PathBuf,
    name: String,
    /// `remove api · stops 2 · archives 7 ended runs` — what removal does.
    plan: String,
    field: line::Field,
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

/// A project being removed, until the daemon answers: stopping its runs can
/// take a few seconds, and the panel goes on meanwhile.
struct Removing {
    name: String,
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
    /// What was asked for: whatever fits the words, or a flow by name.
    want: crate::context::Want,
    task: tokio::task::JoinHandle<Result<(crate::interpret::Answer, rl_proto::methods::AssistantOutput)>>,
}

/// One of an agent's own sessions on its way to being carried on as a run
/// (`import.move`), until `rld` says how it went: the panel stays live.
struct Carrying {
    session: rl_proto::methods::Outside,
    task: tokio::task::JoinHandle<Result<Run>>,
}

/// A run, a flow, a project or everything being summarised, until the
/// assistant answers.
struct Summarising {
    target: SummaryTarget,
    /// What it is of, in words: the run's title, `flow nightly`, the
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
/// flow's firings not seen start, by its row key.
#[derive(Default)]
struct Shown {
    ids: Vec<String>,
    from: Vec<(String, Timestamp)>,
}

struct App {
    home: Home,
    /// The Relay home as runs' folders are recorded: canonical. An
    /// assistant's run there is off the list's groups.
    relay: PathBuf,
    rpc: Client,
    me: String,
    digest: Digest,
    names: Names,
    /// A search the list is narrowed to; see `query`.
    filter: String,
    /// `^f`: the line is searching, and the filter follows what is typed.
    search: Option<Search>,
    /// The list toggles. Flows, agents and shells are additive and on by
    /// default — each shows the runs it is for; hidden is off by default and,
    /// on, stands what you hid inline in its place.
    show_flows: bool,
    show_agents: bool,
    show_shells: bool,
    show_hidden: bool,
    view: View,
    /// Every flow: for its row's place and words, the chips on project rows,
    /// and a flow's pane.
    flows: Vec<Flow>,
    /// What did not load at the last reload: a red `! flows` on the header
    /// of the project it was in, the headline's `flows broken` for the rest.
    not_loaded: Vec<NotLoaded>,
    /// What the last `/reload` did not load, when it was more than the flash
    /// has room for: listed in the pane until the next key.
    reload_list: Option<Vec<NotLoaded>>,
    /// What `/flow test` found, as lines, until the next key.
    tested: Option<Vec<String>>,
    /// A flow drafted while the keys were another's: shown by `/flow draft`.
    parked: Option<new_flow::Parked>,
    /// The views gone through to reach the one that is up, the list first:
    /// going back walks them.
    back: Vec<View>,
    /// Each view's selection while another is up.
    kept: HashMap<View, Option<String>>,
    /// The events view's search while another view is up, or the list's
    /// while it is: `kind:` means another thing there.
    filter_aside: String,
    /// What the events view has read of the ledger.
    feed: events::Feed,
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
    /// A subpage is up and has the whole body; off, the list has it.
    show_pane: bool,
    /// The highlighted choice of the question being answered.
    answer_at: usize,
    /// The flow whose pane is up: its last firings, the one shown, the caret while walked.
    flow_pane: firings::FlowPane,
    /// The flows' rows opened with `→` to their latest firing's steps, by row key.
    open_flows: HashSet<String>,
    /// What the question pane drew last, so a click can take a choice.
    pane_hit: std::cell::RefCell<PaneHit>,
    term: Option<Output>,
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
    /// What each firing heading the catch-up came to (`catch_up: top`), by firing.
    outcomes: heads::Outcomes,
    /// The keys of the leads the catch-up drew last, the ones the room let it show; until it is
    /// drawn, all of them. The cursor stops only on these.
    leads_shown: std::cell::RefCell<Option<Vec<String>>>,
    /// `/summary deep` waiting on `y`: what it would ask.
    summary_ask: Option<SummaryParams>,
    /// The last summary made of each target (`SummaryTarget::key`) while the
    /// panel is open: which stretch and depth the pane asks the kept one
    /// for, and what it shows if none is kept.
    made: HashMap<String, Summary>,
    /// Where a project's or everything's `/summary` starts with no stretch
    /// typed: when you were last here before this open, or while a catch-up
    /// is up, where its stretch starts. None: the last day.
    away_since: Option<Timestamp>,
    /// When you were last here before this open, as presence said: where a
    /// bare `/since` starts. None: the first open.
    left: Option<Timestamp>,
    /// When this panel last told presence you are here: on open, then on
    /// the tick once a minute, and as it closes.
    presence_at: Option<Instant>,
    /// The catch-up's stretch, while it is up or on the way back to it: the
    /// list's digest cut to what happened since it starts, again on each
    /// refresh. The list's own stays as it is.
    catch: Option<Digest>,
    /// While a frame is drawn, the catch-ups built for it, by the search
    /// each is narrowed by: built once a draw, and let go when it ends.
    drawn: std::cell::RefCell<Option<Vec<(String, crate::catchup::Kept)>>>,
    /// Where a flow row's `/summary` starts with no stretch typed, by row key:
    /// when its oldest firing not seen started, noted as the cursor lands on
    /// it — landing marks them seen, so the daemon could no longer tell.
    /// Kept until used, or until the row has firings not seen again.
    unseen_from: HashMap<String, Timestamp>,
    /// A project on its way out (`^x` twice on its header).
    removing: Option<Removing>,
    /// `/flow edit`: the flow being edited, and the line of its `flow.md` to
    /// open the editor at once the screen is given back.
    editor: Option<(crate::flow::Editing, usize)>,
    /// A run to hand the real terminal to (its id, and whether to take the
    /// keyboard from whoever holds it), once ratatui has stepped aside.
    attach: Option<(String, bool)>,
    /// An agent's own session to pass the terminal through to — `claude
    /// attach <id>` (`carry`) — once ratatui has stepped aside.
    attach_outside: Option<Vec<std::ffi::OsString>>,
    /// Where an agent's binary is, by its profile's name (`carry::bin`).
    agent_bin: fn(&str) -> Option<PathBuf>,
    /// An agent's own session being carried on.
    carrying: Option<Carrying>,
    /// `/import`: the Claude sessions step, once the screen is free for it.
    importing: bool,
    /// The assistant at work on some words: what was asked, where, and the
    /// task waiting for the answer.
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
/// The state column's width: `needs you~` and a flow's marks both fit.
const WORD_W: usize = 10;

/// Where the state column starts, for a title column `tw` wide — where a
/// header's checkout starts too, so the two line up.
fn state_col(tw: usize) -> usize {
    ICON_W + tw + 1
}

/// A row's icon, in its slot and its colour — an agent's its CLI's (`cli`);
/// `ended`, quiet.
fn icon_span(icon: &str, cli: Option<&str>, ended: bool) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled(icon.to_string(), rows::icon_style(icon, cli, ended)),
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

/// `spans` in at most `room` columns, the last cut with `…` where it does
/// not fit whole, and what comes after it dropped.
fn cap_spans(spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut left = room;
    for s in spans {
        let n = s.width();
        if n <= left {
            left -= n;
            out.push(s);
        } else {
            if left > 0 {
                out.push(Span::styled(fit(&s.content, left), s.style));
            }
            break;
        }
    }
    out
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
    // What goes before the summary has the summary's room at most, so a
    // narrow list still ends in its age.
    let lead = cap_spans(
        lead,
        w.saturating_sub(state_col(tw) + WORD_W + 1 + right.chars().count() + 1),
    );
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

/// How many of a flow's runs the row shows as marks.
const ROW_MARKS: usize = 5;

/// Who is joined, for the slot before a row's summary.
fn joined_lead(joined: Option<&str>) -> Vec<Span<'static>> {
    joined
        .map(|j| vec![Span::raw(format!("● {j}  ")).dim()])
        .unwrap_or_default()
}

/// How a flow has been going, oldest first, for its state column: `···×·`.
fn flow_marks(f: &rows::FlowRow) -> Vec<Span<'static>> {
    f.marks(ROW_MARKS)
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
/// flows, with any that wait for a yes just before it, in amber. Each piece
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
    // The tally at the right edge is the honest total of the flows defined
    // here; before it, in amber, a hint when something that needs you is off
    // the list — hidden, or a toggle dropped it — so nothing urgent is buried.
    let mut right: Vec<Span<'static>> = Vec::new();
    // A flow here that did not load comes first, in red: what is loaded is not
    // what its folder says.
    if broken {
        right.push(Span::styled(rows::BROKEN_CHIP, theme::bad()));
        right.push(Span::raw("  "));
    }
    if let Some(hint) = rows::needs_hint(h.attention) {
        right.push(Span::styled(hint, theme::needs()));
        right.push(Span::raw("  "));
    }
    let count = group_count(&h.flows);
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
/// tone: what a restart took, the flows that did not load.
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
/// at all — no search, no toggle — the mark stands full size in the middle of
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
    /// The separators between the sections: under the header, above the line,
    /// above the footer. Empty while a subpage is up.
    seps: Vec<Rect>,
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

#[allow(clippy::too_many_arguments)]
fn areas(area: Rect, show_pane: bool, form_h: u16, cands_h: u16, line_h: u16, foot_h: u16) -> Areas {
    // The panel runs to the screen's edges, with separators between its sections —
    // but not under a subpage, which wants every cell.
    let chrome = !show_pane;
    let sep = u16::from(chrome);
    let [head, sep_a, body, form, cands, sep_b, line, sep_c, footer] = Layout::vertical([
        Constraint::Length(head_h(area, show_pane)),
        Constraint::Length(sep),
        Constraint::Fill(1),
        Constraint::Length(if form_h > 0 { form_h + 1 } else { 0 }),
        Constraint::Length(cands_h),
        Constraint::Length(sep),
        Constraint::Length(line_h.max(1)),
        Constraint::Length(sep),
        Constraint::Length(foot_h.max(1)),
    ])
    .areas(area);
    let seps = if chrome { vec![sep_a, sep_b, sep_c] } else { vec![] };
    let form = (form_h > 0).then_some(form);
    let cands = (cands_h > 0).then_some(cands);
    // A subpage has the body to itself, and the list has it otherwise.
    let (list, pane) = if show_pane {
        (None, Some(body))
    } else {
        (Some(body), None)
    };
    Areas {
        head,
        seps,
        list,
        pane,
        form,
        cands,
        line,
        footer,
    }
}

/// How tall the header is: a row of air and the small mark's rows, with
/// three lines beside it, where the screen has room; one line under a subpage, which
/// wants every cell, and on a screen too short or too narrow to spare them.
fn head_h(area: Rect, show_pane: bool) -> u16 {
    if show_pane || area.height < TALL_H || area.width < TALL_W {
        1
    } else {
        HEAD_AIR + mark::SMALL.len() as u16
    }
}

/// A subpage's rows: the one-row header that names it and the body under it.
/// Where the pane is a single row there is only the body, and no header.
fn subpage_rows(pane: Rect) -> (Option<Rect>, Rect) {
    if pane.height <= 1 {
        return (None, pane);
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(pane);
    (Some(head), body)
}

/// The size a terminal gets in a subpage: the body under its header.
fn term_size(size: Rect, line_h: u16, foot_h: u16) -> (u16, u16) {
    let a = areas(size, true, 0, 0, line_h, foot_h);
    let (_, body) = subpage_rows(a.pane.unwrap_or(size));
    (body.width.max(1), body.height.max(1))
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
        setup::Guide::new(&state, &sessions, setup::hookable(&state.agents))
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

/// Whether it is time to touch presence again, last touched by this panel
/// at `last` (never: at once).
pub(crate) fn presence_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= PRESENCE_EVERY)
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
    app.presence_at = Some(Instant::now());
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
        // The agents' own hooks files are read only when the guide could
        // ask about them.
        let hooks = if state.seen < SETUP_STEPS {
            setup::hookable(&state.agents)
        } else {
            vec![]
        };
        match setup::Guide::since(&state, &sessions, hooks) {
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
        app.open_catch_up(since, true).await;
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
            relay: std::fs::canonicalize(home.path()).unwrap_or_else(|_| home.path().to_path_buf()),
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
                firings: Vec::new(),
                outside: vec![],
            },
            names: Names::default(),
            filter: String::new(),
            search: None,
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            view: View::Work,
            flows: Vec::new(),
            back: Vec::new(),
            kept: HashMap::new(),
            filter_aside: String::new(),
            feed: events::Feed::default(),
            shell_at: HashMap::new(),
            scroll: std::cell::Cell::new(0),
            list_h: std::cell::Cell::new(0),
            hit: std::cell::RefCell::default(),
            sel: None,
            pane: Pane::Facts,
            show_pane: false,
            answer_at: 0,
            flow_pane: Default::default(),
            open_flows: HashSet::new(),
            pane_hit: std::cell::RefCell::default(),
            term: None,
            term_size: (0, 0),
            events: Vec::new(),
            editor: None,
            attach: None,
            attach_outside: None,
            agent_bin: carry::bin,
            carrying: None,
            importing: false,
            not_loaded: Vec::new(),
            reload_list: None,
            tested: None,
            parked: None,
            asking: None,
            summary: None,
            summarising: None,
            topping: None,
            top: None,
            outcomes: Default::default(),
            leads_shown: Default::default(),
            summary_ask: None,
            made: HashMap::new(),
            away_since: None,
            left: None,
            presence_at: None,
            catch: None,
            drawn: std::cell::RefCell::default(),
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
                    t.relayout(size);
                }
            }
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                        self.key(input::typed(k), area).await?;
                        if let Some((editing, line)) = self.editor.take() {
                            // The editor gets the terminal; our reader lets go of stdin first.
                            drop(input);
                            leave_screen();
                            let outcome = crate::env::open_in_editor(&editing.file, line);
                            *terminal = enter_screen();
                            input = EventStream::new();
                            match outcome {
                                Ok(()) => self.edited(editing).await,
                                Err(e) => {
                                    // However the editor ends, a new flow's template is not left behind.
                                    match crate::flow::drop_new(&self.home, &editing, true) {
                                        Err(left) => self.say(format!("{e:#}; {left:#}")),
                                        Ok(_) => self.say(format!("{e:#}")),
                                    }
                                }
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
                                    take,
                                    scrollback: SCROLLBACK,
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
                        if let Some(argv) = self.attach_outside.take() {
                            // The agent gets the terminal passed through, as a joined run
                            // does, `ctrl-\` letting go; the list comes back however it ends,
                            // and the next poll says how it stands.
                            drop(input);
                            leave_screen();
                            let outcome = carry::attach(&argv).await;
                            *terminal = enter_screen();
                            input = EventStream::new();
                            self.refresh().await;
                            match outcome {
                                Ok(Some(words)) => self.say(words),
                                Ok(None) => {}
                                Err(why) => self.say(why),
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
                done = async {
                    match &mut self.carrying {
                        Some(c) => (&mut c.task).await,
                        None => std::future::pending().await,
                    }
                } => self.carried(done).await,
                gone = async {
                    match &mut self.removing {
                        Some(r) => (&mut r.task).await,
                        None => std::future::pending().await,
                    }
                } => self.removed(gone).await,
            }
        }
        Ok(())
    }

    fn say(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    /// The area the terminal draws into.
    fn pane_size(&self, size: Rect) -> (u16, u16) {
        term_size(size, self.line_h(), self.foot_h(size.width))
    }

    /// How many rows the live line takes: its own, capped, at least one.
    fn line_h(&self) -> u16 {
        if self.search.is_some() {
            return 1;
        }
        (self.line.rows() as u16).clamp(1, LINE_MAX)
    }

    /// How many rows the footer takes at this width: one for the hints, or
    /// the shortcuts' rows when they are up, with the flash's row above them
    /// while there is a flash.
    fn foot_h(&self, width: u16) -> u16 {
        match self.shortcut_lines(width).len() as u16 {
            0 => 1,
            n => n + u16::from(self.flashing().is_some()),
        }
    }

    /// What the footer says for a moment, while it still does.
    fn flashing(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| at.elapsed() < FLASH)
            .map(|(m, _)| m.as_str())
    }

    /// The rows of the footer at this width: the shortcuts where they are up,
    /// under a flash if there is one, else the one row of hints.
    fn foot_lines(&self, width: u16) -> Vec<Line<'static>> {
        let keys = self.shortcut_lines(width);
        if keys.is_empty() {
            let room = width.saturating_sub(TEXT_INDENT) as usize;
            return vec![Line::from(self.footer(room)).dim()];
        }
        let flash = self.flashing().map(|m| Line::from(m.to_string()).dim());
        flash.into_iter().chain(keys).collect()
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
            Mode::Walk => return Some(shortcuts::Place::Walk),
            _ => return None,
        }
        if self.search.is_some() {
            return None;
        }
        // The same test as the footer's: the output page's keys while it is up.
        if self.pane == Pane::Term && self.term.is_some() {
            return Some(shortcuts::Place::Term {
                wide: true,
                live: false,
            });
        }
        Some(match self.view {
            View::Events => shortcuts::Place::Events,
            View::CatchUp => shortcuts::Place::CatchUp,
            View::Work if self.selected_question().is_some() => shortcuts::Place::Question,
            View::Work if self.show_hidden => shortcuts::Place::Hidden,
            View::Work => shortcuts::Place::List,
        })
    }

    /// The shortcuts as drawn in the footer at this width, wrapped between
    /// keys; none while they are put away.
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

    /// Flip the flows toggle, and settle the selection onto what now shows.
    fn toggle_flows(&mut self) {
        self.show_flows = !self.show_flows;
        self.settle();
    }

    /// Flip the agents toggle, and settle the selection.
    fn toggle_agents(&mut self) {
        self.show_agents = !self.show_agents;
        self.settle();
    }

    /// Flip the shells toggle, and settle the selection.
    fn toggle_shells(&mut self) {
        self.show_shells = !self.show_shells;
        self.settle();
    }

    /// Flip the hidden toggle, and settle the selection.
    fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.settle();
    }

    /// The toggles as they stand, as `(flows, agents, shells, hidden)`.
    fn toggles(&self) -> (bool, bool, bool, bool) {
        (self.show_flows, self.show_agents, self.show_shells, self.show_hidden)
    }

    /// The toggles back to their default: flows, agents and shells on, hidden off.
    fn reset_toggles(&mut self) {
        self.set_toggles((true, true, true, false));
    }

    /// Put the toggles to a set, `(flows, agents, shells, hidden)`.
    fn set_toggles(&mut self, (flows, agents, shells, hidden): (bool, bool, bool, bool)) {
        self.show_flows = flows;
        self.show_agents = agents;
        self.show_shells = shells;
        self.show_hidden = hidden;
    }

    async fn refresh(&mut self) {
        if let Ok(ws) = self.rpc.call::<WorkspaceList>(()).await {
            self.names = Names::new(ws.into_iter().map(|w| (w.path, w.name)));
        }
        match self
            .rpc
            .call::<DigestGet>(DigestParams {
                watching: true,
                ..Default::default()
            })
            .await
        {
            Ok(d) => self.digest = d,
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        match self.rpc.call::<FlowList>(FlowFilter::default()).await {
            Ok(f) => self.flows = f,
            Err(e) => self.say(format!("daemon: {e:#}")),
        }
        if let Ok(n) = self.rpc.call::<FlowErrors>(()).await {
            self.not_loaded = n;
        }
        // The catch-up's stretch, while it is up or there to go back to: cut
        // from the list's digest, not asked for again.
        match self.catch.as_ref().map(|c| c.since) {
            Some(since) if self.view == View::CatchUp || self.back.contains(&View::CatchUp) => {
                self.catch = Some(crate::catchup::stretch(&self.digest, since));
            }
            _ => self.catch = None,
        }
        self.fetch_leads().await;
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
        // A flow gone from the list takes its open steps line with it; one only a search or a
        // toggle hides keeps it, for when it shows again.
        let all = rows::Context {
            names: &self.names,
            flows: &self.flows,
            filter: "",
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            now: Timestamp::now(),
            relay: Some(&self.relay),
        };
        let flow_keys: HashSet<String> = rows::build(&self.digest, &all)
            .iter()
            .filter_map(|r| match r {
                Row::Flow(f) => Some(f.key.clone()),
                _ => None,
            })
            .collect();
        self.open_flows.retain(|k| flow_keys.contains(k));
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
        if let Some(name) = self.selected_flow().map(|f| f.name.clone()) {
            let filter = FiringFilter {
                flow: name.clone(),
                limit: Some(firings::LAST),
            };
            if let Ok(fs) = self.rpc.call::<FlowFirings>(filter).await {
                self.flow_pane.load(&name, fs);
                self.flow_pane.files = self
                    .flow_pane
                    .shown()
                    .map(|f| firings::files_in(&self.home.firing_dir(&name, &f.id)))
                    .unwrap_or_default();
            }
        }
        let Some(target) = self.summary_target() else { return };
        let made = self.made.get(&target.key()).cloned();
        let (deep, from) = match (&made, &target) {
            (Some(m), _) => (m.deep, m.from),
            (None, SummaryTarget::Project { .. } | SummaryTarget::All) => (false, self.away_since),
            (None, SummaryTarget::Flow { .. }) => {
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

    /// What the `everywhere` header says to a project's action.
    const NOT_A_PROJECT: &'static str = "everywhere is not a project";

    /// Leave the subpage for the list, the one way out: the page it was reading is let go, a
    /// sub-mode inside it (an answer, a walk, a name) ends, and the row stays selected.
    fn leave_subpage(&mut self) {
        self.show_pane = false;
        self.pane = Pane::Facts;
        self.term = None;
        if matches!(self.mode, Mode::Answer(_) | Mode::Walk | Mode::Rename(_)) {
            self.mode = Mode::Normal;
        }
    }

    /// `/pane`, `^o`: the row's subpage, shown or left.
    fn toggle_pane(&mut self) {
        if self.show_pane {
            self.leave_subpage();
        } else {
            self.show_pane = true;
        }
    }

    async fn seen(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        mark_seen(&mut self.digest, &ids);
        let _ = self.rpc.call::<RunSeen>(RunIds { ids }).await;
    }

    // ---- doing things ------------------------------------------------------

    async fn answer(&mut self, id: String, options: Vec<String>, note: Option<String>) {
        let what = options.join(", ");
        match self
            .rpc
            .call::<DecisionTake>(DecisionTakeParams { id, options, note })
            .await
        {
            Ok(d) => {
                self.picked.clear();
                self.refresh().await;
                match d.outcome_runs.first() {
                    Some(r) => {
                        let title = self.run_by_id(r).map(|r| r.title.clone()).unwrap_or_default();
                        self.say(format!("{what} → started {title}"));
                        let started = format!("r:{r}");
                        if self.view == View::Work || self.keys().contains(&started) {
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

    /// What `^x` or `/archive` on a flow in the catch-up says.
    const FLOW_ON_THE_LIST: &'static str = "^x archives a flow's runs on the list · esc goes there";

    // ---- flows -------------------------------------------------------------

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
    /// the ledger. Once a minute presence hears you are here, so the next
    /// open counts from about when the panel went, however it went.
    async fn tick(&mut self) {
        if presence_due(self.presence_at, Instant::now()) {
            self.presence_at = Some(Instant::now());
            let _ = self
                .rpc
                .call::<PresenceTouch>(PresenceClient {
                    client: self.me.clone(),
                })
                .await;
        }
        self.refresh().await;
        if self.view == View::Events {
            self.fetch_events().await;
        }
    }

    // ---- the events view ---------------------------------------------------

    // ---- the catch-up -----------------------------------------------------

    /// The list, with the cursor on a run: its own row, or its flow's. When
    /// a toggle or the search hides it, they give way; when the list does
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
        let (key, opened) = match self.list_key(id, &list_filter, self.toggles()) {
            Some(k) => (k, None),
            None => match self
                .list_key(id, "", (true, true, true, false))
                .map(|k| (k, (true, true, true, false)))
                .or_else(|| {
                    self.list_key(id, "", (true, true, true, true))
                        .map(|k| (k, (true, true, true, true)))
                }) {
                Some((k, t)) => (k, Some(t)),
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
        if let Some(t) = opened {
            self.filter.clear();
            self.set_toggles(t);
            self.say(if t.3 {
                format!("'{title}' is hidden: shown here with hidden on, /hide brings it back")
            } else {
                format!("the search and the toggles let go, to show '{title}'")
            });
        }
        self.select(Some(key));
    }

    // ---- the form ----------------------------------------------------------

    /// Where a new thing would go: the selected row's folder, else here.
    fn folder_here(&self) -> PathBuf {
        self.selected_folder()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// `/new`: the run form, in the folder here.
    fn new_run_form(&mut self) {
        let form = Form::run(&self.folder_here())
            .with_folders(&self.names.paths())
            .with_agents(&self.agents);
        self.mode = Mode::Form(Box::new(form));
    }

    /// `enter` on the form: what it says, or why not — the form stays up then.
    async fn save_form(&mut self, f: Box<Form>, area: Rect) {
        let outcome = match form::run_draft(&f) {
            Ok(d) => self.start_run(d, area).await,
            Err(e) => Err(e),
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
            chain: None,
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

    // ---- asking ------------------------------------------------------------

    // ---- summarising -------------------------------------------------------

    // ---- keys --------------------------------------------------------------

    // ---- drawing -----------------------------------------------------------

    /// A frame: drawn with the catch-up built once for it, however many of
    /// its parts read it.
    fn draw(&mut self, f: &mut Frame) {
        *self.drawn.get_mut() = Some(Vec::new());
        self.draw_frame(f);
        *self.drawn.get_mut() = None;
    }

    fn draw_frame(&mut self, f: &mut Frame) {
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
        // The footer: the hints, or the keys for here, as many rows as they
        // wrap to.
        let foot = self.foot_lines(f.area().width);
        let a = areas(
            f.area(),
            self.show_pane,
            form_h,
            cands_h,
            self.line_h(),
            foot.len() as u16,
        );
        let rows = self.rows();
        // News is counted the way it is shown: a flow with unseen runs is one.
        let news = rows
            .iter()
            .filter(|r| match r {
                Row::Run(i) => i.unseen,
                Row::Flow(f) => f.unseen(),
                _ => false,
            })
            .count();
        let title = match self.view {
            View::Work => rows::headline(&rows, news, &self.filter),
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
        // The flows with no `in` have no header of their own to ride, so the
        // headline says them, amber when one waits for a yes. One of theirs
        // that did not load is said beside them, in red.
        if self.view == View::Work {
            let chips = rows::chips(&self.flows, None, Timestamp::now());
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
        // left, the toggles at the right of them, and who you are at the edge.
        // The toggles ride here rather than a row of their own; a subpage has
        // no list to filter, so they stand down while it is up.
        let lens = (self.view == View::Work && !self.show_pane).then(|| self.toggles_line());
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
        // The separators between the sections — a little air, a little structure.
        for sep in &a.seps {
            f.render_widget(Paragraph::new("─".repeat(sep.width as usize)).dim(), *sep);
        }
        // Clicks go by where the list was last drawn: with no list on screen,
        // nothing is there to hit.
        if a.list.is_none() || self.view != View::Work {
            *self.hit.borrow_mut() = ListHit::default();
        }
        if let Some(list) = a.list {
            match self.view {
                View::Work => self.draw_list(f, list, &rows),
                View::Events => self.draw_events(f, list),
                View::CatchUp => self.draw_catch_up(f, list),
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
        match self.mode {
            Mode::Normal => f.render_widget(Paragraph::new(self.line_lines(a.line.width)), a.line),
            // The line waits under a name being changed, quiet and with no
            // caret: the keys are the name's.
            Mode::Rename(_) => f.render_widget(Paragraph::new(format!("❯ {}", self.line.text())).dim(), a.line),
            // Removing a project: what is typed stands on the line with a
            // caret, so you see the word form.
            Mode::Confirm(ref c) => {
                let mut spans = vec![Span::raw("❯ ")];
                spans.extend(theme::with_caret(c.field.text(), c.field.cursor(), Style::default()));
                f.render_widget(Paragraph::new(Line::from(spans)), a.line);
            }
            _ => {}
        }
        let footer = Rect {
            x: a.footer.x + TEXT_INDENT,
            width: a.footer.width.saturating_sub(TEXT_INDENT),
            ..a.footer
        };
        f.render_widget(Paragraph::new(foot), footer);
    }

    /// The toggles, at the right of the header, compact: a glyph and its key
    /// for each kind — `✦a` agents, `⚑f` flows, `❯s` shells — then a `│` and
    /// `◌h` hidden, the kinds apart from the hidden axis. On is accented and
    /// bold, off is quiet. The letters flip them on an empty line.
    fn toggles_line(&self) -> Line<'static> {
        let on = theme::accent().add_modifier(Modifier::BOLD);
        let off = theme::dim();
        let mut spans = Vec::new();
        for (i, (glyph, key, lit)) in [
            ("✦", "a", self.show_agents),
            ("⚑", "f", self.show_flows),
            ("❯", "s", self.show_shells),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                spans.push(Span::styled("  ", off));
            }
            let style = if lit { on } else { off };
            spans.push(Span::styled(format!("{glyph}{key}"), style));
        }
        // Hidden is a different axis — what you hid, not a kind — so a divider
        // sets it apart.
        spans.push(Span::styled("  │  ", off));
        let style = if self.show_hidden { on } else { off };
        spans.push(Span::styled("◌h", style));
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
            let msg: String = if !self.filter.is_empty() {
                " nothing matches".into()
            } else if !self.show_flows || !self.show_agents || !self.show_shells {
                " nothing here · f flows · a agents · s shells · h hidden".into()
            } else if self.show_hidden {
                " nothing hidden · /hide takes a row off the list".into()
            } else {
                " nothing running · rl run -- <command>".into()
            };
            let nothing = self.filter.is_empty() && self.show_flows && self.show_agents && !self.show_hidden;
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
                        icon_span(rows::QUESTION, None, false),
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
                    let through = !self.show_hidden
                        && rows::hidden_since(r, &self.names, &self.flows, &self.digest.hidden).is_some();
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
                        icon_span(rows::icon(r), r.cli.as_deref(), r.state.is_terminal()),
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
                Row::Flow(f) => {
                    let title = Span::raw(fit(&f.name, tw));
                    let title = vec![if f.unseen() { title.bold() } else { title }];
                    // How the last few firings went, oldest first, then where
                    // the latest stands and what it came to; the tally and the
                    // cadence are the pane's. Before it has run, what starts it.
                    let flow = self.flow_of_row(f);
                    let stands = flows::stands(f, flow, now);
                    let last = f.last().map(|i| i.at).or(flow.and_then(|x| x.last_fired_at));
                    list_line(
                        w,
                        tw,
                        icon_span(rows::FLOW, None, false),
                        title,
                        flow_marks(f),
                        &stands,
                        vec![],
                        None,
                        last.map(|t| fmt_age(t, now)).unwrap_or_default(),
                    )
                }
                // An agent's own session: how it stands in its words, what it
                // waits on leading the summary — the column has room for a
                // word, not that — then where it lives.
                Row::Outside(c) => {
                    let state = c.run_state();
                    let waits = outside::waiting_for(c)
                        .map(|x| vec![Span::styled(format!("{x} · "), theme::needs())])
                        .unwrap_or_default();
                    list_line(
                        w,
                        tw,
                        icon_span(rows::outside_icon(c), Some(c.agent.as_str()), rows::outside_ended(c)),
                        vec![Span::raw(fit(&c.title(), tw))],
                        vec![Span::styled(
                            fit(&outside::word(c), WORD_W).trim_end().to_string(),
                            state_style(state),
                        )],
                        &outside::lives(c),
                        waits,
                        None,
                        c.started_at.map(|t| fmt_age(t, now)).unwrap_or_default(),
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
            // A flow opened with → has its latest firing's steps under it.
            if let Row::Flow(f) = row
                && self.open_flows.contains(&f.key)
                && let Some(x) = f.firing
            {
                lines.push(flows::steps_line(x, state_col(tw)));
                keys.push(row.key());
            }
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

    /// The catch-up: its top, a separator, then its projects and rows, a window
    /// of them following the selection.
    fn draw_catch_up(&self, f: &mut Frame, area: Rect) {
        self.list_h.set(area.height);
        *self.hit.borrow_mut() = ListHit::default();
        let (Some(cu), Some(whole)) = (self.catch_up(), self.catch_up_by("")) else {
            f.render_widget(Paragraph::new(" reading what happened…").dim(), area);
            return;
        };
        let w = area.width as usize;
        // The top is of all of it; the rows below follow the search.
        let mut top = match &self.top {
            Some(s) => catchup::written(s, w),
            None => catchup::top(&whole, w),
        };
        if self.topping.is_some() && self.top.is_none() {
            let words = "writing the top…";
            let pad = w.saturating_sub(words.chars().count() + 1);
            top.push(Line::styled(format!("{}{words}", " ".repeat(pad)), theme::dim()));
        }
        // What heads it comes first, cut so Relay's own top under it always shows.
        let firings = self.catch.as_ref().map_or(&[][..], |c| c.firings.as_slice());
        let leads = heads::leads(&self.flows, firings, cu.since);
        let leads = catchup::leads(&leads, &self.outcomes, w, Timestamp::now());
        let (block, block_keys) = catchup::head_block(leads, top, TOP, area.height as usize);
        let mut shown: Vec<String> = block_keys
            .iter()
            .filter(|k| heads::flow_of(k).is_some())
            .cloned()
            .collect();
        shown.dedup();
        *self.leads_shown.borrow_mut() = Some(shown);
        let block = catchup::lit(block, &block_keys, self.sel.as_deref(), w);
        let top_h = (block.len() as u16).min(area.height * 2 / 3);
        let [top_area, sep, rest] = Layout::vertical([
            Constraint::Length(top_h),
            Constraint::Length(u16::from(top_h > 0)),
            Constraint::Fill(1),
        ])
        .areas(area);
        f.render_widget(Paragraph::new(block), top_area);
        f.render_widget(Paragraph::new("─".repeat(sep.width as usize)).dim(), sep);
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

    /// The one row that names a subpage, whatever it is of: `‹ esc` to step
    /// back, then what it is the page of. A run's says its id and, once its
    /// output is being read, how it ended.
    fn subpage_head(&self) -> Line<'static> {
        let mut spans = vec![Span::styled("‹ esc  ", theme::dim())];
        let plain = |words: &str| Span::styled(words.to_string(), theme::name());
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            // Browse away and the page stays on its run, so it has to say which.
            let title = self
                .run_by_id(&t.id)
                .map(|r| r.title.clone())
                .unwrap_or_else(|| t.title.clone());
            spans.push(Span::styled(format!("{} · ", t.id), theme::dim()));
            spans.push(plain(&title));
            if let Some(code) = t.ended() {
                spans.push(Span::styled(format!(" · ended ({code})"), theme::dim()));
            }
        } else if matches!(self.mode, Mode::Draft(_)) {
            spans.push(plain("draft"));
        } else if self.tested.is_some() {
            spans.push(plain("flow test"));
        } else if self.reload_list.is_some() {
            spans.push(plain("not loaded"));
        } else if self.view == View::Events {
            spans.push(plain("event"));
        } else if self.lead_here().is_some() {
            spans.push(plain("firing"));
        } else if self.on_top() {
            spans.push(plain("while you were away"));
        } else {
            match self.selected() {
                Some(Row::Run(i)) => {
                    spans.push(Span::styled(format!("{} · ", i.run.id), theme::dim()));
                    spans.push(plain(&i.run.title));
                }
                Some(Row::Flow(f)) => spans.push(plain(&f.name)),
                Some(Row::Outside(c)) => spans.push(plain(&c.title())),
                Some(Row::Question(_)) => spans.push(plain("question")),
                Some(Row::Group(h)) if h.is_everywhere() => spans.push(plain(rows::EVERYWHERE)),
                Some(Row::Group(h)) => spans.push(plain(&self.names.workspace(&h.workspace))),
                None => {}
            }
        }
        Line::from(spans)
    }

    /// A subpage: its header on the first row, its page under it.
    fn draw_pane(&mut self, f: &mut Frame, area: Rect) {
        let (head, body) = subpage_rows(area);
        if let Some(head) = head {
            f.render_widget(Paragraph::new(self.subpage_head()), head);
        }
        self.draw_subpage_body(f, body);
    }

    fn draw_subpage_body(&mut self, f: &mut Frame, area: Rect) {
        // Filled in again only if a question is drawn, so a click never takes
        // a choice that is no longer on screen.
        *self.pane_hit.borrow_mut() = PaneHit::default();
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            let body = area;
            if t.empty() {
                f.render_widget(
                    Paragraph::new(Line::from(Span::raw(" nothing was recorded").dim())),
                    body,
                );
            } else {
                screen::draw(t.screen(), body, f.buffer_mut());
            }
            return;
        }
        let now = Timestamp::now();
        // A draft, until it is written or let go.
        if let Mode::Draft(d) = &self.mode {
            return d.draw(f, area);
        }
        // What `/flow test` found, until the next key.
        if let Some(lines) = &self.tested {
            let lines: Vec<Line> = lines.iter().map(|l| Line::raw(format!(" {l}"))).collect();
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        // What a reload just did not load, when there was more than the flash
        // could say: whose, which file and line, and why, until the next key.
        if let Some(list) = &self.reload_list {
            let list: Vec<&NotLoaded> = list.iter().collect();
            let lines = flows::not_loaded_lines(&list);
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
        if let Some(l) = self.lead_here() {
            let key = SummaryTarget::Firing {
                id: l.firing.id.clone(),
            }
            .key();
            let said = self.summary.as_ref().filter(|s| s.target == key);
            let lines = catchup::lead_pane(&l, self.outcomes.of(&l.firing.id), said, area.width as usize, now);
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        // The catch-up's top: what was said of everything, kept for the
        // stretch last asked of it here.
        if self.on_top() {
            let said = self.summary.as_ref().filter(|s| s.target == SummaryTarget::All.key());
            let lines = pane::top_lines(said, area.width as usize, now);
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
            Some(Row::Flow(f)) => pane::flow_lines(
                &f,
                self.flow_of_row(&f),
                &flow_links::wired(&self.flows, &self.digest.items),
                Some(&self.flow_pane).filter(|p| p.flow == f.name),
                matches!(self.mode, Mode::Walk),
                self.summary_of_row(&Row::Flow(f.clone())),
                &self.names,
                area.width as usize,
                now,
            ),
            Some(Row::Outside(c)) => outside::pane_lines(c, now),
            Some(Row::Question(q)) => {
                let asker = q.run.as_deref().and_then(|id| self.run_by_id(id));
                let at = matches!(&self.mode, Mode::Answer(id) if *id == q.id).then_some(self.answer_at);
                let said = self.summary_of_row(&Row::Question(q));
                let firing = rows::firing_of(q, &self.digest.firings);
                let p = pane::question_lines(q, asker, firing, said, &self.picked, at, area.width as usize, now);
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
                    .filter(|i| rows::everywhere_run(&i.run, &self.names, &self.flows))
                    .collect();
                let kept = self.summary_of_row(&Row::Group(h.clone()));
                pane::everywhere_lines(
                    &mine,
                    &self.flows,
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
                &self.flows,
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
    /// under way, else the keys for the row — with `^h shortcuts` at the end
    /// while `^h` would show them and there is room.
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
            Mode::Mark(id) => return format!("mark {}: d done · b blocked · w working · esc", title_of(id)),
            Mode::Walk => return "↑↓ steps, then firings · enter joins a step, or shows a firing · esc".into(),
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
            Mode::Draft(d) => {
                if let Some((m, at)) = &self.flash
                    && at.elapsed() < FLASH
                {
                    return m.clone();
                }
                return d.hint();
            }
            Mode::Rename(_) => {
                if let Some((m, at)) = &self.flash
                    && at.elapsed() < FLASH
                {
                    return m.clone();
                }
                return "enter saves · esc cancels".into();
            }
            Mode::Confirm(c) => {
                if let Some((m, at)) = &self.flash
                    && at.elapsed() < FLASH
                {
                    return m.clone();
                }
                return format!("{} · type DELETE then enter · esc keeps it", c.plan);
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
        let mut hints = self.hints();
        // As long as the carry takes, not as long as a flash lasts.
        if let Some(c) = &self.carrying {
            let carrying = format!("carrying {} on…", c.session.title());
            hints = if hints.is_empty() {
                carrying
            } else {
                format!("{carrying} · {hints}")
            };
        }
        if self.shortcuts || self.shortcuts_place().is_none() {
            return hints;
        }
        shortcuts::with_hint(hints, width)
    }

    /// The keys for what the cursor is on, in the footer.
    fn hints(&self) -> String {
        // The output page's own footer, while it is up.
        if self.pane == Pane::Term
            && let Some(t) = &self.term
        {
            let mut s = String::new();
            // A full-screen app keeps no lines above its screen: nothing to
            // read back, so the keys are not offered.
            if !t.screen().alternate_screen() {
                s.push_str("pgup/pgdn reads back · ");
            }
            s.push_str("esc back · ctrl-\\ list");
            if t.scrolled() > 0 {
                s.push_str(&format!(" · {} back", t.scrolled()));
            }
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
                Some(events::Target::Flow(_)) => "enter its flow on the list · ",
                None => "",
            };
            return format!("{go}^f kind: in: by: · {pane} · esc back");
        }
        let pane = if self.show_pane {
            "^o hides details"
        } else {
            "^o details"
        };
        if self.lead_here().is_some() {
            return format!("enter shows all of it · /summary tells this firing · {pane} · esc the list");
        }
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
            Some(Row::Flow(f)) => {
                let yes = match self.flow_of_row(&f).map(Flow::status) {
                    Some(FlowStatus::NeedsApproval) => "/flow approve · ",
                    _ => "/flow run · ",
                };
                let archives = if f.runs.is_empty() { "" } else { "^x archives · " };
                format!("enter its firings · {yes}/edit · {archives}{pane}")
            }
            Some(Row::Run(i)) if i.run.state.is_terminal() => {
                format!("enter output · /summary · ^x archives · ^t pins · {pane}")
            }
            Some(Row::Run(_)) => format!("enter joins it · ^x stops it · ^t pins · {pane}"),
            Some(Row::Outside(c)) => format!("{} · /hide · {pane}", outside::enter_hint(c)),
            // A flow here that did not load is what `/flow edit` opens, at the line.
            Some(Row::Group(h)) if h.is_everywhere() => {
                let fix = self.broken_hint(&h);
                format!("enter its stats · {fix}{pane}")
            }
            Some(Row::Group(h)) => {
                let fix = self.broken_hint(&h);
                format!("enter its stats · /shell · {fix}^r renames · ^x removes · {pane}")
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

/// A group header's right-edge tally: how many flows are `in` the folder, in
/// words; empty when there are none.
fn group_count(flows: &[rows::Chip]) -> String {
    match flows.len() {
        0 => String::new(),
        1 => "1 flow".into(),
        n => format!("{n} flows"),
    }
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
    use rl_proto::flow::FlowStatus;
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
            firings: Vec::new(),
            outside: vec![],
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
                Some("f:one".into()), // scrolled out of sight
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
        // A subpage wants every cell.
        assert_eq!(head_h(screen(160, 45), true), 1);
        // Too short, or too narrow, to spare the rows for the mark.
        assert_eq!(head_h(screen(160, 23), false), 1);
        assert_eq!(head_h(screen(80, 20), false), 1);
        assert_eq!(head_h(screen(79, 45), false), 1);
        // The list starts under the header and its separator, whichever it is.
        let list = |w, h| areas(screen(w, h), false, 0, 0, 1, 1).list.unwrap();
        assert_eq!(list(100, 30).y, 7);
        assert_eq!(list(80, 20).y, 2);
    }

    #[test]
    fn a_click_under_the_tall_header_finds_its_row() {
        let a = areas(Rect::new(0, 0, 100, 30), false, 0, 0, 1, 1);
        let list = a.list.unwrap();
        let hit = ListHit {
            area: list,
            scroll: 0,
            keys: vec![Some("g:one".into()), Some("r:two".into())],
        };
        assert_eq!(hit_row(&hit, 3, 7), Some("g:one".into()));
        assert_eq!(hit_row(&hit, 3, 8), Some("r:two".into()));
        // The header's rows and the separator under it are nobody's.
        for row in 0..7 {
            assert_eq!(hit_row(&hit, 3, row), None, "row {row}");
        }
    }

    #[test]
    fn a_terminal_in_a_subpage_is_sized_under_the_header_it_has() {
        // The whole width, and the height less the one-line panel header, the line,
        // the footer, and the row naming the run.
        assert_eq!(term_size(Rect::new(0, 0, 160, 45), 1, 1), (160, 41));
        assert_eq!(term_size(Rect::new(0, 0, 80, 20), 1, 1), (80, 16));
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
        // The events view's headline leads with its own word, and keeps it.
        let said = Said::new("events · 3".into(), vec![]);
        let [_, tally, rest] = said.lines(None);
        assert_eq!(text(&tally), "events · 3");
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
        // Too small for it, or a toggle or a search that came up empty: words.
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

    fn chip() -> Chip {
        Chip {
            name: "x".into(),
            status: FlowStatus::On,
            due: None,
        }
    }

    #[test]
    fn the_header_count_says_its_flows_and_pluralises() {
        assert_eq!(group_count(&[]), "");
        assert_eq!(group_count(&[chip()]), "1 flow");
        assert_eq!(group_count(&[chip(), chip(), chip()]), "3 flows");
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
        let head = |place: &str, own: Option<&str>, checkout, flows| Head {
            place: place.into(),
            workspace: "/w".into(),
            name: "w".into(),
            own: own.map(Into::into),
            flows,
            checkout,
            attention: 0,
        };
        let a = header_line(
            &head("~/Projects/flint-platform", None, Some(co("main", 24)), vec![]),
            w,
            tw,
            false,
        );
        let b = header_line(
            &head("~/tmp/foodai", Some("food"), Some(co("a-long-branch", 4)), vec![chip()]),
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
        assert!(b.to_string().ends_with("1 flow"));

        // A folder whose flow did not load says so in red; the flow count
        // (every flow defined there, waiting-for-a-yes ones among them) still
        // ends at the edge — the waiting flows are rows now, not header chips.
        let waiting = Chip {
            name: "deploy".into(),
            status: FlowStatus::NeedsApproval,
            ..chip()
        };
        let flows = vec![waiting, chip()];
        let broken = header_line(
            &head("~/tmp/foodai", None, Some(co("main", 0)), flows.clone()),
            w,
            tw,
            true,
        );
        assert!(broken.to_string().ends_with("! flows  2 flows"), "{broken}");
        assert_eq!(broken.width(), w);
        let red = broken.spans.iter().find(|s| s.content == "! flows").unwrap();
        assert_eq!(red.style, crate::tui::theme::bad());
        let fine = header_line(&head("~/tmp/foodai", None, Some(co("main", 0)), flows), w, tw, false);
        assert!(!fine.to_string().contains("! flows"), "{fine}");
        // Nothing else on it: the chip alone, at the edge.
        let alone = header_line(&head("~/tmp/foodai", None, None, vec![]), w, tw, true);
        assert!(alone.to_string().ends_with("! flows"), "{alone}");
        assert_eq!(alone.width(), w);

        // Something that needs you is off the list: the header hints in amber,
        // before the flow count, and the line still ends at the edge.
        let mut hinted = head("~/tmp/foodai", None, Some(co("main", 0)), vec![chip()]);
        hinted.attention = 2;
        let line = header_line(&hinted, w, tw, false);
        assert!(line.to_string().ends_with("! 2 need you  1 flow"), "{line}");
        assert_eq!(line.width(), w);
        let amber = line.spans.iter().find(|s| s.content == "! 2 need you").unwrap();
        assert_eq!(amber.style, crate::tui::theme::needs());

        // A run's state word starts where a header's checkout does, and every
        // title in the same column, whatever icon it wears.
        let row = |icon: &str, word: &str| {
            list_line(
                w,
                tw,
                icon_span(icon, None, false),
                vec![Span::raw(crate::tui::rows::fit("title", tw))],
                vec![Span::raw(word.to_string())],
                "summary",
                vec![],
                None,
                "2m".into(),
            )
        };
        for (icon, word) in [(rows::AGENT, "needs you"), (rows::SHELL, ""), (rows::FLOW, "·×··")] {
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
    pub(super) async fn panel() -> (
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
        /// More of `config.yaml`, after `assistants: {summary: fakeai, interpret: fakeai}`.
        config: &'static str,
        /// `assistants.summary` left out: the first agent here would answer.
        unnamed: bool,
        /// What `fakeai` answers; None, the fixture's own draft.
        answer: Option<&'static str>,
        /// `rl-session`, so an assistant's run really runs; without it the
        /// daemon cannot start one.
        session: Option<std::path::PathBuf>,
        /// More profiles for `detect/`, by name, over the built-ins.
        profiles: Vec<(&'static str, String)>,
    }

    /// A panel whose `rld` runs what it starts, with `profiles` over the
    /// built-ins (an agent's `bins` pointed at a stand-in, say). None (and
    /// the test says it is skipping) when `rl-session` is not built.
    pub(super) async fn panel_with_profiles(
        profiles: Vec<(&'static str, String)>,
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
            session: Some(session),
            profiles,
            ..Default::default()
        };
        Some(panel_set(setup, |_, _| vec![]).await)
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

    /// `panel()` with more of the config: `fakeai` is the summariser, run by
    /// whatever `rl-session` the daemon finds (none, as a rule, so it fails
    /// to start) — what is asserted is what was asked, not the answer.
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
        for (name, text) in &set.profiles {
            std::fs::write(home.detect_dir().join(format!("{name}.yaml")), text).unwrap();
        }
        std::fs::write(
            home.config_file(),
            // The interpreter is always `fakeai`: a real `claude` on the PATH is never picked.
            if set.unnamed {
                format!("assistants:\n  interpret: fakeai\n{}", set.config)
            } else {
                format!("assistants:\n  summary: fakeai\n  interpret: fakeai\n{}", set.config)
            },
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
                        // Each happened when it started: now, unless `more`
                        // gave it another time.
                        let at = run.started_at;
                        // The list shows what has news since it last looked.
                        events::append(c, &NewEvent::for_run(run, at, event_kind::RUN_STARTED, "started"))?;
                        // Each said something, so a deep read of it asks.
                        let said = format!("{} said this\r\n", run.title);
                        session_io::append(c, &run.id, session_io::Direction::Out, said.as_bytes(), at)?;
                        runs::finish(c, &run.id, RunState::Finished, Source::Pty, Some(0), at)?;
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

    #[tokio::test]
    async fn a_project_moves_up_among_the_projects_and_stays_there() {
        let (tmp, token, mut app, _folder) = panel().await;
        let zed = std::fs::canonicalize(tmp.path()).unwrap().join("zed");
        std::fs::create_dir_all(&zed).unwrap();
        app.rpc
            .call::<super::WorkspaceUp>(super::WorkspaceUpParams { path: zed.clone() })
            .await
            .unwrap();
        app.refresh().await;
        let heads = |app: &super::App| -> Vec<String> {
            app.rows()
                .iter()
                .filter_map(|r| match r {
                    super::Row::Group(h) => Some(h.name.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(heads(&app), ["api", "zed"], "by name until you move one");
        let key = format!("g:{}", zed.display());
        app.select(Some(key.clone()));
        press(&mut app, KeyCode::Up, KeyModifiers::SHIFT).await;
        assert_eq!(heads(&app), ["zed", "api"]);
        assert_eq!(app.sel.as_deref(), Some(key.as_str()), "the cursor goes with it");
        // The daemon keeps it: fetched again, it stands where you put it.
        app.refresh().await;
        assert_eq!(heads(&app), ["zed", "api"]);
        press(&mut app, KeyCode::Up, KeyModifiers::SHIFT).await;
        let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert_eq!(said, "already the first project");
        token.cancel();
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

    pub(super) async fn type_in(app: &mut super::App, text: &str) {
        for c in text.chars() {
            app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), AREA)
                .await
                .unwrap();
        }
    }

    pub(super) async fn press(app: &mut super::App, code: KeyCode, m: KeyModifiers) {
        app.key(KeyEvent::new(code, m), AREA).await.unwrap();
    }

    pub(super) const AREA: Rect = Rect::new(0, 0, 120, 40);
    const RUN: &str = "r:run00001";

    fn renaming(app: &super::App) -> Option<(String, bool)> {
        match &app.mode {
            super::Mode::Rename(r) => Some((r.field.text().to_string(), r.field.is_selected())),
            _ => None,
        }
    }

    /// The project name and the word typed so far, when a delete confirm is up.
    fn confirming(app: &super::App) -> Option<(String, String)> {
        match &app.mode {
            super::Mode::Confirm(c) => Some((c.name.clone(), c.field.text().to_string())),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_project_is_removed_only_by_typing_delete() {
        let (_tmp, token, mut app, folder) = panel().await;
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        app.select(Some(format!("g:{}", folder.display())));
        // ^x on a project opens a typed confirm, not an armed double-press.
        press(&mut app, KeyCode::Char('x'), ctrl).await;
        assert_eq!(confirming(&app), Some(("api".into(), String::new())));
        assert!(app.removing.is_none(), "nothing removed yet");
        assert!(app.arm.is_none(), "no armed removal to slip on");
        // A stray second ^x is a key the prompt ignores: the project stands.
        press(&mut app, KeyCode::Char('x'), ctrl).await;
        assert!(app.removing.is_none(), "a double ^x cannot take a project down");
        assert!(confirming(&app).is_some(), "still asking");
        // The wrong word, even on enter, does not remove it.
        type_in(&mut app, "delete").await;
        assert_eq!(confirming(&app).map(|(_, t)| t), Some("delete".into()));
        press(&mut app, KeyCode::Enter, none).await;
        assert!(app.removing.is_none(), "only the exact word removes it");
        assert!(confirming(&app).is_some(), "it keeps asking");
        // esc keeps the project.
        press(&mut app, KeyCode::Esc, none).await;
        assert!(confirming(&app).is_none());
        assert!(app.removing.is_none());
        token.cancel();
    }

    #[tokio::test]
    async fn typing_delete_removes_the_project() {
        let (_tmp, token, mut app, folder) = panel().await;
        app.select(Some(format!("g:{}", folder.display())));
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
        type_in(&mut app, "DELETE").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.removing.is_some(), "the exact word starts the removal");
        assert!(matches!(app.mode, super::Mode::Normal), "the prompt is done");
        token.cancel();
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

    /// The screen as rows of text, drawn at this size.
    pub(super) fn screen(app: &mut super::App, w: u16, h: u16) -> Vec<String> {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    /// Where the live line is on the screen: its row.
    fn line_row(rows: &[String]) -> usize {
        rows.iter().position(|r| r.starts_with('❯')).unwrap()
    }

    #[tokio::test]
    async fn ctrl_h_swaps_the_footers_hints_for_the_shortcuts_and_esc_swaps_them_back() {
        let (_tmp, token, mut app, _folder) = panel().await;
        let (none, ctrl) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        app.select(Some(RUN.into()));

        // Closed: the few hints, ending in the pointer, in the one last row.
        let closed = screen(&mut app, 120, 40);
        let last = closed.last().unwrap();
        assert!(last.trim_end().ends_with("^h shortcuts"), "{last:?}");
        assert!(!last.contains("? shortcuts"), "{last:?}");
        assert!(!closed.iter().any(|r| r.contains("↑↓ ^j^k move")), "{closed:#?}");

        // `^h`: the keys are the footer, and the line sits just above its
        // separator, the footer's own rows under it.
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        assert!(app.shortcuts);
        let open = screen(&mut app, 120, 40);
        let at = line_row(&open);
        assert!(open[at + 1].starts_with('─'), "{:?}", open[at + 1]);
        assert!(open[at + 2].contains("↑↓ ^j^k move"), "{:?}", open[at + 2]);
        assert!(open[at + 2].starts_with("  "), "indented as the footer is");
        assert!(
            !open.last().unwrap().contains("shortcuts"),
            "no pointer while they are up"
        );
        assert_eq!(open.len() - (at + 2), app.foot_h(120) as usize);

        // `esc` puts them away first, and the hints are back.
        press(&mut app, KeyCode::Esc, none).await;
        assert!(!app.shortcuts);
        assert_eq!(screen(&mut app, 120, 40), closed);

        // `^h` again, too.
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        assert!(!app.shortcuts);
        token.cancel();
    }

    #[tokio::test]
    async fn the_footer_grows_on_a_narrow_screen_and_the_list_gives_up_the_rows() {
        let (_tmp, token, mut app, _folder) = panel().await;
        let ctrl = KeyModifiers::CONTROL;
        app.select(Some(RUN.into()));
        for (w, h) in [(160, 45), (80, 24)] {
            let area = Rect::new(0, 0, w, h);
            let closed = screen(&mut app, w, h);
            assert_eq!(app.foot_h(w), 1);
            let size = app.pane_size(area);
            press(&mut app, KeyCode::Char('h'), ctrl).await;
            // As many rows as the keys wrap to in the room the footer has,
            // each with its keys whole.
            let keys = super::shortcuts::keys(super::shortcuts::Place::List);
            let rows = super::shortcuts::wrap(&keys, w as usize - super::TEXT_INDENT as usize);
            assert_eq!(app.foot_h(w) as usize, rows.len(), "at {w}");
            let open = screen(&mut app, w, h);
            let foot = &open[open.len() - rows.len()..];
            for (row, want) in foot.iter().zip(&rows) {
                for k in want {
                    assert!(row.contains(k.keys.as_str()), "{row:?} lacks {}", k.keys);
                }
                assert!(row.chars().count() <= w as usize);
            }
            // The line is the same distance above the bottom, less the rows
            // the footer took over, and the list lost them.
            let extra = rows.len() - 1;
            assert_eq!(line_row(&open), line_row(&closed) - extra, "at {w}");
            // A watched terminal is sized by what is left of the pane.
            let size_open = app.pane_size(area);
            assert_eq!(size_open.0, size.0);
            assert_eq!(size.1 - size_open.1, extra as u16, "at {w}");
            press(&mut app, KeyCode::Char('h'), ctrl).await;
        }
        // The width the shortcuts wrap at is the footer's, at 80: more than one row.
        assert!(app.foot_h(80) == 1);
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        let rows = app.foot_h(80);
        assert!(rows > 1);
        // A flash is said above the keys, which stay where they were.
        app.say("daemon: gone");
        assert_eq!(app.foot_h(80), rows + 1);
        let said = screen(&mut app, 80, 24);
        assert!(said[24 - rows as usize - 1].contains("daemon: gone"), "{said:#?}");
        assert!(said[24 - rows as usize].contains("↑↓ ^j^k move"), "{said:#?}");
        token.cancel();
    }

    #[tokio::test]
    async fn a_question_mark_is_the_shortcuts_only_on_an_empty_line_and_typing_leaves_them_up() {
        let (_tmp, token, mut app, _folder) = panel().await;
        let (none, ctrl) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Char('?'), none).await;
        assert!(app.shortcuts && app.line.is_empty());
        press(&mut app, KeyCode::Char('?'), none).await;
        assert!(!app.shortcuts);

        // With words on the line `?` is a character, and `^h` still shows them.
        type_in(&mut app, "why").await;
        press(&mut app, KeyCode::Char('?'), none).await;
        assert_eq!(app.line.text(), "why?");
        assert!(!app.shortcuts);
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        assert!(app.shortcuts);
        assert_eq!(app.line.text(), "why?", "^h is no character");
        // Typing and moving about the words do not put them away; `esc` does,
        // before it clears the line.
        type_in(&mut app, "!").await;
        press(&mut app, KeyCode::Left, none).await;
        assert!(app.shortcuts);
        press(&mut app, KeyCode::Esc, none).await;
        assert!(!app.shortcuts);
        assert_eq!(app.line.text(), "why?!");
        press(&mut app, KeyCode::Esc, none).await;
        assert!(app.line.is_empty());
        token.cancel();
    }

    #[tokio::test]
    async fn a_name_being_changed_keeps_its_keys_and_takes_the_shortcuts_beside_them() {
        let (_tmp, token, mut app, _folder) = panel().await;
        let (none, ctrl) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "new").await;
        press(&mut app, KeyCode::Char('h'), ctrl).await;
        assert!(app.shortcuts);
        assert_eq!(renaming(&app), Some(("new".into(), false)), "the name is as it was");
        let rows = screen(&mut app, 120, 40);
        assert!(
            rows.iter().any(|r| r.contains("enter saves · esc cancels · ^a ^e")),
            "{rows:#?}"
        );
        // `esc` puts the keys away first; the next one cancels the name.
        press(&mut app, KeyCode::Esc, none).await;
        assert!(!app.shortcuts);
        assert!(renaming(&app).is_some());
        press(&mut app, KeyCode::Esc, none).await;
        assert!(renaming(&app).is_none());
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

        // `tab` lets the name go, back to the list.
        app.select(Some(format!("g:{}", folder.display())));
        press(&mut app, KeyCode::Char('r'), ctrl).await;
        type_in(&mut app, "x").await;
        press(&mut app, KeyCode::Tab, none).await;
        assert!(renaming(&app).is_none());
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
            let step = |id: &str, name: &str, at: &std::path::Path| {
                let mut r = Run::new(id, RunKind::Command, name, vec!["true".into()], at, at, now);
                r.cause = Cause::Flow {
                    flow: name.into(),
                    step: "probe".into(),
                };
                r
            };
            let elsewhere = folder.parent().unwrap().join("elsewhere");
            vec![
                step("run00002", "tidy", folder),
                step("run00003", "nightly", &elsewhere),
            ]
        })
        .await;
        app.select(Some(RUN.into()));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Run { id: "run00001".into() }));
        let flow_key = |app: &super::App, name: &str| {
            app.rows().iter().find_map(|r| match r {
                super::Row::Flow(f) if f.name == name => Some(f.key.clone()),
                _ => None,
            })
        };
        // A flow's row's summary is of the flow of that name.
        app.select(flow_key(&app, "tidy"));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Flow { name: "tidy".into() }));
        app.select(flow_key(&app, "nightly"));
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Flow { name: "nightly".into() })
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
    async fn a_flows_summary_starts_at_the_run_you_had_not_seen() {
        use rl_proto::types::{Cause, Run, RunKind};
        let (_tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let firing = |id: &str, hours| {
                let at = now - jiff::SignedDuration::from_hours(hours);
                let mut r = Run::new(id, RunKind::Command, "tidy", vec!["true".into()], folder, folder, at);
                r.cause = Cause::Flow {
                    flow: "tidy".into(),
                    step: "probe".into(),
                };
                r
            };
            vec![firing("run00002", 2), firing("run00003", 1)]
        })
        .await;
        let oldest = app.run_by_id("run00002").unwrap().started_at;
        let key = app.rows().iter().find_map(|r| match r {
            super::Row::Flow(f) if f.name == "tidy" => Some(f.key.clone()),
            _ => None,
        });
        // The cursor lands on it: that is looking, and its firings are seen.
        app.select(key);
        app.moved().await;
        assert!(app.selected_flow().is_some_and(|f| !f.unseen()));
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let s = app.summarising.as_ref().expect("asked");
        assert!(matches!(s.target, SummaryTarget::Flow { .. }), "{:?}", s.target);
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
            chain: "f".into(),
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
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
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
            chain: id.into(),
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
    pub(super) fn draw_text(app: &mut super::App) -> String {
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

    /// A panel of two runs with the first selected and its ended output up as a
    /// subpage, `lines` long.
    async fn panel_on_output(lines: usize) -> (tempfile::TempDir, tokio_util::sync::CancellationToken, super::App) {
        let (tmp, token, mut app, _folder) = panel_with_runs(2).await;
        app.select(Some(RUN.into()));
        let text: String = (1..=lines).map(|n| format!("line {n}\n")).collect();
        app.term = Some(super::Output::ended_with("run00001", "make test", &text, 3, (118, 36)));
        app.pane = super::Pane::Term;
        app.show_pane = true;
        (tmp, token, app)
    }

    #[tokio::test]
    async fn a_subpage_takes_the_whole_body_and_leaves_no_list_beside_it() {
        let (_tmp, token, mut app) = panel_on_output(3).await;
        let screen = draw_text(&mut app);
        assert!(screen.contains("‹ esc  run00001 · make test · ended (3)"), "{screen}");
        assert!(screen.contains("line 3"), "{screen}");
        assert!(!screen.contains('│'), "no divider, no second column: {screen}");
        assert!(!screen.contains("echo 2"), "the other run's row is not drawn: {screen}");
        token.cancel();
    }

    /// A run that ends while you are joined to it: you come back to the list,
    /// not to a page of a dead join, and the run is the row you are on.
    #[tokio::test]
    async fn coming_back_from_a_join_lands_on_the_list() {
        let (_tmp, token, mut app) = panel_on_output(3).await;
        assert!(app.show_pane, "a page is up");
        app.back_from_join(
            "run00001",
            crate::attach::Ended::Exit {
                code: 0,
                how: "exit 0".into(),
            },
        );
        assert!(!app.show_pane, "the list is back");
        assert!(app.term.is_none(), "and the page it was reading is let go");
        assert_eq!(app.sel.as_deref(), Some(RUN), "the run is the row");
        assert!(draw_text(&mut app).contains("ended (exit 0)"), "a word on how it went");
        token.cancel();
    }

    #[tokio::test]
    async fn a_subpage_draws_at_a_sliver_of_a_screen_without_overflowing() {
        let (_tmp, token, mut app) = panel_on_output(30).await;
        // The header is dropped where there is no row to spare for it, and nothing panics.
        for (w, h) in [(120, 1), (20, 1), (20, 2), (20, 10), (1, 1)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(|f| app.draw(f)).unwrap();
        }
        let (head, body) = super::subpage_rows(Rect::new(3, 4, 20, 1));
        assert_eq!((head, body), (None, Rect::new(3, 4, 20, 1)));
        let (head, body) = super::subpage_rows(Rect::new(3, 4, 20, 5));
        assert_eq!((head, body), (Some(Rect::new(3, 4, 20, 1)), Rect::new(3, 5, 20, 4)));
        // A long head is cut at the screen's edge, not carried past it.
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 10)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        assert!(buf_has(t.backend().buffer(), "‹ esc  run00001"));
        token.cancel();
    }

    #[tokio::test]
    async fn keys_with_a_subpage_up_do_not_move_the_list_and_scroll_the_output() {
        let (_tmp, token, mut app) = panel_on_output(100).await;
        let none = KeyModifiers::NONE;
        let before = app.sel.clone();
        for code in [KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Down, KeyCode::Up] {
            press(&mut app, code, none).await;
            assert_eq!(app.sel, before, "{code:?} left the list where it was");
        }
        press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL).await;
        assert_eq!(app.sel, before, "^j too");
        assert!(app.show_pane, "and the subpage is still up");
        press(&mut app, KeyCode::PageUp, none).await;
        let back = app.term.as_ref().unwrap().scrolled();
        assert!(back > 0, "pgup reads back");
        press(&mut app, KeyCode::PageDown, none).await;
        assert!(app.term.as_ref().unwrap().scrolled() < back, "pgdn reads forward");
        assert_eq!(app.sel, before);
        // The way out still works.
        press(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL).await;
        assert!(!app.show_pane, "ctrl-backslash leaves");
        token.cancel();
    }

    #[test]
    fn presence_is_touched_once_a_minute() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert!(super::presence_due(None, now), "never touched by this panel");
        assert!(!super::presence_due(Some(now - Duration::from_secs(59)), now));
        assert!(super::presence_due(Some(now - Duration::from_secs(60)), now));
    }

    #[tokio::test]
    async fn the_tick_touches_presence_while_the_panel_is_open() {
        use rl_proto::methods::{PresenceClient, PresenceTouch};
        let (_tmp, token, mut app, _folder) = panel().await;
        // The ledger keeps milliseconds.
        let before = jiff::Timestamp::from_millisecond(jiff::Timestamp::now().as_millisecond()).unwrap();
        // Whatever the last touch, a minute on the tick touches again: a
        // window closed without quitting still says when you were here.
        app.presence_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(61));
        app.tick().await;
        let me = || PresenceClient { client: "test".into() };
        let ticked = app.rpc.call::<PresenceTouch>(me()).await.unwrap().last_seen_at;
        assert!(ticked.is_some_and(|t| t >= before), "{ticked:?}");
        // Within the minute it is left alone: the last touch stays the one
        // just made here.
        let looked = jiff::Timestamp::now();
        app.tick().await;
        let after = app.rpc.call::<PresenceTouch>(me()).await.unwrap().last_seen_at;
        assert!(after.is_some_and(|t| t <= looked), "{after:?}");
        token.cancel();
    }

    #[tokio::test]
    async fn the_catch_up_shows_and_esc_goes_to_the_list() {
        use jiff::{SignedDuration, Timestamp};
        let (_tmp, token, mut app, _folder) = panel().await;
        assert!(app.digest.items.iter().any(|i| i.unseen), "news before it is looked at");
        let since = Timestamp::now() - SignedDuration::from_hours(9);
        app.open_catch_up(since, true).await;
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
        app.open_catch_up(Timestamp::now() - SignedDuration::from_hours(9), true)
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

    #[tokio::test]
    async fn the_catch_up_is_cut_from_the_lists_digest_not_asked_for_again() {
        use rl_proto::methods::{DigestGet, DigestParams};
        let (_tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let at = now - jiff::SignedDuration::from_hours(48);
            vec![rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "old",
                vec!["true".into()],
                folder,
                folder,
                at,
            )]
        })
        .await;
        // Looked at long ago: not in a stretch of the last nine hours.
        app.seen(vec!["run00002".into()]).await;
        app.refresh().await;
        let since = nine_hours_ago();
        app.open_catch_up(since, true).await;
        app.refresh().await;
        let own = app
            .rpc
            .call::<DigestGet>(DigestParams {
                since: Some(since),
                ..Default::default()
            })
            .await
            .unwrap();
        let ids = |d: &rl_proto::methods::Digest| -> Vec<String> { d.items.iter().map(|i| i.run.id.clone()).collect() };
        let catch = app.catch.as_ref().unwrap();
        assert_eq!(ids(catch), ids(&own), "what a digest of the stretch has");
        assert_eq!(catch.since, since);
        // Cut from the list's own, not read again: what the list has is
        // what it shows.
        for i in app.digest.items.iter_mut() {
            i.summary = "as the list has it".into();
        }
        app.open_catch_up(since, false).await;
        let catch = app.catch.as_ref().unwrap();
        assert!(
            catch.items.iter().all(|i| i.summary == "as the list has it"),
            "{:?}",
            catch.items.iter().map(|i| &i.summary).collect::<Vec<_>>()
        );
        token.cancel();
    }

    #[tokio::test]
    async fn leaving_a_stretch_asked_for_gives_a_projects_summary_its_own_back() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        let left = nine_hours_ago();
        (app.left, app.away_since) = (Some(left), Some(left));
        type_in(&mut app, "/since 2d").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp);
        assert_ne!(
            app.away_since,
            Some(left),
            "the stretch up is the one a project's /summary takes"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        assert_eq!(app.away_since, Some(left), "back to since you were last here");
        select_header(&mut app);
        type_in(&mut app, "/summary").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(app.summarising.as_ref().expect("asked").from, Some(left));
        token.cancel();
    }

    #[tokio::test]
    async fn the_catch_up_leaves_the_lists_own_doings_to_the_list() {
        let (_tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "nightly",
                vec!["true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        app.open_catch_up(nine_hours_ago(), true).await;
        let said = |app: &super::App| app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        // What reads the list's rows says where it is done.
        app.select(Some(RUN.into()));
        type_in(&mut app, "/hide").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(said(&app), "/hide hides on the list · esc goes there");
        assert!(app.digest.items.iter().all(|i| i.run.hidden_at.is_none()));
        type_in(&mut app, "/sort").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(said(&app), "/sort sorts on the list · esc goes there");
        // A flow here holds only the stretch's runs: archiving them is the
        // list's, where it holds them all.
        let flow = app
            .catch_rows()
            .iter()
            .find_map(|r| match r {
                super::Row::Flow(f) => Some(f.key.clone()),
                _ => None,
            })
            .expect("the nightly flow");
        app.select(Some(flow));
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
        assert_eq!(said(&app), "^x archives a flow's runs on the list · esc goes there");
        assert!(app.arm.is_none());
        type_in(&mut app, "/archive").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(said(&app), "^x archives a flow's runs on the list · esc goes there");
        assert!(app.digest.items.iter().any(|i| i.run.id == "run00002"), "not archived");
        // What acts on the row itself acts here: ^t pins the run under the
        // cursor.
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).await;
        assert!(app.view == super::View::CatchUp);
        assert!(app.run_by_id("run00001").is_some_and(|r| r.pinned_at.is_some()));
        token.cancel();
    }

    #[tokio::test]
    async fn a_draw_builds_the_catch_up_once() {
        let built = || crate::catchup::BUILT.with(std::cell::Cell::get);
        let (_tmp, token, mut app, _folder) = panel_with_runs(2).await;
        app.open_catch_up(nine_hours_ago(), true).await;
        app.select(Some(RUN.into()));
        app.toggle_pane();
        let before = built();
        let frame = draw_text(&mut app);
        assert!(frame.contains("make test"), "{frame}");
        assert_eq!(
            built() - before,
            1,
            "the top, the rows, the pane and the footer share one"
        );
        // Narrowed by a search, the top is still told from all of it: one
        // of each, on the list.
        app.leave_subpage();
        app.filter = "echo".into();
        let before = built();
        draw_text(&mut app);
        assert_eq!(built() - before, 2);
        // Kept for the draw only: what moves after it is built anew.
        let before = built();
        app.keys();
        assert_eq!(built() - before, 1);
        token.cancel();
    }

    #[tokio::test]
    async fn enter_on_a_run_the_list_lets_go_of_reads_it_in_the_catch_up() {
        use jiff::{SignedDuration, Timestamp};
        // `old` ended two days ago and is not seen yet: the list has it only
        // until it is.
        let (_tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let at = now - SignedDuration::from_hours(48);
            vec![rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "old",
                vec!["sh".into(), "-c".into(), "true".into()],
                folder,
                folder,
                at,
            )]
        })
        .await;
        app.open_catch_up(Timestamp::now() - SignedDuration::from_hours(72), true)
            .await;
        app.select(Some("r:run00002".into()));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp, "the list would not have it");
        assert_eq!(app.sel.as_deref(), Some("r:run00002"));
        assert!(app.show_pane && app.pane == super::Pane::Facts, "read in the pane");
        assert!(unseen(&app, "run00002"), "seen only once the catch-up is left");
        assert!(draw_text(&mut app).contains("old"));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(
            !app.show_pane && app.view == super::View::CatchUp,
            "esc steps back to the catch-up"
        );
        // A recent one goes to the list, the cursor on it.
        app.select(Some(RUN.into()));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        assert_eq!(app.sel.as_deref(), Some(RUN));
        assert!(!unseen(&app, "run00002"), "left, so seen");
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
            chain: "f".into(),
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

    pub(super) fn nine_hours_ago() -> jiff::Timestamp {
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
        app.open_catch_up(nine_hours_ago(), true).await;
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
    async fn landing_on_a_question_in_the_catch_up_opens_nothing_until_enter() {
        let (tmp, token, mut app, folder) = panel().await;
        ask_in(&tmp, &folder);
        app.refresh().await;
        app.open_catch_up(nine_hours_ago(), true).await;
        assert!(!app.show_pane, "on the project's line");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some("q:dec00001"));
        assert!(!app.show_pane, "selecting a question opens nothing");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(
            app.view == super::View::Work && app.sel.as_deref() == Some("q:dec00001"),
            "enter in the catch-up goes to the row on the list, as for any row"
        );
        token.cancel();
    }

    /// `enter` on a line a flow set off, in the events view: its row on the
    /// list; the search and toggles give way when they hide it; and with no
    /// row at all the view stays and says so.
    #[tokio::test]
    async fn a_flow_in_the_events_view_is_found_on_the_list_or_said_to_have_no_row() {
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
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        let flow = |name: &str, approved: bool| rl_proto::flow::Flow {
            name: name.into(),
            hash: "h".into(),
            def: serde_json::json!({ "in_": folder, "trigger": { "when": { "Every": 3600 }, "on": [] } }),
            source: format!("/h/flows/{name}").into(),
            folder: Some(folder.clone()),
            paused: false,
            approved_hash: approved.then(|| "h".to_string()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        };
        // A refresh reloads the flows from the daemon, which has none here.
        let load = |app: &mut super::App| {
            app.flows = vec![flow("nightly", true), {
                // Loaded but removed: it is still a flow, so it has no row on
                // the list — every flow not removed does.
                let mut unseen = flow("ghost", false);
                unseen.def = serde_json::json!({ "trigger": { "when": { "Every": 3600 }, "on": [] } });
                unseen.folder = None;
                unseen.removed_at = Some("2020-01-01T00:00:00Z".parse().unwrap());
                unseen
            }];
        };

        let said = |app: &super::App| app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        app.toggle_events().await;
        load(&mut app);
        assert!(app.view == super::View::Events);
        app.show_flow("nightly").await;
        assert!(app.view == super::View::Work);
        assert_eq!(app.sel.as_deref(), Some("f:nightly"));

        // A search that hides the row gives way, and says so.
        app.filter = "zzz-nothing".into();
        app.toggle_events().await;
        load(&mut app);
        app.show_flow("nightly").await;
        assert!(app.view == super::View::Work);
        assert!(app.filter.is_empty());
        assert_eq!(app.sel.as_deref(), Some("f:nightly"));
        assert!(said(&app).contains("let go"), "{}", said(&app));

        // With no row at all, the events view is still up.
        app.toggle_events().await;
        load(&mut app);
        app.show_flow("ghost").await;
        assert!(app.view == super::View::Events, "the view stays");
        assert_eq!(said(&app), "ghost has no row on the list");
        token.cancel();
    }

    #[tokio::test]
    async fn enter_on_a_flow_in_the_catch_up_goes_to_its_row_on_the_list() {
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
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        // The flow is in the project: its row is there, the list is still
        // where enter goes.
        app.flows.push(rl_proto::flow::Flow {
            name: "nightly".into(),
            hash: "h".into(),
            def: serde_json::json!({ "in_": folder, "trigger": { "when": { "Every": 3600 }, "on": [] } }),
            source: "/h/flows/nightly".into(),
            folder: Some(folder.clone()),
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        });
        app.open_catch_up(nine_hours_ago(), true).await;
        let flow = app
            .catch_rows()
            .iter()
            .find_map(|r| match r {
                super::Row::Flow(f) => Some(f.key.clone()),
                _ => None,
            })
            .expect("the nightly flow in the catch-up");
        let cu = app.catch_up().unwrap();
        let mine = cu.projects.iter().find(|p| p.head.workspace == folder).unwrap();
        assert!(
            mine.rows.iter().any(|r| matches!(r, super::Row::Flow(_))),
            "under its project"
        );
        drop(cu);
        app.select(Some(flow.clone()));
        assert!(app.hints().starts_with("enter it on the list"), "{}", app.hints());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "the list has flow rows of its own");
        assert_eq!(app.sel.as_deref(), Some(flow.as_str()));
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
        app.open_catch_up(nine_hours_ago(), true).await;
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
        app.open_catch_up(nine_hours_ago(), true).await;
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
        app.open_catch_up(nine_hours_ago(), true).await;
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

    /// Hands a `/summary`, once it is made, to the panel as the main loop
    /// would.
    async fn wait_summarised(app: &mut super::App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while let Some(s) = app.summarising.as_mut() {
            if s.task.is_finished() {
                let done = (&mut s.task).await;
                app.summarised(done).await;
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the summary never came");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// A panel whose summariser, `fake-agent`, really runs and answers
    /// `answer`, with the top left to `/summary` and a second run, `echo 2`.
    /// None when `rl-session` is not built.
    async fn catch_up_with_fake_summariser(
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
            config: "catch_up:\n  summary: ask\n",
            answer: Some(answer),
            session: Some(session),
            ..Default::default()
        };
        let set = panel_set(setup, |folder, now| {
            vec![rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "echo 2",
                vec!["sh".into(), "-c".into(), "echo 2".into()],
                folder,
                folder,
                now,
            )]
        });
        Some(set.await)
    }

    #[tokio::test]
    async fn summary_deep_on_a_project_in_the_catch_up_asks_first_then_reads() {
        let Some((_tmp, token, mut app, _folder)) = catch_up_with_fake_summariser("Both ran, both fine.").await else {
            return;
        };
        let since = nine_hours_ago();
        app.open_catch_up(since, true).await;
        let head = app
            .catch_rows()
            .iter()
            .find(|r| matches!(r, super::Row::Group(_)))
            .and_then(|r| r.key());
        app.select(head);
        type_in(&mut app, "/summary deep").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(
            matches!(app.mode, super::Mode::SummaryCost { runs: 2, calls: 3 }),
            "two deep reads and one to join, asked first"
        );
        assert!(app.summarising.is_none(), "nothing asked before y");
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).await;
        let s = app.summarising.as_ref().expect("y asks");
        assert!(matches!(s.target, SummaryTarget::Project { .. }));
        assert_eq!(
            (s.deep, s.from),
            (true, Some(since)),
            "deep, over the catch-up's stretch"
        );
        wait_summarised(&mut app).await;
        assert!(app.view == super::View::CatchUp, "read where it was asked");
        let frame = draw_text(&mut app);
        assert!(frame.contains("Both ran, both fine."), "{frame}");
        assert!(frame.contains("deep"), "{frame}");
        token.cancel();
    }

    #[tokio::test]
    async fn the_tops_summary_over_another_stretch_is_shown_in_the_pane() {
        let Some((_tmp, token, mut app, _folder)) = catch_up_with_fake_summariser("• api: two runs, fine").await
        else {
            return;
        };
        app.open_catch_up(nine_hours_ago(), true).await;
        app.select(Some(super::TOP.into()));
        type_in(&mut app, "/summary 2d").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.show_pane, "not the top's stretch: the pane has it");
        wait_summarised(&mut app).await;
        assert!(app.top.is_none(), "the top is still the counts");
        let frame = draw_text(&mut app);
        assert!(frame.contains("two runs, fine"), "{frame}");
        app.leave_subpage();
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: 2 ran"), "{frame}");
        token.cancel();
    }

    #[tokio::test]
    async fn a_top_cut_at_half_the_room_says_where_the_rest_is() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        let since = nine_hours_ago();
        app.open_catch_up(since, true).await;
        let text: Vec<String> = (1..=20).map(|n| format!("• bullet {n}")).collect();
        let top = rl_proto::types::Summary {
            run: String::new(),
            agent: "fakeai".into(),
            by: "x1".into(),
            at: jiff::Timestamp::now(),
            text: text.join("\n"),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: SummaryTarget::All.key(),
            deep: false,
            from: Some(since),
            calls: 1,
            runs: 1,
        };
        app.top = Some(top.clone());
        let frame = draw_text(&mut app);
        assert!(frame.contains("• bullet 1 "), "{frame}");
        assert!(!frame.contains("• bullet 20"), "half the room at the most: {frame}");
        assert!(frame.contains("… ^o on the top shows the rest"), "{frame}");
        assert!(frame.contains("make test"), "the rows still have their room: {frame}");
        // And so it does.
        app.select(Some(super::TOP.into()));
        app.summary = Some(top);
        app.toggle_pane();
        let frame = draw_text(&mut app);
        assert!(frame.contains("bullet 20"), "{frame}");
        token.cancel();
    }

    #[tokio::test]
    async fn the_top_starts_counted_and_the_assistants_replaces_it() {
        let Some((_tmp, token, mut app, _folder)) = panel_with_fake_summariser("• api: one run, fine").await else {
            return;
        };
        let since = nine_hours_ago();
        app.open_catch_up(since, true).await;
        assert!(app.topping.is_some(), "auto: the top is being written");
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: 1 ran"), "counted bullets first: {frame}");
        assert!(frame.contains("writing the top"), "{frame}");
        wait_topped(&mut app).await;
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: one run, fine"), "{frame}");
        assert!(frame.contains("by fakeai · from 1 run"), "{frame}");
        assert!(!frame.contains("• api: 1 ran"), "the bullets gave way: {frame}");
        // Kept: the same stretch again asks no one — and shows it when the
        // stretch is asked for, too.
        app.back_to_list().await;
        app.open_catch_up(since, true).await;
        assert!(app.topping.is_none(), "the one kept is given back");
        assert!(draw_text(&mut app).contains("one run, fine"));
        app.back_to_list().await;
        app.open_catch_up(since, false).await;
        assert!(app.topping.is_none());
        assert!(draw_text(&mut app).contains("one run, fine"), "kept for that stretch");
        token.cancel();
    }

    #[tokio::test]
    async fn a_top_that_fails_keeps_the_bullets_and_says_why() {
        let Some((_tmp, token, mut app, _folder)) = panel_with_fake_summariser("").await else {
            return;
        };
        app.open_catch_up(nine_hours_ago(), true).await;
        wait_topped(&mut app).await;
        let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert!(said.contains("said nothing"), "{said}");
        assert!(draw_text(&mut app).contains("• api: 1 ran"));
        token.cancel();
    }

    #[tokio::test]
    async fn ask_means_no_call_on_open() {
        let (_tmp, token, mut app, _folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
        app.open_catch_up(nine_hours_ago(), true).await;
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
    async fn the_first_agent_here_never_writes_the_top_by_itself() {
        let setup = Setup {
            unnamed: true,
            ..Default::default()
        };
        let (_tmp, token, mut app, _folder) = panel_set(setup, |_, _| vec![]).await;
        app.open_catch_up(nine_hours_ago(), true).await;
        assert!(app.topping.is_none(), "no assistant named for summaries: no call");
        let frame = draw_text(&mut app);
        assert!(
            frame.contains("• api: 1 ran") && !frame.contains("writing the top"),
            "{frame}"
        );
        token.cancel();
    }

    #[tokio::test]
    async fn the_top_is_counted_from_everything_whatever_the_search() {
        let (_tmp, token, mut app, _folder) = panel_with_runs(2).await;
        app.filter = "echo".into();
        app.open_catch_up(nine_hours_ago(), true).await;
        let frame = draw_text(&mut app);
        assert!(frame.contains("• api: 2 ran"), "{frame}");
        assert!(!frame.contains("make test"), "the rows are narrowed: {frame}");
        token.cancel();
    }

    #[tokio::test]
    async fn a_rows_summary_goes_ahead_while_the_top_is_written() {
        let (_tmp, token, mut app, _folder) = panel_with_config("").await;
        app.open_catch_up(nine_hours_ago(), true).await;
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
        app.open_catch_up(since, true).await;
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
        app.open_catch_up(nine_hours_ago(), true).await;
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
        let (_tmp, token, mut app, _folder) = panel_with_config("").await;
        let before = jiff::Timestamp::now();
        // Longer than `catch_up.after`, but asked for: counted, and
        // `/summary` on the top writes it.
        type_in(&mut app, "/since 2d").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp);
        assert!(app.topping.is_none(), "a stretch asked for is counted");
        assert!(draw_text(&mut app).contains("• api: 1 ran"));
        let since = app.catch.as_ref().unwrap().since;
        let hour = jiff::SignedDuration::from_hours(48);
        assert!(
            since >= before - hour && since <= jiff::Timestamp::now() - hour,
            "{since}"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work);
        // From the events view too; bare, since you were last here.
        let left = nine_hours_ago();
        app.left = Some(left);
        app.toggle_events().await;
        assert!(app.view == super::View::Events);
        type_in(&mut app, "/since").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::CatchUp);
        assert_eq!(app.catch.as_ref().unwrap().since, left);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "esc goes to the list, not the events");
        type_in(&mut app, "/since soon").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.view == super::View::Work, "a stretch it cannot read opens nothing");
        token.cancel();
    }

    #[tokio::test]
    async fn a_flows_row_says_where_its_latest_firing_stands() {
        use rl_ledger::firings::{NewFiring, insert, save};
        use rl_proto::flow::{FiringStatus, StepStatus};
        let (tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "probe",
                vec!["true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(|c| {
                let loaded = rl_ledger::flows::Loaded {
                    name: "nightly".into(),
                    hash: "h".into(),
                    def_json: "{}".into(),
                };
                rl_ledger::flows::sync(c, &[loaded], jiff::Timestamp::now())?;
                let steps = ["probe".to_string(), "review".to_string()];
                insert(
                    c,
                    &NewFiring {
                        id: "fi1",
                        flow: "nightly",
                        def_json: "{}",
                        hash: "h",
                        trigger: "when",
                        event: None,
                        at: jiff::Timestamp::now(),
                        steps: &steps,
                    },
                )?;
                let at = [
                    ("probe".to_string(), StepStatus::Done),
                    ("review".to_string(), StepStatus::Waiting),
                ];
                save(c, "fi1", &at.into_iter().collect(), FiringStatus::Waiting, None)
            })
            .unwrap();
        app.refresh().await;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        let row = (0..40)
            .map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .find(|l| l.contains("↻ nightly"))
            .expect("the flow's row");
        assert!(row.contains("waiting at review"), "{row}");
        token.cancel();
    }

    /// `/flow` acts on the one named after its verb, else on the flow under
    /// the cursor; with neither it says what it needs.
    #[tokio::test]
    async fn flow_commands_act_on_the_one_named_else_the_flow_here() {
        let (tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "probe",
                vec!["true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(|c| {
                let loaded = rl_ledger::flows::Loaded {
                    name: "nightly".into(),
                    hash: "h".into(),
                    def_json: "{}".into(),
                };
                rl_ledger::flows::sync(c, &[loaded], jiff::Timestamp::now())
            })
            .unwrap();
        app.refresh().await;
        let said = |app: &super::App| app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        let nightly = |app: &super::App| app.flows.iter().find(|f| f.name == "nightly").cloned().unwrap();
        async fn run(app: &mut super::App, line: &str) {
            type_in(app, line).await;
            press(app, KeyCode::Enter, KeyModifiers::NONE).await;
        }

        app.select(Some(RUN.into()));
        run(&mut app, "/flow pause").await;
        assert_eq!(said(&app), "/flow pause which? a flow's row or its name");
        run(&mut app, "/flow approve nightly").await;
        assert_eq!(said(&app), "nightly approved");
        assert!(nightly(&app).approved());

        // On its row, no name is needed.
        app.select(Some("f:nightly".into()));
        assert_eq!(app.flow_here().as_deref(), Some("nightly"));
        run(&mut app, "/flow pause").await;
        assert_eq!(said(&app), "nightly paused");
        assert!(nightly(&app).paused);
        run(&mut app, "/flow resume").await;
        assert_eq!(said(&app), "nightly resumed");
        assert!(!nightly(&app).paused);
        // A name typed wins over the row: `other` is asked for, nightly left be.
        run(&mut app, "/flow pause other").await;
        assert!(said(&app).starts_with("pause other: "), "{}", said(&app));
        assert!(!nightly(&app).paused);

        run(&mut app, "/flow now").await;
        assert_eq!(
            said(&app),
            "/flow run|pause|resume|approve|edit|new|test|draft|reload [name]"
        );
        // Its `flow.md` waits for the screen to be given back.
        run(&mut app, "/flow edit").await;
        let home = rl_core::Home::at(tmp.path().join("h"));
        let (editing, line) = app.editor.take().expect("the editor is next");
        let file = home.flow_dir("nightly").join("flow.md");
        assert_eq!((editing.name.as_str(), &editing.file, line), ("nightly", &file, 1));
        // Back from the editor, the template as saved loads, and gets its yes.
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str("\nEdited.\n");
        std::fs::write(&file, text).unwrap();
        app.edited(editing).await;
        assert_eq!(said(&app), "nightly approved");
        assert!(nightly(&app).approved());
        // A new one left as the template is not kept.
        run(&mut app, "/flow edit draft").await;
        let (editing, _) = app.editor.take().expect("the editor is next");
        app.edited(editing).await;
        assert_eq!(said(&app), "draft left as the template: not kept");
        assert!(!home.flow_dir("draft").exists());
        // One that does not load is said why, and gets none.
        std::fs::create_dir_all(home.flow_dir("broken")).unwrap();
        std::fs::write(home.flow_dir("broken").join("flow.md"), "---\nsteps: [\n---\n").unwrap();
        app.edited(crate::flow::start_edit(&home, "broken").unwrap()).await;
        assert!(said(&app).starts_with("not loaded: broken · flow.md"), "{}", said(&app));
        assert!(app.flows.iter().all(|f| f.name != "broken"));

        run(&mut app, "/flow run nightly").await;
        assert!(said(&app).starts_with("fired nightly · firing "), "{}", said(&app));
        app.select(Some(RUN.into()));
        run(&mut app, "/flow edit not_a_name").await;
        assert_eq!(
            said(&app),
            "`not_a_name` is not a flow name; use lowercase letters, digits and dashes"
        );
        assert!(app.editor.is_none());
        token.cancel();
    }

    #[tokio::test]
    async fn edit_opens_the_selected_flows_file() {
        let (tmp, token, mut app, _folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "probe",
                vec!["true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
        ledger
            .call_blocking(|c| {
                let loaded = rl_ledger::flows::Loaded {
                    name: "nightly".into(),
                    hash: "h".into(),
                    def_json: "{}".into(),
                };
                rl_ledger::flows::sync(c, &[loaded], jiff::Timestamp::now())
            })
            .unwrap();
        app.refresh().await;
        let said = |app: &super::App| app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        async fn run(app: &mut super::App, line: &str) {
            type_in(app, line).await;
            press(app, KeyCode::Enter, KeyModifiers::NONE).await;
        }

        // On a flow's row, `/edit` opens its `flow.md` — no name needed.
        app.select(Some("f:nightly".into()));
        run(&mut app, "/edit").await;
        let home = rl_core::Home::at(tmp.path().join("h"));
        let (editing, line) = app.editor.take().expect("the editor is next");
        let file = home.flow_dir("nightly").join("flow.md");
        assert_eq!((editing.name.as_str(), &editing.file, line), ("nightly", &file, 1));

        // Off a flow — on a run — there is nothing to edit, and it says so.
        app.select(Some(RUN.into()));
        run(&mut app, "/edit").await;
        assert!(app.editor.is_none());
        assert_eq!(said(&app), "nothing here to edit · select a flow");
        token.cancel();
    }

    #[tokio::test]
    async fn the_filter_bar_is_compact_glyphs() {
        let (_tmp, token, app, _folder) = panel().await;
        // A glyph and its key for each kind, then a divider and hidden.
        assert_eq!(app.toggles_line().to_string(), "✦a  ⚑f  ❯s  │  ◌h");
        token.cancel();
    }

    #[tokio::test]
    async fn s_and_slash_shells_flip_the_shells_toggle() {
        let (_tmp, token, mut app, _folder) = panel().await;
        assert!(app.show_shells, "shells on by default");
        // `s` on an empty line flips it off.
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE).await;
        assert!(!app.show_shells);
        // `/shells` flips it back on.
        type_in(&mut app, "/shells").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.show_shells);
        token.cancel();
    }

    #[tokio::test]
    async fn a_flows_row_stands_in_its_project_however_its_in_is_spelled() {
        let (tmp, token, mut app, folder) = panel_of(|folder, now| {
            let mut r = rl_proto::types::Run::new(
                "run00002",
                rl_proto::types::RunKind::Command,
                "probe",
                vec!["true".into()],
                folder,
                folder,
                now,
            );
            r.cause = rl_proto::types::Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into(),
            };
            vec![r]
        })
        .await;
        // Not the project's own path: the temp folder as given (a link on
        // macOS), and a trailing slash.
        let spelled = format!("{}/proj/", tmp.path().display());
        let dir = rl_core::Home::at(tmp.path().join("h")).flow_dir("nightly");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("flow.md"),
            format!("---\nin: {spelled}\nsteps:\n  probe:\n    run: x\n---\n"),
        )
        .unwrap();
        app.rpc.call::<rl_proto::flow::FlowReload>(()).await.unwrap();
        app.refresh().await;
        let rows = app.rows();
        let at = rows
            .iter()
            .position(|r| matches!(r, super::Row::Flow(_)))
            .expect("the flow's row");
        let head = rows[..at].iter().rev().find_map(|r| match r {
            super::Row::Group(h) => Some(h.clone()),
            _ => None,
        });
        assert_eq!(
            head.map(|h| h.workspace),
            Some(folder.clone()),
            "under the project itself"
        );
        let headers = rows
            .iter()
            .filter(|r| matches!(r, super::Row::Group(h) if h.workspace == folder));
        assert_eq!(headers.count(), 1, "and no second header beside it");
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
