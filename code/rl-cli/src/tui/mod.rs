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
mod graph;
mod input;
mod line;
mod palette;
mod pane;
mod query;
mod remove;

pub use pane::recap_label;
mod rows;
mod rules;
mod screen;
mod setup;
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
use jiff::Timestamp;
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
    AgentList, DecisionDismiss, DecisionNote, DecisionTake, DecisionTakeParams, Digest, DigestGet, DigestItem,
    DigestParams, EventFilter, EventList, GcParams, GcRun, GraphGet, GraphParams, ImportList, ImportListParams,
    PresenceClient, PresenceTouch, ReactionPause, ReactionReload, ReactionResume, RuleApprove, RuleList, RuleRef,
    RunArchive, RunArchiveParams, RunId, RunIds, RunInput, RunInputParams, RunMark, RunMarkParams, RunPin,
    RunPinParams, RunRename, RunRenameParams, RunResume, RunResumeParams, RunSeen, RunSpec, RunStart, RunStop,
    RunStopParams, SETUP_STEPS, ScheduleFilter, ScheduleName, SchedulePause, ScheduleReload, ScheduleResume,
    ScheduleRun, SetupGet, SummaryGet, SummaryMake, SummaryParams, WorkspaceList, WorkspaceRemove,
    WorkspaceRemoveParams, WorkspaceRemoved, WorkspaceRename, WorkspaceRenameParams, WorkspaceUp, WorkspaceUpParams,
};
use rl_proto::rule::{Rule, RuleKind, RuleStatus};
use rl_proto::types::{AttachMode, Cause, Event as LedgerEvent, Mark, Run, RunKind, RunState, Summary};
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Work,
    /// The wiring graph, over every project.
    Graph,
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
    /// A search the list and the graph are narrowed to; see `query`.
    filter: String,
    /// `^f`: the line is searching, and the filter follows what is typed.
    search: Option<Search>,
    /// Which slice of the list is shown: all, agents, jobs or reactions.
    lens: rows::Lens,
    view: View,
    /// Every rule: for the chips on project rows, a job's rule, and the graph's nodes.
    rules: Vec<Rule>,
    /// The wiring, fetched while the graph is up.
    graph: Option<rl_proto::graph::Graph>,
    /// The view the graph was opened from, and the selection it had.
    graph_from: (View, Option<String>),
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
    /// The selected run's summary, when there is one kept.
    summary: Option<Summary>,
    /// The assistant at work on a run's summary (`/summarise`).
    summarising: Option<Summarising>,
    /// A project on its way out (`^x` twice on its header).
    removing: Option<Removing>,
    /// `/edit file` (or a scope's file): the file and line to open in the editor once the
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

/// The icon slot: a space, a two-column icon, a space. Every row has one, so
/// every title starts in the same column.
const ICON_W: usize = 4;
/// The state column's width: `needs you~` and a job's marks both fit.
const WORD_W: usize = 10;

/// Where the state column starts, for a title column `tw` wide — where a
/// header's checkout starts too, so the two line up.
fn state_col(tw: usize) -> usize {
    ICON_W + tw + 1
}

/// A row's icon, in its slot.
fn icon_span(icon: &str) -> Span<'static> {
    Span::raw(format!(" {icon} "))
}

/// One row of the list, to the width: icon, title, state, whatever goes
/// before the summary (who is joined), the summary filling the middle, then
/// the ref or branch and the age at the right.
#[allow(clippy::too_many_arguments)]
fn list_line(
    w: usize,
    tw: usize,
    icon: &str,
    title: Span<'static>,
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
    let mut spans = vec![icon_span(icon), title, Span::raw(" ")];
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
fn header_line(h: &rows::Head, w: usize, tw: usize) -> Line<'static> {
    let col = state_col(tw);
    let (parent, last, own) = rows::place_parts(&h.place, h.own.as_deref(), col - 2);
    let mut l = vec![
        Span::styled(parent, theme::dim()),
        Span::styled(last, theme::name()),
        Span::styled(own, theme::dim()),
    ];
    let width = |l: &[Span]| l.iter().map(Span::width).sum::<usize>();
    // The tally at the right edge is the honest total; a rule that waits for a
    // yes is named beside it, since that one is yours to give.
    let mut right: Vec<Span<'static>> = Vec::new();
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
        Constraint::Length(1),
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
/// modifier (`CSI 13;2u`), which is what the line is watching for.
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
            checkouts: Vec::new(),
            lost: Vec::new(),
        },
        names: Names::default(),
        filter: String::new(),
        search: None,
        lens: rows::Lens::All,
        view: View::Work,
        rules: Vec::new(),
        graph: None,
        graph_from: (View::Work, None),
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
        asking: None,
        summary: None,
        summarising: None,
        removing: None,
        picked: Vec::new(),
        line: line::Line::default(),
        agents: Vec::new(),
        path_dirs: complete::path_dirs(),
        aliases: complete::shell_names().await,
        completing: None,
        pick: None,
        mode: Mode::Normal,
        flash: None,
        arm: None,
        quit: false,
    };
    app.refresh().await;

    let mut terminal = enter_screen();
    // The first start, or a release with a step not seen yet: the guide
    // before the list, asking only what is new. Otherwise a word about
    // Claude's sessions that are not here, once each.
    if let Ok(state) = app.rpc.call::<SetupGet>(()).await {
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
    let result = app.main_loop(&mut terminal).await;
    leave_screen();
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
        let a = areas(size, self.wide, true, LIST_SHARE_TERM, 0, 0, self.line_h());
        let r = a.pane.unwrap_or(size);
        // Less the row that names the run.
        (r.width.max(1), r.height.saturating_sub(1).max(1))
    }

    /// How many rows the live line takes: its own, capped, at least one.
    fn line_h(&self) -> u16 {
        if self.search.is_some() {
            return 1;
        }
        (self.line.rows() as u16).clamp(1, LINE_MAX)
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
                filter: &self.filter,
                lens: self.lens,
                now: Timestamp::now(),
            },
        )
    }

    /// The folder the selected row's work is in.
    fn selected_folder(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Run(i) => Some(i.run.workspace.clone()),
            Row::Job(j) => Some(j.last().run.workspace.clone()),
            Row::Question(q) => Some(q.workspace.clone()),
            Row::Group(h) => Some(h.workspace.clone()),
        }
    }

    /// The registered folder under the cursor, when the cursor is on a project
    /// header — what `^r` and `/edit schedule` act on.
    fn selected_project(&self) -> Option<PathBuf> {
        match self.selected()? {
            Row::Group(h) => Some(h.workspace.clone()),
            _ => None,
        }
    }

    /// Keys of the selectable rows of the view that is up, in order.
    fn keys(&self) -> Vec<String> {
        match self.view {
            View::Work => self.rows().into_iter().filter_map(|r| r.key()).collect(),
            View::Graph => self.graph_plan().map(|(_, p)| p.order()).unwrap_or_default(),
        }
    }

    /// The graph as the filter leaves it, laid out.
    fn graph_plan(&self) -> Option<(&rl_proto::graph::Graph, graph::Plan)> {
        let g = self.graph.as_ref()?;
        let kept = graph::keep(g, &self.filter, &self.names, &self.digest.checkouts);
        Some((g, graph::plan(g, &kept, &self.names, Timestamp::now())))
    }

    /// Keeps the selection on something that exists.
    fn settle(&mut self) {
        let keys = self.keys();
        if self.sel.as_ref().is_none_or(|k| !keys.contains(k)) {
            self.select(keys.first().cloned());
        }
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
            // A question is only worth its details: selecting one opens the
            // pane on it, and moving off closes the pane again — unless it was
            // open already.
            let question = self.view == View::Work && key.as_deref().is_some_and(|k| k.starts_with("q:"));
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

    /// The question the line answers: the one under the cursor on the list,
    /// or the first one waiting at the node under it in the graph.
    fn question_here(&self) -> Option<&Decision> {
        match self.view {
            View::Work => self.selected_question(),
            View::Graph => {
                let n = self.selected_node()?;
                n.waiting
                    .iter()
                    .find_map(|id| self.digest.decisions.iter().find(|d| &d.id == id))
            }
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
    async fn saw_selected(&mut self) {
        let ids: Vec<String> = match self.selected() {
            Some(Row::Run(i)) if i.unseen => vec![i.run.id.clone()],
            Some(Row::Job(j)) => j.runs.iter().filter(|i| i.unseen).map(|i| i.run.id.clone()).collect(),
            _ => vec![],
        };
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
        self.saw_selected().await;
        self.refresh().await;
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

    /// A paste, whole: into the line as it came, newlines and all, or a
    /// form's field a character at a time.
    async fn paste(&mut self, s: String, area: Rect) -> Result<()> {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
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
    /// ledger, and a job by the rule it comes from, which is the rule form's
    /// own rename.
    fn rename_here(&mut self) {
        if let Some(folder) = self.selected_project() {
            let name = self.names.workspace(&folder);
            self.mode = Mode::Form(Box::new(Form::rename(&folder, &name)));
        } else if let Some((id, title)) = self.selected_run().map(|i| (i.run.id.clone(), i.run.title.clone())) {
            self.mode = Mode::Form(Box::new(Form::rename_run(&id, &title)));
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
                        if self.view == View::Graph {
                            if next_node.as_ref().is_some_and(|id| self.keys().contains(id)) {
                                self.select(next_node);
                            }
                        } else {
                            self.select(Some(format!("r:{r}")));
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
        if self.view == View::Graph {
            self.toggle_graph().await;
        }
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
            let (view, sel) = self.graph_from.clone();
            self.graph_from.1 = std::mem::replace(&mut self.sel, sel);
            self.view = view;
        } else {
            let sel = self.graph_from.1.take();
            self.graph_from = (self.view, std::mem::replace(&mut self.sel, sel));
            self.view = View::Graph;
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
        if self.view != View::Work {
            return None;
        }
        if let Some(j) = self.selected_job() {
            return self.rule_of_job(&j).map(rules::key);
        }
        let scope = match self.selected() {
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
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn new_rule_form(&mut self) {
        let folder = self.folder_here();
        // A new rule is global from a global rule's node in the graph; else
        // it belongs to the folder here, and `in: global` typed into the form
        // makes it global from anywhere.
        let yours = self.view == View::Graph
            && self.selected_node().is_some_and(|n| n.folder.is_none())
            && self.rule_here().is_none_or(Rule::is_global);
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

    /// `/edit file`: the rule's file in the editor, at its line, once the screen is free.
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

    /// `/edit schedule|reaction [global]`: the whole rule file for the scope
    /// the cursor stands in, rather than the one rule under it. Any row
    /// answers which folder its work is in, so this reads the same from a run
    /// as from the folder's own header, and a node in the graph answers with
    /// its own folder. Where nothing names a folder — an empty list, a global
    /// rule's node — it is the Relay home's file, the global rules having no
    /// row of their own to stand on; `global` asks for that file from
    /// anywhere.
    fn edit_scope_file(&mut self, kind: RuleKind, global: bool) {
        let folder = if global {
            None
        } else {
            self.selected_folder()
                .or_else(|| self.selected_node().and_then(|n| n.folder.clone()))
        };
        self.editor = Some((rulefile::file(&self.home, kind, folder.as_deref()), 1));
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
            form::Kind::Rename => {
                self.rename_project(f.origin.clone(), f.get("name").trim().to_string())
                    .await
            }
            form::Kind::RenameRun => match f.renaming.clone() {
                Some(id) => self.rename_run(id, f.get("name").trim().to_string()).await,
                None => Err(anyhow::anyhow!("no run to rename")),
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
        if self.view == View::Graph {
            self.toggle_graph().await;
        }
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

    /// Give a project a new name. The cursor stays on it, under the new key.
    async fn rename_project(&mut self, folder: PathBuf, name: String) -> Result<()> {
        if name.is_empty() {
            anyhow::bail!("a name, please");
        }
        self.rpc
            .call::<WorkspaceRename>(WorkspaceRenameParams {
                path: folder.clone(),
                name: name.clone(),
            })
            .await?;
        self.say(format!("renamed to {name}"));
        self.refresh().await;
        self.select(Some(format!("g:{}", folder.display())));
        Ok(())
    }

    /// A run's own name, which is its title. The ledger is the only place it
    /// lives, so the rename is one call and the next refresh shows it.
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

    async fn rename_run(&mut self, id: String, title: String) -> Result<()> {
        if title.is_empty() {
            anyhow::bail!("a name, please");
        }
        self.rpc
            .call::<RunRename>(RunRenameParams {
                id: id.clone(),
                title: title.clone(),
            })
            .await?;
        self.say(format!("renamed to {title}"));
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

    /// `s` on an ended run: the assistant is asked what it did, as a run; the
    /// answer lands in the pane through the main loop.
    /// `/summarise [since]`: a recap of the run under the cursor, live or
    /// ended — the one kept while the run has not moved on.
    fn summarise(&mut self, since: &str) {
        if let Some(s) = &self.summarising {
            self.say(format!("still summarising `{}`; ^x on its run stops it", s.title));
            return;
        }
        let Some(i) = self.selected_run() else {
            self.say("/summarise works on a run");
            return;
        };
        let run = &i.run;
        if matches!(run.cause, Cause::Assistant(_)) {
            self.say("an assistant's own run; nothing to summarise");
            return;
        }
        let (id, title) = (run.id.clone(), run.title.clone());
        let home = self.home.clone();
        let params = SummaryParams {
            run: id.clone(),
            since: (!since.is_empty()).then(|| since.to_string()),
            again: false,
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
                self.say(format!(
                    "recap of `{}` · {}",
                    s.title,
                    pane::recap_label(&summary, Timestamp::now())
                ));
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
        // A destructive key stays armed only until the very next key: its own
        // second press acts, anything else disarms it. (ctrl-x/ctrl-c re-check
        // and re-arm themselves in `key_list`.)
        if !matches!(
            (k.code, k.modifiers),
            (KeyCode::Char('c') | KeyCode::Char('x'), KeyModifiers::CONTROL)
        ) {
            self.arm = None;
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
                let page = area.height.saturating_sub(3).max(1);
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

    /// `^f` or `/filter`: search, starting from the filter already in force.
    fn open_search(&mut self) {
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

    /// `tab` in a search: the token names, then a token's values — states,
    /// kinds, projects, rules. One fits: taken. Several: what they share, and
    /// the rest shown in the footer. None — a plain word, or no word at all
    /// after one — and it goes to the next match instead.
    fn complete_search(&mut self) {
        let mut projects: Vec<String> = self.names.paths().iter().map(|p| self.names.workspace(p)).collect();
        projects.push("global".into());
        let rules: Vec<String> = self.rules.iter().map(|r| r.name().to_string()).collect();
        let branches: Vec<String> = (self.digest.checkouts.iter().filter_map(|c| c.branch.clone()))
            .chain(self.digest.items.iter().filter_map(|i| i.run.branch.clone()))
            .collect();
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let (text, cur) = (s.line.text().to_string(), s.line.cursor());
        let start = text[..cur].rfind(' ').map_or(0, |i| i + 1);
        let word = &text[start..cur];
        let cands = query::complete(word, &projects, &rules, &branches);
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
                }
            }
            (KeyCode::Down, _) if self.view == View::Graph => self.graph_step(graph::Dir::Down),
            (KeyCode::Up, _) if self.view == View::Graph => self.graph_step(graph::Dir::Up),
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
        if self.view == View::Graph {
            self.graph_enter();
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
        } else if self.selected_project().is_some() {
            // A project header: its rules in the pane.
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
            folder,
            agents: &self.agents,
            path_dirs: &self.path_dirs,
            aliases: &self.aliases,
        }
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
            let here = self.here(&folder);
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
        if let Some(rest) = self.line.text().strip_prefix('/') {
            let n = palette::matching(rest).len();
            if n > 0 {
                return Some(Cands::Slash(n));
            }
        }
        match &self.completing {
            Some(c) if !c.cands.is_empty() => Some(Cands::Words(c.cands.len())),
            _ => None,
        }
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
            palette::Cmd::Up => self.add_dir(&arg).await?,
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
            palette::Cmd::Graph => self.toggle_graph().await,
            palette::Cmd::Shell => self.new_shell(area).await,
            palette::Cmd::Reload => match arg.trim() {
                "rules" => self.reload_rules().await,
                "shell" => self.reload_shell().await,
                "" => {
                    self.reload_rules().await;
                    self.reload_shell().await;
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
            palette::Cmd::Edit => {
                let words: Vec<&str> = arg.split_whitespace().collect();
                let kind = match words.first().copied() {
                    Some("schedule" | "schedules") => Some(RuleKind::Schedule),
                    Some("reaction" | "reactions") => Some(RuleKind::Reaction),
                    _ => None,
                };
                match (words.as_slice(), kind) {
                    ([], _) => self.edit_rule_form(),
                    (["file"], _) => self.edit_rule_file(),
                    ([_], Some(kind)) => self.edit_scope_file(kind, false),
                    ([_, "global"], Some(kind)) => self.edit_scope_file(kind, true),
                    _ => self.say(format!("edit {}? file, or schedule or reaction [global]", arg.trim())),
                }
            }
            palette::Cmd::Summarise => self.summarise(arg.trim()),
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
        let slash: Option<Vec<&palette::Command>> = matches!(self.mode, Mode::Normal)
            .then(|| self.line.text().strip_prefix('/').map(palette::matching))
            .flatten();
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
        let a = areas(
            f.area(),
            self.wide,
            self.show_pane,
            share,
            form_h,
            cands_h,
            self.line_h(),
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
        };
        // The header carries three things on one line: the tally at the left,
        // the lens at the right of it, and who you are at the edge. The lens
        // rides here rather than a row of its own.
        let lens = (self.view == View::Work).then(|| self.lens_line());
        let lens_w = lens.as_ref().map_or(0, |l| l.width() as u16 + 2);
        let [_, l, lens_area, r] = Layout::horizontal([
            Constraint::Length(TEXT_INDENT),
            Constraint::Fill(1),
            Constraint::Length(lens_w),
            Constraint::Length(self.me.len() as u16 + 1),
        ])
        .areas(a.head);
        // The lead word wears the accent; the tally after it reads plainly.
        let mut head = match title.split_once(" · ") {
            Some((lead, rest)) => Line::from(vec![
                Span::styled(lead.to_string(), theme::accent().add_modifier(Modifier::BOLD)),
                Span::raw(format!(" · {rest}")),
            ]),
            None => Line::from(Span::styled(title, theme::accent().add_modifier(Modifier::BOLD))),
        };
        // What a restart took waits on you like anything else that does.
        if self.view == View::Work
            && let Some(note) = rows::lost_note(&self.digest)
        {
            head.spans.push(Span::raw(" · "));
            head.spans.push(Span::styled(note, theme::needs()));
        }
        // The global rules have no row — nothing would sit under one — so the
        // headline says them, amber when one waits for a yes. The graph shows
        // them wired, and it is where that yes is given.
        if self.view == View::Work
            && let Some((note, waiting)) = rows::everywhere(&rows::chips(&self.rules, None, Timestamp::now()))
        {
            head.spans.push(Span::raw(" · "));
            head.spans.push(if waiting {
                Span::styled(note, theme::needs())
            } else {
                Span::raw(note)
            });
        }
        f.render_widget(Paragraph::new(head), l);
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
        if matches!(self.mode, Mode::Normal) {
            f.render_widget(Paragraph::new(self.line_lines(a.line.width)), a.line);
        }
        let footer = Rect {
            x: a.footer.x + TEXT_INDENT,
            width: a.footer.width.saturating_sub(TEXT_INDENT),
            ..a.footer
        };
        f.render_widget(Paragraph::new(self.footer()).dim(), footer);
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
            let here = self.here(&folder);
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
                    header_line(h, w, tw)
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
                        rows::QUESTION,
                        Span::styled(fit(&title, tw), theme::name()),
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
                    // A pinned run stands above every folder, so it says
                    // which one it is from.
                    let title = if under {
                        format!("  {}", r.title)
                    } else if r.pinned_at.is_some() {
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
                        None => i.summary.as_str(),
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
                        rows::icon(r),
                        title,
                        vec![Span::styled(rows::run_words(r), style)],
                        summary,
                        joined_lead(i.joined_by.as_deref()),
                        (!aside.is_empty()).then_some(aside.as_str()),
                        fmt_age(i.at, now),
                    )
                }
                Row::Job(j) => {
                    let last = j.last();
                    let title = Span::raw(fit(&j.name, tw));
                    let title = if j.unseen() { title.bold() } else { title };
                    // Its state is how the last few went, oldest first; the
                    // tally and the cadence are the pane's.
                    list_line(
                        w,
                        tw,
                        rows::JOB,
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
        let lines = match self.selected() {
            Some(Row::Run(i)) => pane::run_lines(
                i,
                &self.events,
                self.summary.as_ref().filter(|s| s.run == i.run.id),
                &self.names,
                area.width as usize,
                now,
            ),
            Some(Row::Job(j)) => pane::job_lines(&j, self.rule_of_job(&j), &self.names, area.width as usize, now),
            Some(Row::Question(q)) => {
                let asker = q.run.as_deref().and_then(|id| self.run_by_id(id));
                let at = matches!(&self.mode, Mode::Answer(id) if *id == q.id).then_some(self.answer_at);
                let p = pane::question_lines(q, asker, &self.picked, at, area.width as usize, now);
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
            Some(Row::Group(h)) => pane::project_lines(
                &self.names.workspace(&h.workspace),
                &h.place,
                &h.workspace,
                &self.digest.items,
                &self.rules,
                now,
            ),
            _ => vec![Line::from(Span::raw(" nothing selected").dim())],
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn footer(&self) -> String {
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
            Mode::Normal => {}
        }
        if let Some((m, at)) = &self.flash
            && at.elapsed() < FLASH
        {
            return m.clone();
        }
        if self.search.is_some() {
            return "type to narrow · tab completes in: is: kind: by:, or next match · ↑↓ move · enter keeps · esc puts back".into();
        }
        if let Some(a) = &self.asking {
            return format!("asking about \"{}\"… · its run is in the list · /stop there", a.words);
        }
        if let Some(s) = &self.summarising {
            return format!("summarising `{}`… · its run is in the list · /stop there", s.title);
        }
        if let Some(r) = &self.removing {
            return format!("removing {}… · stopping its runs", r.name);
        }
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
        let pane = if self.show_pane {
            "^o hides details"
        } else {
            "^o details"
        };
        match self.selected() {
            Some(Row::Question(_)) => format!("enter or `1 and a note` on the line answers it · ^x not this · {pane}"),
            Some(Row::Job(_)) => {
                format!("enter its firings · /edit its rule · ^x archives · {pane}")
            }
            Some(Row::Run(i)) if i.run.state.is_terminal() => {
                format!("enter output · /summarise · ^x archives · ^t pins · {pane}")
            }
            Some(Row::Run(_)) => format!("enter joins it · ^x stops it · ^t pins · {pane}"),
            Some(Row::Group(_)) => {
                format!("enter its stats · /shell · ^g its wiring · /edit schedule · ^r renames · ^x removes · {pane}")
            }
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

#[cfg(test)]
mod tests {
    use super::{ListHit, cand_window, group_count, hit_row, spawn_size, vi_step, viewport};
    use crate::tui::rows::Chip;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use rl_proto::rule::{RuleKind, RuleStatus};

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
        use super::{header_line, list_line, state_col};
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

        // A run's state word starts where a header's checkout does, and every
        // title in the same column, whatever icon it wears.
        let row = |icon: &str, word: &str| {
            list_line(
                w,
                tw,
                icon,
                Span::raw(crate::tui::rows::fit("title", tw)),
                vec![Span::raw(word.to_string())],
                "summary",
                vec![],
                None,
                "2m".into(),
            )
        };
        for (icon, word) in [(rows::AGENT, "needs you"), (rows::SHELL, ""), (rows::JOB, "·×··")] {
            let l = row(icon, word);
            assert_eq!(col_of(&l, "title"), 4, "{l}");
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
}
