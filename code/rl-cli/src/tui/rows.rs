//! The one list: questions and runs, grouped by what they belong to, each
//! where it came and staying there, and named in words rather than ids and
//! paths.
//!
//! A group is a run's ref, else its folder — never its branch: a folder is one
//! project whichever branch it is on, and its header says which. Groups come in
//! order of name, until you move one (shift-↑/↓ on its header): then in yours,
//! and what you have not placed after it by name; `everywhere` comes last.
//! Inside a group, runs and flows stand in the order they came — a run by when
//! it started, a flow by its oldest run on the list — the newest at the
//! bottom; nothing that happens to a row moves it. A question stands right
//! under the run that asked it, or, that run not being in the group, at the
//! bottom of it. A workspace path reads as the name it was registered under.
//!
//! A flow's step runs, once they have ended, fold into one row per flow —
//! its name, how the last few went, and where its latest firing stands — so
//! a flow firing every thirty seconds is one row, not a screen, and it never
//! grows into one: its firings are in the pane. The row stands in the
//! project the flow is `in`, wherever its runs ran; a flow with no `in`
//! stands under `everywhere`. A run still going, or waiting on you, is never
//! folded. A flow with a yes has its row from that yes on — what starts it
//! and when it fires next, before it ever fired — and its row keeps all the
//! runs the digest has of it, however long ago they ended.
//!
//! Before all of it, pinned runs, in the order they were pinned.
//!
//! What you hid (`/hide`: a run, a flow, a project and everything in it) is
//! off the list, back inline only when the `hidden` toggle is on — unless it
//! comes to need you after you hid it: then it comes through, above the pinned
//! runs, whenever hidden is off.
//!
//! Agents' own sessions — Claude's that `claude agents` lists, and others',
//! that are not Relay's runs — stand under the project `rld` filed them
//! under, as a run in their folder would be, in the order they came with
//! its runs. They are agents, not a flow's; `/hide` takes one off the list
//! as it does a flow, and one that comes to need you comes through.
//!
//! A project whose rows you moved (shift-↑/↓) keeps your order: every row
//! stands where you put it, whatever comes of it, what is new enters at the
//! bottom, and `/sort` lets it go back to the order its rows came in.

use super::flows;
use super::outside;
use super::query::{Named, Query, Subject};
use crate::rank;
use jiff::{SignedDuration, Timestamp};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, Flow, FlowStatus};
use rl_proto::methods::{Digest, DigestItem, Hidden, Order, Outside};
use rl_proto::types::{Cause, Checkout, Confidence, Run, RunKind, RunState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Ended runs stay on the list this long — but a flow's with a row of its
/// own, which stay as long as the digest has them.
pub const RECENT: SignedDuration = SignedDuration::from_hours(24);

/// Whether a run's kind and cause let it onto the list, given the toggles.
/// Not exclusive: a flow's agent step is both an agent and a flow's. A run
/// that is none of agent, shell or flow is always shown; an agent run (by
/// kind) when `show_agents`; a shell run (by kind) when `show_shells`; a
/// flow's run (by cause) when `show_flows` — a run that is more than one shows
/// when any of its toggles is on, each adding what it is for. Whether it is
/// hidden is `build`'s to say. Questions ignore all of it: they are what needs you.
fn keeps(r: &Run, show_agents: bool, show_flows: bool, show_shells: bool) -> bool {
    let is_agent = r.kind == RunKind::Agent;
    let is_flow = matches!(r.cause, Cause::Flow { .. });
    let is_shell = r.is_shell();
    (!is_agent && !is_flow && !is_shell)
        || (is_agent && show_agents)
        || (is_flow && show_flows)
        || (is_shell && show_shells)
}

pub enum Row<'a> {
    /// A group: where it is, what it is called, and the flows that are in it.
    Group(Head),
    Question(&'a Decision),
    Run(&'a DigestItem),
    /// A flow's ended runs, as one row.
    Flow(FlowRow<'a>),
    /// One of an agent's own sessions, not a Relay run.
    Outside(&'a Outside),
}

impl<'a> Row<'a> {
    /// Selectable rows carry a stable key, so the cursor survives a re-rank. A
    /// folder header is selectable by its path.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Group(h) if h.is_everywhere() => Some(format!("g:{EVERYWHERE}")),
            Row::Group(h) => Some(format!("g:{}", h.workspace.display())),
            Row::Question(d) => Some(format!("q:{}", d.id)),
            Row::Run(i) => Some(format!("r:{}", i.run.id)),
            Row::Flow(f) => Some(f.key.clone()),
            Row::Outside(c) => Some(c.key()),
        }
    }

    /// The run a row is.
    pub fn item(&self) -> Option<&'a DigestItem> {
        match self {
            Row::Run(i) => Some(i),
            _ => None,
        }
    }
}

/// A group's header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The folder, with `~` for home.
    pub place: String,
    /// The folder itself, so actions can act on it.
    pub workspace: PathBuf,
    /// A ref, or a folder by the name it was registered under.
    pub name: String,
    /// The name it goes by where that is not its folder's own — a ref, or a
    /// folder registered (or renamed) under another name.
    pub own: Option<String>,
    /// The flows `in` that folder, needing a yes first, then by name.
    pub flows: Vec<Chip>,
    /// The folder's git checkout, when it is in one.
    pub checkout: Option<Checkout>,
    /// How many of the project's items need you but are off the list now —
    /// hidden, or a toggle dropped them. The header hints at them so nothing
    /// urgent is silently buried; `0` when everything that needs you is shown.
    pub attention: usize,
}

impl Head {
    /// The `everywhere` group's header: no folder, and not a project.
    pub fn everywhere() -> Self {
        Self {
            place: EVERYWHERE.into(),
            workspace: PathBuf::new(),
            name: EVERYWHERE.into(),
            own: None,
            flows: Vec::new(),
            checkout: None,
            attention: 0,
        }
    }

    /// Whether this is the `everywhere` group, which has no folder to act on.
    pub fn is_everywhere(&self) -> bool {
        self.workspace.as_os_str().is_empty()
    }
}

/// The group, and the word, for the flows with no `in` and their runs
/// outside every project. It comes after the projects.
pub const EVERYWHERE: &str = "everywhere";
/// Its key among the groups: nothing a folder or a ref could be.
const EVERYWHERE_KEY: &str = "\u{0}everywhere";

/// The group a flow's row stands in, as `(key, folder)`: the folder it is
/// `in` (as the daemon resolved it, so it is the project's own path), else
/// `everywhere` — also for a flow not loaded now, whose `in` is not known.
fn flow_group(flows: &[Flow], name: &str) -> (String, PathBuf) {
    match flows.iter().find(|f| f.name == name).and_then(|f| f.folder.clone()) {
        Some(p) => (p.to_string_lossy().into_owned(), p),
        None => (EVERYWHERE_KEY.to_string(), PathBuf::new()),
    }
}

/// The group a run is shown under, as `(key, folder)`: an ended flow run's
/// is its flow's row's; a flow's run still going outside every project —
/// in a folder that is not a registered project, filed under no ref — is
/// `everywhere`'s; anything else goes under its ref, else its folder.
fn group_of(r: &Run, names: &Names, flows: &[Flow]) -> (String, PathBuf) {
    if let Some(flow) = flow_of(r) {
        return flow_group(flows, flow);
    }
    if matches!(r.cause, Cause::Flow { .. }) && r.ref_.is_none() && names.registered(&r.workspace).is_none() {
        return (EVERYWHERE_KEY.to_string(), PathBuf::new());
    }
    (r.place_key(), r.workspace.clone())
}

/// Whether a run is shown under `everywhere`: a flow's, outside every
/// project or folded into a flow with no `in`.
pub fn everywhere_run(r: &Run, names: &Names, flows: &[Flow]) -> bool {
    group_of(r, names, flows).0 == EVERYWHERE_KEY
}

/// The group a run is shown under: the `everywhere` one, or its ref, else
/// its folder — or, ended and a flow's, its flow's row's.
pub fn group_key(r: &Run, names: &Names, flows: &[Flow]) -> String {
    group_of(r, names, flows).0
}

/// The key a header is hidden by, as its row's own: `g:<folder>`, or
/// `g:everywhere`.
fn head_key((key, workspace): &(String, PathBuf)) -> String {
    if key == EVERYWHERE_KEY {
        format!("g:{EVERYWHERE}")
    } else {
        format!("g:{}", workspace.display())
    }
}

/// The flow a run is a step of, by its row's key, whether or not it has ended.
fn flow_key_of(r: &Run) -> Option<String> {
    match &r.cause {
        Cause::Flow { flow, .. } => Some(flow_key(flow)),
        _ => None,
    }
}

/// What hides a run from the list — its own `/hide`, its project's, its
/// flow's — and since when: the latest of them, the last time you said so.
/// `None`: it is on the list.
pub fn hidden_since(r: &Run, names: &Names, flows: &[Flow], hidden: &[Hidden]) -> Option<Timestamp> {
    let group = group_of(r, names, flows);
    let at = |k: &str| hidden.iter().find(|h| h.key == k).map(|h| h.at);
    let head = at(&head_key(&group));
    let flow = flow_key_of(r).and_then(|k| at(&k));
    [r.hidden_at, head, flow].into_iter().flatten().max()
}

/// Whether what hides a run is its project or its flow rather than its own
/// `/hide` — what bringing the run back alone cannot undo.
pub fn hidden_with(r: &Run, names: &Names, flows: &[Flow], hidden: &[Hidden]) -> Option<&'static str> {
    let group = group_of(r, names, flows);
    let has = |k: &str| hidden.iter().any(|h| h.key == k);
    if has(&head_key(&group)) {
        Some("its project")
    } else if flow_key_of(r).is_some_and(|k| has(&k)) {
        Some("its flow")
    } else {
        None
    }
}

/// A hidden run that has come to need you since it was hidden — asking,
/// blocked or failed after you said so. It comes through to the list.
pub fn comes_through(r: &Run, since: Timestamp) -> bool {
    matches!(r.state, RunState::NeedsInput | RunState::Blocked | RunState::Failed) && r.state_at > since
}

/// A checkout on a header: `⎇ main · PR #95 · 3 worktrees` — the badges after
/// the branch, the count on the main checkout only and only when there are any.
pub fn checkout_words(c: &Checkout) -> String {
    let mut words = vec![format!("⎇ {}", c.head())];
    words.extend(c.badges.iter().cloned());
    match c.worktrees {
        0 => {}
        1 => words.push("1 worktree".into()),
        n => words.push(format!("{n} worktrees")),
    }
    words.join(" · ")
}

/// What a run's row says of its branch: only where it started on another
/// than the one its folder is on now, so a switch does not hide where it ran.
pub fn branch_aside(r: &Run, checkouts: &[Checkout]) -> Option<String> {
    let started = r.branch.as_deref()?;
    let now = checkout_of(checkouts, &r.workspace)?;
    (now.branch.as_deref() != Some(started)).then(|| format!("⎇ {started}"))
}

/// A folder's checkout among those the digest carries.
pub fn checkout_of<'a>(checkouts: &'a [Checkout], folder: &Path) -> Option<&'a Checkout> {
    checkouts.iter().find(|c| c.path == folder)
}

/// A flow on its project's header: `↻ nightly 4h`, `↻ tidy paused`,
/// `! deploy` when it needs a yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub name: String,
    pub status: FlowStatus,
    /// One that is on: how long until it fires.
    pub due: Option<String>,
}

impl Chip {
    pub fn needs_yes(&self) -> bool {
        self.status == FlowStatus::NeedsApproval
    }

    pub fn text(&self) -> String {
        match (&self.status, &self.due) {
            (FlowStatus::NeedsApproval, _) => format!("! {}", self.name),
            (FlowStatus::Paused, _) => format!("{FLOW} {} paused", self.name),
            (_, Some(d)) => format!("{FLOW} {} {d}", self.name),
            (_, None) => format!("{FLOW} {}", self.name),
        }
    }
}

/// The flows `in` one folder (`None`: those with no `in`) as chips.
pub fn chips(flows: &[Flow], scope: Option<&Path>, now: Timestamp) -> Vec<Chip> {
    let mut out: Vec<Chip> = flows
        .iter()
        .filter(|f| f.removed_at.is_none())
        .filter(|f| f.folder.as_deref() == scope)
        .map(|f| Chip {
            name: f.name.clone(),
            status: f.status(),
            due: match (f.status(), f.next_at) {
                (FlowStatus::On, Some(t)) if t > now => Some(fmt_age(now, t)),
                (FlowStatus::On, Some(_)) => Some("due".into()),
                _ => None,
            },
        })
        .collect();
    out.sort_by(|a, b| b.needs_yes().cmp(&a.needs_yes()).then_with(|| a.name.cmp(&b.name)));
    out
}

/// A run's state as a word on its row, with `~` or `?` when it is a guess.
pub fn state_words(state: RunState, confidence: Confidence) -> String {
    let word = match state {
        RunState::Starting => "starting",
        RunState::Working => "working",
        RunState::Idle => "idle",
        RunState::NeedsInput => "needs you",
        RunState::Blocked => "blocked",
        RunState::Stale => "stale",
        RunState::Finished => "done",
        RunState::Failed => "failed",
        RunState::Stopped => "stopped",
    };
    match confidence {
        Confidence::Exact => word.to_string(),
        Confidence::Likely => format!("{word}~"),
        Confidence::Guess => format!("{word}?"),
    }
}

pub use rl_proto::types::SHELL_TAG;

/// The icon slot at the left of every row: what a thing is, as one quiet
/// glyph in a colour (`icon_style`) — how it stands is its word's. Text
/// glyphs one column wide in every font and every width table, ratatui's and
/// tmux's alike, so the title after them always starts in the same column;
/// emoji were wide, loud, and not always agreed on.
pub const AGENT: &str = "✦";
/// The prompt's own caret, in the prompt's green.
pub const SHELL: &str = "❯";
pub const COMMAND: &str = "▸";
/// A flow, as its row and its chips wear it.
pub const FLOW: &str = "↻";
/// A question waiting on you.
pub const QUESTION: &str = "?";

/// An icon's colour: a shell the prompt's green, a question amber, a command
/// and a flow quiet, an agent its CLI's colour (`cli`) or the plain
/// foreground. Anything that has ended is quiet too — its word says how it
/// ended.
pub fn icon_style(icon: &str, cli: Option<&str>, ended: bool) -> ratatui::style::Style {
    use crate::tui::theme;
    use ratatui::style::{Color, Style};
    if ended {
        return theme::dim();
    }
    match icon {
        SHELL => theme::accent(),
        QUESTION => theme::needs(),
        COMMAND | FLOW => theme::dim(),
        _ => match cli.and_then(|c| looks().get(c)).and_then(|l| l.color) {
            Some((r, g, b)) => Style::default().fg(Color::Rgb(r, g, b)),
            None => Style::default(),
        },
    }
}

/// How a CLI's rows look: its glyph and its colour, as its profile has them.
struct Look {
    icon: Option<String>,
    color: Option<(u8, u8, u8)>,
}

/// The profiles: the built-ins with yours in `$RELAY_HOME/detect/` over
/// them, read once (the built-ins alone under test). Profiles that do not
/// load leave the built-ins — `rld` is the one that says what is wrong
/// with them.
pub(crate) fn registry() -> &'static rl_detect::Registry {
    use rl_detect::Registry;
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        if cfg!(test) {
            Registry::builtin()
        } else {
            rl_core::Home::resolve()
                .ok()
                .and_then(|h| Registry::load(&h).ok())
                .unwrap_or_else(Registry::builtin)
        }
    })
}

/// What `kind:` takes on the list: `query::KINDS`, and each agent's name
/// (`kind:codex`: its runs and its own sessions).
pub fn list_kinds() -> Vec<String> {
    crate::tui::query::KINDS
        .iter()
        .map(|k| k.to_string())
        .chain(registry().agents().map(|p| p.name.clone()))
        .collect()
}

/// Every CLI's look, by name, as its profile (`registry`) has it.
fn looks() -> &'static BTreeMap<String, Look> {
    static LOOKS: OnceLock<BTreeMap<String, Look>> = OnceLock::new();
    LOOKS.get_or_init(|| {
        registry()
            .agents()
            .map(|p| {
                let look = Look {
                    icon: p.icon.clone(),
                    color: p.color,
                };
                (p.name.clone(), look)
            })
            .collect()
    })
}

/// A shell: an interactive one by its profile, or a project's warm shell by
/// its tag, whatever its argv resolved to.
pub fn is_shell(r: &Run) -> bool {
    r.is_shell()
}

/// A shell that is simply there to be typed in. One that ended, failed or
/// wants you is news like any other run and is left to read as one.
pub fn live_shell(r: &Run) -> bool {
    is_shell(r) && !r.state.is_terminal() && !matches!(r.state, RunState::NeedsInput | RunState::Blocked)
}

/// What a run's row says in its summary column. A Claude or Codex run that is
/// waiting — on you, or for nothing, or ended — says its latest recap, which
/// is what you come back for; at work, what it is doing.
pub fn said(i: &DigestItem) -> &str {
    match &i.recap {
        Some(r) if !matches!(i.run.state, RunState::Starting | RunState::Working) => r,
        _ => &i.summary,
    }
}

/// A run's icon: an agent — in its CLI's own glyph, where its profile has
/// one — a shell, or a command (a plugin is one).
pub fn icon(r: &Run) -> &'static str {
    if is_shell(r) {
        return SHELL;
    }
    match r.kind {
        RunKind::Agent => r
            .cli
            .as_deref()
            .and_then(|c| looks().get(c)?.icon.as_deref())
            .unwrap_or(AGENT),
        RunKind::Shell => SHELL,
        RunKind::Command | RunKind::Plugin => COMMAND,
    }
}

/// The icon of one of an agent's own sessions: its agent's glyph, as its
/// runs wear it.
pub fn outside_icon(c: &Outside) -> &'static str {
    looks().get(&c.agent).and_then(|l| l.icon.as_deref()).unwrap_or(AGENT)
}

/// Whether one of an agent's own sessions has ended, so its icon is quiet.
pub fn outside_ended(c: &Outside) -> bool {
    c.run_state().is_terminal()
}

/// The word in the state column. A shell at rest says nothing there — it is
/// idle whenever you are not typing, and its icon already says what it is.
pub fn run_words(r: &Run) -> String {
    if live_shell(r) {
        String::new()
    } else {
        state_words(r.state, r.confidence)
    }
}

/// A header's place in the columns it has: the folder above, dim; the folder
/// itself, bold; and, when it goes by a name of its own, that name — ` · api`.
/// A path too long for the room loses its start to `…`, a whole folder at a
/// time where it can; the folder itself is never cut. `(parent, last, own)`.
pub fn place_parts(place: &str, own: Option<&str>, room: usize) -> (String, String, String) {
    let (parent, last) = match place.rfind('/') {
        Some(i) if i + 1 < place.len() => (&place[..=i], &place[i + 1..]),
        _ => ("", place),
    };
    let own = own.map(|n| format!(" · {n}")).unwrap_or_default();
    let n = |s: &str| s.chars().count();
    let need = n(last) + n(&own);
    if n(parent) + need <= room {
        return (parent.to_string(), last.to_string(), own);
    }
    // The parent gives way first, from the left.
    if need + 2 <= room {
        let keep = room - need - 1;
        let tail: String = parent.chars().skip(n(parent) - keep).collect();
        let tail = match tail.find('/') {
            Some(i) if i > 0 => tail[i..].to_string(),
            _ => tail,
        };
        return (format!("…{tail}"), last.to_string(), own);
    }
    // Then the name; the folder stands whole, whatever the room.
    let own_room = room.saturating_sub(n(last));
    let own = if own_room >= 5 {
        fit(&own, own_room).trim_end().to_string()
    } else {
        String::new()
    };
    (String::new(), last.to_string(), own)
}

/// `s` in exactly `w` columns as a terminal shows it: padded, or cut with
/// `…`. Columns, not chars: a wide glyph (CJK, most emoji) takes two, and
/// one that would straddle the cut gives way to a space.
pub fn fit(s: &str, w: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let n = s.width();
    if n <= w {
        return format!("{s}{}", " ".repeat(w - n));
    }
    if w == 0 {
        return String::new();
    }
    let mut t = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w - 1 {
            break;
        }
        used += cw;
        t.push(c);
    }
    t.push('…');
    t.push_str(&" ".repeat(w - 1 - used));
    t
}

/// A flow's runs, folded into one row: what the flow is, how it has been
/// going, and where its latest firing stands. `runs` is newest first.
#[derive(Clone)]
pub struct FlowRow<'a> {
    /// `f:<name>`.
    pub key: String,
    pub name: String,
    /// The flow's full run history (not just what a filter happened to
    /// match), newest first. Empty for a flow with a yes that has not run
    /// that the digest knows of: its row is there from its yes on.
    pub runs: Vec<&'a DigestItem>,
    /// `every 30m`, `on run.failed` — when the flow is known.
    pub when: Option<String>,
    /// Its latest firing, when the digest has it.
    pub firing: Option<&'a Firing>,
}

impl<'a> FlowRow<'a> {
    /// Its newest run; none before it has run.
    pub fn last(&self) -> Option<&'a DigestItem> {
        self.runs.first().copied()
    }
    pub fn ok(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Finished).count()
    }
    pub fn failed(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Failed).count()
    }
    pub fn stopped(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Stopped).count()
    }
    pub fn unseen(&self) -> bool {
        self.runs.iter().any(|i| i.unseen)
    }
    /// Whether its last run failed.
    pub fn failed_last(&self) -> bool {
        self.last().is_some_and(|i| i.run.state == RunState::Failed)
    }
    /// What the last run came to, in its own words — `5 feedback rows to
    /// process`, or `exit 1 · 2s` where it failed. The cadence and the tally
    /// are the pane's; the marks say how it has been going. Nothing before
    /// it has run.
    pub fn summary(&self) -> String {
        self.last().map(|i| i.summary.clone()).unwrap_or_default()
    }
    /// One mark per firing — a firing's runs share its id as their chain — oldest first, at
    /// most `n` of the newest: `·` went well, `×` a step failed, `○` one was stopped.
    pub fn marks(&self, n: usize) -> String {
        let worst = |m: char| match m {
            '×' => 2,
            '○' => 1,
            _ => 0,
        };
        let mut firings: Vec<(&str, char)> = Vec::new();
        for i in &self.runs {
            let m = match i.run.state {
                RunState::Failed => '×',
                RunState::Stopped => '○',
                _ => '·',
            };
            match firings.iter_mut().find(|(chain, _)| *chain == i.run.chain) {
                Some((_, was)) if worst(m) > worst(*was) => *was = m,
                Some(_) => {}
                None => firings.push((&i.run.chain, m)),
            }
        }
        firings.iter().take(n).rev().map(|(_, m)| *m).collect()
    }
}

/// Which flow an ended run folds into, if any.
pub fn flow_of(r: &Run) -> Option<&str> {
    if !r.state.is_terminal() {
        return None;
    }
    match &r.cause {
        Cause::Flow { flow, .. } => Some(flow),
        _ => None,
    }
}

/// A flow's row key.
pub fn flow_key(name: &str) -> String {
    format!("f:{name}")
}

/// Whether a flow has a row of its own, run or not: every flow defined now —
/// waiting for its first yes, on, paused, or changed since — until it is
/// removed. Its row is where you see it and act on it, not a header chip.
fn stands_on_the_list(f: &Flow) -> bool {
    f.removed_at.is_none()
}

/// Whether a flow's row is hidden — by its own `/hide` or its project's —
/// by the keys `hidden_since` reads for its runs.
fn flow_hidden(name: &str, flows: &[Flow], hidden: &[Hidden]) -> bool {
    let keys = [head_key(&flow_group(flows, name)), flow_key(name)];
    hidden.iter().any(|h| keys.contains(&h.key))
}

/// The group one of an agent's own sessions stands in, as `(key, folder)`:
/// the folder `rld` filed it under, as a run started in its `cwd` would be
/// (`workspace`). Where `rld` has not said, a guess: the registered folder
/// its `cwd` is in, the deepest where they nest, else the `cwd` itself.
fn outside_group(s: &Outside, names: &Names) -> (String, PathBuf) {
    let folder = s
        .workspace
        .clone()
        .or_else(|| names.containing(&s.cwd))
        .unwrap_or_else(|| s.cwd.clone());
    (folder.to_string_lossy().into_owned(), folder)
}

/// What hides one of an agent's own sessions — its own `/hide` or its
/// project's — and since when: the later of them. `None`: it is on the list.
fn outside_hidden_since(s: &Outside, names: &Names, hidden: &[Hidden]) -> Option<Timestamp> {
    let keys = [head_key(&outside_group(s, names)), s.key()];
    hidden.iter().filter(|h| keys.contains(&h.key)).map(|h| h.at).max()
}

/// Workspace paths → the names they were registered under.
#[derive(Default, Clone)]
pub struct Names(HashMap<PathBuf, String>);

impl Names {
    pub fn new(ws: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        Self(ws.into_iter().collect())
    }

    /// Every registered folder, by path.
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.0.keys().cloned().collect();
        v.sort();
        v
    }

    /// The registered folder `p` is in, or is: the deepest where they nest.
    pub fn containing(&self, p: &Path) -> Option<PathBuf> {
        self.0
            .keys()
            .filter(|w| p.starts_with(w))
            .max_by_key(|w| w.as_os_str().len())
            .cloned()
    }

    /// The name a folder was registered under, if it was.
    pub fn registered(&self, p: &Path) -> Option<String> {
        self.0.get(p).cloned()
    }

    /// A workspace by its registered name, else its last path component —
    /// except your home folder, which is `home`, not your login.
    pub fn workspace(&self, p: &Path) -> String {
        if let Some(n) = self.0.get(p) {
            return n.clone();
        }
        if std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == p) {
            return "home".into();
        }
        p.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Where a group's work is: the folder, with `~` for home. A group named
    /// for a ref or a branch still says which folder that was in.
    pub fn place(&self, key: &str, workspace: &Path) -> String {
        if key.starts_with('/') {
            short_path(Path::new(key))
        } else {
            short_path(workspace)
        }
    }

    /// A group's name, where it is not just its folder's: a ref, or the name
    /// a folder was registered or renamed under when that differs.
    pub fn own(&self, key: &str, workspace: &Path) -> Option<String> {
        if !key.starts_with('/') {
            return Some(key.to_string());
        }
        let name = self.registered(Path::new(key))?;
        let last = workspace.file_name().map(|s| s.to_string_lossy().into_owned());
        (last.as_deref() != Some(name.as_str())).then_some(name)
    }

    /// A group key in words: a ref or branch as it is, a path by name.
    pub fn group(&self, key: &str) -> String {
        if key.starts_with('/') {
            self.workspace(Path::new(key))
        } else {
            key.to_string()
        }
    }
}

/// A path with `~` for home — only for home itself and what is under it, not
/// a sibling that happens to share its start.
pub fn short_path(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) if !h.as_os_str().is_empty() => match p.strip_prefix(&h) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.to_string_lossy()),
            Err(_) => p.to_string_lossy().into_owned(),
        },
        _ => p.to_string_lossy().into_owned(),
    }
}

/// Where a shell is, as its row says it: inside its project, the way down
/// from the project's folder (`app/src`); at the project's folder, nothing;
/// anywhere else, the whole path with `~` for home.
pub fn shell_place(cwd: &Path, project: &Path) -> String {
    match cwd.strip_prefix(project) {
        Ok(rest) => rest.to_string_lossy().into_owned(),
        Err(_) => short_path(cwd),
    }
}

/// Why a run started, in words.
pub fn cause_words(c: &Cause) -> String {
    match c {
        Cause::Manual => "started by hand".into(),
        Cause::Decision(_) => "your answer".into(),
        Cause::Flow { flow, step } => format!("flow {flow} · {step}"),
        Cause::Assistant(job) => format!("asked, to {job}"),
    }
}

/// The firing a question is a flow's ask in: the one whose id is its chain.
pub fn firing_of<'a>(d: &Decision, firings: &'a [Firing]) -> Option<&'a Firing> {
    firings.iter().find(|f| f.id == d.chain)
}

/// Who asked a question, in words: the run that asked; else the flow whose
/// firing it is, and the step that asks (`flow prod-logs · review`); else you.
pub fn asked_by(d: &Decision, run: Option<&Run>, firing: Option<&Firing>) -> String {
    if let Some(r) = run {
        return r.title.clone();
    }
    let Some(f) = firing else {
        return "you".into();
    };
    let step = f
        .steps
        .iter()
        .find(|s| s.decision.as_deref() == Some(d.id.as_str()))
        .or_else(|| f.steps.iter().find(|s| s.status == rl_proto::flow::StepStatus::Waiting));
    match step {
        Some(s) => format!("flow {} · {}", f.flow, s.name),
        None => format!("flow {}", f.flow),
    }
}

struct Group<'a> {
    key: String,
    /// The folder of whatever came first; a group's members share one.
    workspace: PathBuf,
    runs: Vec<&'a DigestItem>,
    flows: Vec<FlowRow<'a>>,
    outside: Vec<&'a Outside>,
    /// The questions asked in it, each with the key of the row it stands
    /// under when that row is there.
    asked: Vec<Asked<'a>>,
}

/// A question, and the key of the row that asked it: its run's, or the
/// row of the flow that run folded into, or its firing's flow's.
type Asked<'a> = (Option<String>, &'a Decision);

/// What the list is built from besides the digest.
pub struct Context<'a> {
    pub names: &'a Names,
    /// Every flow, for where its row stands and what starts it; may be empty.
    pub flows: &'a [Flow],
    pub filter: &'a str,
    /// The list toggles. Flows, agents and shells are additive — each on
    /// shows the runs it is for; off hides them. Hidden is off by default;
    /// on, what you hid stands inline in its place. Questions ignore them all.
    pub show_flows: bool,
    pub show_agents: bool,
    pub show_shells: bool,
    pub show_hidden: bool,
    pub now: Timestamp,
    /// The Relay home: an assistant's run there — a summary of everything —
    /// is Relay's own, and makes no group on the list.
    pub relay: Option<&'a Path>,
}

/// What a search matches a run on: its words, its project, what state it is
/// in, what it is, and the flow behind it.
fn run_subject<'a>(i: &'a DigestItem, project: &'a str) -> Subject<'a> {
    let r = &i.run;
    let mut is = vec![match r.state {
        RunState::Failed => "failed",
        RunState::NeedsInput | RunState::Blocked => "needs",
        RunState::Finished => "done",
        RunState::Stopped => "stopped",
        RunState::Starting | RunState::Working | RunState::Stale | RunState::Idle => "going",
    }];
    if r.pinned_at.is_some() {
        is.push("pinned");
    }
    if i.run.hidden_at.is_some() {
        is.push("hidden");
    }
    if i.unseen {
        is.push("new");
    }
    let mut kind = vec![match r.kind {
        RunKind::Agent => "agent",
        RunKind::Shell => "shell",
        RunKind::Command | RunKind::Plugin => "command",
    }];
    // `kind:<agent>`: that agent, whoever started it.
    if r.kind == RunKind::Agent
        && let Some(cli) = r.cli.as_deref()
    {
        kind.push(cli);
    }
    let by = match &r.cause {
        Cause::Flow { flow, .. } => {
            kind.push("flow");
            Some(flow.as_str())
        }
        _ => None,
    };
    Subject {
        text: vec![&r.title, r.cli.as_deref().unwrap_or(""), said(i)],
        project: vec![project, r.workspace.to_str().unwrap_or("")],
        is,
        kind,
        by: by.into_iter().collect(),
        on: r.branch.as_deref(),
    }
}

/// What a search matches one of an agent's own sessions on: its name, where
/// it lives (`lives`), its project and folder, how it stands as a run
/// would, and that it is an agent, and whose (`kind:claude`, `kind:codex`).
fn outside_subject<'a>(s: &'a Outside, project: &'a str, lives: &'a str, hidden: bool) -> Subject<'a> {
    let mut is = vec![outside::is_word(s)];
    if hidden {
        is.push("hidden");
    }
    Subject {
        text: vec![&s.name, &s.session_id, lives],
        project: vec![project, s.cwd.to_str().unwrap_or("")],
        is,
        kind: vec!["agent", &s.agent],
        ..Subject::default()
    }
}

/// A group's name: `everywhere`, or its ref or folder in words.
fn group_name(key: &str, names: &Names) -> String {
    if key == EVERYWHERE_KEY {
        EVERYWHERE.to_string()
    } else {
        names.group(key)
    }
}

/// A row as the search ranks it (`query::ranked`): a header by its name, the
/// rest by title, project and summary; a flow that has not run, by the
/// group its flow (in `flows`) stands in.
pub fn named(row: &Row, d: &Digest, names: &Names, flows: &[Flow]) -> Named {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    match row {
        Row::Group(h) => Named {
            head: h.name.clone(),
            rest: vec![h.place.clone(), path(&h.workspace)],
            ..Named::default()
        },
        Row::Question(q) => {
            let asker = q.run.as_deref().and_then(|id| d.items.iter().find(|i| i.run.id == id));
            let key = asker.map_or_else(|| path(&q.workspace), |i| i.run.place_key());
            Named {
                title: q.title.clone(),
                project: names.group(&key),
                summary: q.summary.clone().unwrap_or_default(),
                rest: vec![path(&q.workspace)],
                ..Named::default()
            }
        }
        Row::Run(i) => Named {
            title: i.run.title.clone(),
            project: names.group(&i.run.place_key()),
            summary: said(i).to_string(),
            rest: vec![i.run.cli.clone().unwrap_or_default(), path(&i.run.workspace)],
            ..Named::default()
        },
        Row::Flow(f) => match f.last() {
            Some(i) => {
                let r = &i.run;
                Named {
                    title: f.name.clone(),
                    project: names.group(&r.place_key()),
                    summary: f.summary(),
                    rest: vec![r.title.clone(), r.cli.clone().unwrap_or_default(), path(&r.workspace)],
                    ..Named::default()
                }
            }
            None => Named {
                title: f.name.clone(),
                project: group_name(&flow_group(flows, &f.name).0, names),
                summary: f.when.clone().unwrap_or_default(),
                ..Named::default()
            },
        },
        Row::Outside(c) => Named {
            title: c.title(),
            project: names.group(&outside_group(c, names).0),
            summary: outside::lives(c),
            rest: vec![path(&c.cwd)],
            ..Named::default()
        },
    }
}

/// Everything on the list, in order, narrowed to what the filter — a search,
/// see `query` — hits.
pub fn build<'a>(d: &'a Digest, cx: &Context<'_>) -> Vec<Row<'a>> {
    let names = cx.names;
    let now = cx.now;
    let query = Query::parse(cx.filter);
    let mut groups: Vec<Group<'a>> = Vec::new();
    let group_at = |key: String, workspace: &Path, groups: &mut Vec<Group<'a>>| -> usize {
        match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    workspace: workspace.to_path_buf(),
                    runs: Vec::new(),
                    flows: Vec::new(),
                    outside: Vec::new(),
                    asked: Vec::new(),
                });
                groups.len() - 1
            }
        }
    };
    let when_of = |name: &str| -> Option<String> {
        cx.flows
            .iter()
            .find(|f| f.name == name)
            .map(|f| flows::when_words(&f.when()))
    };
    let run_by_id = |id: &str| d.items.iter().map(|i| &i.run).find(|r| r.id == id);
    // A flow's remembered history must be its FULL history, independent of
    // the filter — the filter only decides whether the flow's row is shown at
    // all. So flows are accumulated here first, unconditionally, and folded
    // into their group afterwards only if the flow or one of its own runs hit.
    struct FlowAcc<'a> {
        key: String,
        name: String,
        group_key: String,
        workspace: PathBuf,
        runs: Vec<&'a DigestItem>,
        hit: bool,
    }
    let mut flows_acc: Vec<FlowAcc<'a>> = Vec::new();
    // Pinned runs stand above everything, however old, whatever the toggles —
    // kept on purpose, so neither a day nor a toggle takes them away. Only the
    // filter does, as with anything else.
    let mut pinned: Vec<&'a DigestItem> = Vec::new();
    // What you hid is off the list unless `show_hidden` brings it back inline,
    // in its place; what of it has come to need you since comes through, above
    // the projects, whenever hidden is off.
    // By how it ranks, then since when it has needed you, then its key.
    let mut through: Vec<(u8, Option<Timestamp>, String, Row<'a>)> = Vec::new();
    // Per project (by its group key), how many of its items need you but are
    // off the list now — hidden and not come through, or a toggle dropped
    // them. Its header hints at them, so nothing urgent is silently buried.
    let mut buried: HashMap<String, usize> = HashMap::new();
    for i in &d.items {
        let r = &i.run;
        // A summary of everything runs in the Relay home: Relay's doing, not
        // a project of yours. `rl ls --all` and the events have it.
        if matches!(r.cause, Cause::Assistant(_)) && cx.relay.is_some_and(|h| r.cwd == h) {
            continue;
        }
        if let Some(since) = hidden_since(r, names, cx.flows, &d.hidden) {
            if !cx.show_hidden {
                if comes_through(r, since)
                    && keeps(r, cx.show_agents, cx.show_flows, cx.show_shells)
                    && query.hits(&run_subject(i, &names.group(&r.place_key())))
                {
                    through.push((rank::score(r), Some(r.state_at), r.id.clone(), Row::Run(i)));
                } else if !comes_through(r, since) && r.state.needs_attention() {
                    // Hidden while it already needed you, so it does not come
                    // through: its project hints it is there.
                    *buried.entry(group_of(r, names, cx.flows).0).or_insert(0) += 1;
                }
                continue;
            }
            // Hidden on: the run stands inline, in its place.
        }
        if r.pinned_at.is_some() {
            if query.hits(&run_subject(i, &names.group(&r.place_key()))) {
                pinned.push(i);
            }
            continue;
        }
        let live = !r.state.is_terminal() || i.unseen || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT);
        // A flow with a row of its own keeps all the history the digest
        // has of it, however old: its row is there anyway.
        let kept = flow_of(r).is_some_and(|name| cx.flows.iter().any(|f| f.name == name && stands_on_the_list(f)));
        if !live && !kept {
            continue;
        }
        if !keeps(r, cx.show_agents, cx.show_flows, cx.show_shells) {
            // A toggle takes it off the list; if it needs you, its project
            // hints it is still there.
            if r.state.needs_attention() {
                *buried.entry(group_of(r, names, cx.flows).0).or_insert(0) += 1;
            }
            continue;
        }
        let (key, workspace) = group_of(r, names, cx.flows);
        let name = group_name(&key, names);
        if let Some(flow) = flow_of(r) {
            let mut subject = run_subject(i, &name);
            subject.text.push(flow);
            let item_hit = query.hits(&subject);
            let fkey = flow_key(flow);
            match flows_acc.iter_mut().find(|acc| acc.key == fkey) {
                Some(acc) => {
                    acc.runs.push(i);
                    acc.hit = acc.hit || item_hit;
                }
                None => flows_acc.push(FlowAcc {
                    key: fkey,
                    name: flow.to_string(),
                    group_key: key,
                    workspace,
                    runs: vec![i],
                    hit: item_hit,
                }),
            }
            continue;
        }
        if !query.hits(&run_subject(i, &name)) {
            continue;
        }
        let g = group_at(key, &workspace, &mut groups);
        groups[g].runs.push(i);
    }
    // A flow with a yes has its row whether or not it has run — what starts
    // it and when it fires next, before it ever did — when flows are on, and
    // hidden as its runs would be.
    if cx.show_flows {
        for f in cx.flows.iter().filter(|f| stands_on_the_list(f)) {
            let key = flow_key(&f.name);
            if flows_acc.iter().any(|acc| acc.key == key)
                || (flow_hidden(&f.name, cx.flows, &d.hidden) && !cx.show_hidden)
            {
                continue;
            }
            let (group_key, workspace) = flow_group(cx.flows, &f.name);
            let project = group_name(&group_key, names);
            let hit = query.hits(&Subject {
                text: vec![&f.name],
                project: vec![&project, workspace.to_str().unwrap_or("")],
                kind: vec!["flow"],
                by: vec![&f.name],
                ..Subject::default()
            });
            flows_acc.push(FlowAcc {
                key,
                name: f.name.clone(),
                group_key,
                workspace,
                runs: Vec::new(),
                hit,
            });
        }
    }
    // Agents' own sessions, under their folder's project: agents, so shown
    // when agents are on; hidden as a flow is, and come through as a run does.
    if cx.show_agents {
        for c in &d.outside {
            let since = outside_hidden_since(c, names, &d.hidden);
            let hidden = since.is_some();
            let (key, workspace) = outside_group(c, names);
            let project = names.group(&key);
            let lives = outside::lives(c);
            let hits = || query.hits(&outside_subject(c, &project, &lives, hidden));
            match since {
                Some(since) if !cx.show_hidden => {
                    if outside::comes_through(c, since) && hits() {
                        through.push((outside::score(c), c.state_at, c.key(), Row::Outside(c)));
                    } else if !outside::comes_through(c, since) && c.run_state().needs_attention() {
                        *buried.entry(key).or_insert(0) += 1;
                    }
                    continue;
                }
                _ => {}
            }
            if !hits() {
                continue;
            }
            let g = group_at(key, &workspace, &mut groups);
            groups[g].outside.push(c);
        }
    } else {
        // Agents off: an agent's own session that needs you is off the list,
        // so its project hints it is there.
        for c in &d.outside {
            if c.run_state().needs_attention() {
                *buried.entry(outside_group(c, names).0).or_insert(0) += 1;
            }
        }
    }
    // Fold each flow into its group now, with its full history, but only if
    // the flow (or one of its own runs) actually matched the filter.
    for acc in flows_acc {
        if !acc.hit {
            continue;
        }
        let g = group_at(acc.group_key, &acc.workspace, &mut groups);
        groups[g].flows.push(FlowRow {
            key: acc.key,
            when: when_of(&acc.name),
            firing: d.firings.iter().find(|f| f.flow == acc.name),
            name: acc.name,
            runs: acc.runs,
        });
    }
    // A question stands in its project, under the row that asked it — or,
    // where that run is pinned or has come through, under it there. It
    // ignores the toggles: it is what needs you, whatever you are showing.
    let banded: HashSet<&str> = pinned
        .iter()
        .copied()
        .chain(through.iter().filter_map(|(.., row)| row.item()))
        .map(|i| i.run.id.as_str())
        .collect();
    let mut band_asked: Vec<Asked<'a>> = Vec::new();
    for q in &d.decisions {
        let asker = q.run.as_deref().and_then(run_by_id);
        let (key, workspace) = match asker {
            Some(r) => group_of(r, names, cx.flows),
            None => (q.workspace.to_string_lossy().into_owned(), q.workspace.clone()),
        };
        let name = group_name(&key, names);
        // A flow's question is its firing's: the chain is the firing.
        let firing = firing_of(q, &d.firings).map(|f| f.flow.as_str());
        let by = firing.or(asker.and_then(|r| match &r.cause {
            Cause::Flow { flow, .. } => Some(flow.as_str()),
            _ => None,
        }));
        let subject = Subject {
            text: vec![&q.title, q.summary.as_deref().unwrap_or("")],
            project: vec![&name, q.workspace.to_str().unwrap_or("")],
            is: vec!["needs"],
            kind: vec!["question"],
            by: by.into_iter().collect(),
            on: asker.and_then(|r| r.branch.as_deref()),
        };
        if !query.hits(&subject) {
            continue;
        }
        let under = match asker {
            Some(r) => Some(flow_of(r).map_or_else(|| format!("r:{}", r.id), flow_key)),
            None => firing.map(flow_key),
        };
        if asker.is_some_and(|r| banded.contains(r.id.as_str())) {
            band_asked.push((under, q));
            continue;
        }
        let g = group_at(key, &workspace, &mut groups);
        groups[g].asked.push((under, q));
    }
    for g in &mut groups {
        for f in &mut g.flows {
            f.runs.sort_by_key(|i| std::cmp::Reverse(i.at));
        }
    }
    // Which folders already have a line, so idle projects don't double up.
    let shown: HashSet<PathBuf> = groups.iter().map(|g| g.workspace.clone()).collect();
    pinned.sort_by_key(|i| (i.run.pinned_at, i.run.id.clone()));
    through.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let band: Vec<Row<'a>> = through
        .into_iter()
        .map(|(_, _, _, row)| row)
        .chain(pinned.into_iter().map(Row::Run))
        .collect();
    let mut rows = attach(band, band_asked);
    // Each block is a project's header and what is under it.
    let mut blocks: Vec<(String, String, Vec<Row<'a>>)> = Vec::new();
    let mut everywhere: Vec<Row<'a>> = Vec::new();
    for g in groups {
        let name = names.group(&g.key);
        let head = if g.key == EVERYWHERE_KEY {
            Head::everywhere()
        } else {
            let mut h = head(&g.key, &g.workspace, d, cx);
            h.attention = buried.get(&g.key).copied().unwrap_or(0);
            h
        };
        let mut block = vec![Row::Group(head)];
        // Runs, flows and agents' own sessions share one order, which nothing
        // that happens to a row changes: the order they came in, the newest at
        // the bottom.
        let mut mixed: Vec<Row<'a>> = g.runs.into_iter().map(Row::Run).collect();
        mixed.extend(g.flows.into_iter().map(Row::Flow));
        mixed.extend(g.outside.into_iter().map(Row::Outside));
        mixed.sort_by_cached_key(came);
        // Once you have put a project's rows in order, it keeps yours: the
        // rows stand where you put them, and what is new enters at the bottom.
        let kept = block[0]
            .key()
            .and_then(|k| d.order.iter().find(|o| o.group == k))
            .filter(|o| !o.rows.is_empty());
        let mixed = match kept {
            Some(o) => in_order(mixed, &o.rows),
            None => mixed,
        };
        block.extend(attach(mixed, g.asked));
        if g.key == EVERYWHERE_KEY {
            everywhere = block;
        } else {
            blocks.push((name.to_lowercase(), g.key, block));
        }
    }
    // A registered folder is always on the list, whatever the toggles, even
    // before anything has run there — so you can select it, read its stats,
    // see its flows or open its shell. The toggles narrow what is shown inside
    // projects, never which projects there are; only the filter takes them
    // away — and `/hide`, which puts a project's header off the list, back
    // inline only when hidden is on.
    let hidden_heads: Vec<&str> = d.hidden.iter().filter_map(|h| h.key.strip_prefix("g:")).collect();
    let heads: Vec<PathBuf> = if cx.show_hidden {
        // Every registered folder, the hidden ones among them, plus any hidden
        // folder that is not registered.
        let mut hs = names.paths();
        for k in &hidden_heads {
            if *k != EVERYWHERE {
                let p = PathBuf::from(*k);
                if !hs.contains(&p) {
                    hs.push(p);
                }
            }
        }
        hs
    } else {
        names
            .paths()
            .into_iter()
            .filter(|p| !hidden_heads.iter().any(|k| Path::new(k) == p))
            .collect()
    };
    if cx.show_flows
        && cx.show_hidden
        && everywhere.is_empty()
        && hidden_heads.contains(&EVERYWHERE)
        && query.hits_project(&[EVERYWHERE], None)
    {
        everywhere = vec![Row::Group(Head::everywhere())];
    }
    for p in heads {
        if shown.contains(&p) {
            continue;
        }
        let place = names.place(&p.to_string_lossy(), &p);
        let name = names.workspace(&p);
        let checkout = checkout_of(&d.checkouts, &p);
        if !query.hits_project(
            &[&name, &place, p.to_str().unwrap_or("")],
            checkout.and_then(|c| c.branch.as_deref()),
        ) {
            continue;
        }
        let mut h = head(&p.to_string_lossy(), &p, d, cx);
        h.attention = buried.get(&p.to_string_lossy().into_owned()).copied().unwrap_or(0);
        blocks.push((
            name.to_lowercase(),
            p.to_string_lossy().into_owned(),
            vec![Row::Group(h)],
        ));
    }
    // Projects in the order you put them (shift-↑/↓ on a header), the rest
    // after them by name — a project new to the list enters at the bottom —
    // so each keeps its place as work comes and goes: what needs you says
    // so with its marks and in the headline, not by jumping the queue.
    let projects = d.order.iter().find(|o| o.group == Order::PROJECTS);
    let place = |block: &[Row]| {
        let key = block.first().and_then(Row::key)?;
        projects?.rows.iter().position(|k| *k == key)
    };
    blocks.sort_by_cached_key(|(name, key, block)| (place(block).unwrap_or(usize::MAX), name.clone(), key.clone()));
    rows.extend(blocks.into_iter().flat_map(|(_, _, block)| block));
    // The flows with no `in` are said in the headline (`everywhere`). Their
    // rows, and what flows ran outside every project, are gathered after the
    // projects, under one `everywhere`, rather than under whatever folder that
    // was — all of it flow content, so only when flows are on.
    if cx.show_flows {
        rows.extend(everywhere);
    }
    rows
}

/// A project's header: the group `key` (a ref, or a folder), whose work is
/// in `workspace`.
pub fn head(key: &str, workspace: &Path, d: &Digest, cx: &Context<'_>) -> Head {
    let names = cx.names;
    Head {
        place: names.place(key, workspace),
        workspace: workspace.to_path_buf(),
        name: names.group(key),
        own: names.own(key, workspace),
        flows: chips(cx.flows, Some(workspace), cx.now),
        checkout: checkout_of(&d.checkouts, workspace).cloned(),
        attention: 0,
    }
}

/// The amber hint on a project's header: how many of its items need you but
/// are off the list now — hidden, or a toggle dropped them. `! 2 need you`;
/// nothing when none are buried, so the header stays quiet until something is.
pub fn needs_hint(attention: usize) -> Option<String> {
    (attention > 0).then(|| format!("! {attention} need{} you", if attention == 1 { "s" } else { "" }))
}

/// Whether a row wants you: it asks, is blocked, or failed — a flow whose
/// last run failed too. On the list it stays where it stands and says so;
/// the catch-up puts it first in its project.
pub fn wants_you(row: &Row) -> bool {
    let needs = |s: RunState| matches!(s, RunState::NeedsInput | RunState::Blocked | RunState::Failed);
    match row {
        Row::Run(i) => needs(i.run.state),
        Row::Flow(f) => f.last().is_some_and(|i| needs(i.run.state)),
        Row::Outside(c) => needs(c.run_state()),
        Row::Group(_) | Row::Question(_) => false,
    }
}

/// Where a row stands in a project you have not put in order: by when it came — a
/// run when it started, a flow when the oldest run the list has of it
/// started — the newest at the bottom; a flow that has not run after them,
/// by name. Nothing that happens to a row afterwards changes it.
fn came(row: &Row) -> (bool, Option<Timestamp>, String) {
    match row {
        Row::Run(i) => (false, Some(i.run.started_at), i.run.id.clone()),
        Row::Flow(f) => {
            let first = f.runs.iter().map(|i| i.run.started_at).min();
            (first.is_none(), first, f.name.clone())
        }
        Row::Outside(c) => (c.started_at.is_none(), c.started_at, c.key()),
        Row::Group(_) | Row::Question(_) => (true, None, String::new()),
    }
}

/// A project's rows, in the order they came, put in the order kept for it:
/// the rows it knows where it has them, then what it does not know yet —
/// new since — at the bottom, in the order they came.
fn in_order<'a>(came: Vec<Row<'a>>, kept: &[String]) -> Vec<Row<'a>> {
    let place = |r: &Row| r.key().and_then(|k| kept.iter().position(|x| *x == k));
    let (mut placed, new): (Vec<Row<'a>>, Vec<Row<'a>>) = came.into_iter().partition(|r| place(r).is_some());
    placed.sort_by_cached_key(|r| place(r));
    placed.into_iter().chain(new).collect()
}

/// Each question right under the row that asked it, oldest first; one whose
/// row is not among `rows` at the bottom.
fn attach<'a>(rows: Vec<Row<'a>>, mut asked: Vec<Asked<'a>>) -> Vec<Row<'a>> {
    asked.sort_by_key(|(_, q)| (q.created_at, q.id.clone()));
    let keys: HashSet<String> = rows.iter().filter_map(Row::key).collect();
    let (under, loose): (Vec<Asked<'a>>, Vec<Asked<'a>>) = asked
        .into_iter()
        .partition(|(k, _)| k.as_ref().is_some_and(|k| keys.contains(k)));
    let mut out = Vec::with_capacity(rows.len() + under.len() + loose.len());
    for row in rows {
        let key = row.key();
        out.push(row);
        out.extend(under.iter().filter(|(k, _)| *k == key).map(|(_, q)| Row::Question(q)));
    }
    out.extend(loose.into_iter().map(|(_, q)| Row::Question(q)));
    out
}

/// A project's rows as they stand on the list with nothing narrowing it
/// (`full`) — the order `in_order` keeps — but for its questions, which
/// stand under their rows rather than in the order.
fn standing(full: &[Row], group: &str) -> Vec<String> {
    let Some(head) = full.iter().position(|r| r.key().as_deref() == Some(group)) else {
        return Vec::new();
    };
    full[head + 1..]
        .iter()
        .take_while(|r| !matches!(r, Row::Group(_)))
        .filter(|r| !matches!(r, Row::Question(_)))
        .filter_map(Row::key)
        .collect()
}

/// A project's header's key — not `everywhere`'s, which is not one.
fn project_key(row: &Row) -> Option<String> {
    match row {
        Row::Group(h) if !h.is_everywhere() => row.key(),
        _ => None,
    }
}

/// The projects in the order they stand: the order kept for them, what it
/// has that the list does not show now keeping its place, then those it
/// does not know, as the list has them (`full`, nothing narrowing it).
fn projects_standing(full: &[Row], kept: &[Order]) -> Vec<String> {
    let mut keys: Vec<String> = kept
        .iter()
        .find(|o| o.group == Order::PROJECTS)
        .map(|o| o.rows.clone())
        .unwrap_or_default();
    for k in full.iter().filter_map(project_key) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// The order to keep once the row `key` moves one place up (or down) among
/// what is `shown`: its project's whole order, from `full` — the list with
/// nothing narrowing it — so rows a search or a toggle leaves out keep theirs.
/// Rows move only inside their project, past a question as past nothing; a
/// header moves among the projects. A question, a pinned run and
/// `everywhere` stay put, and say why.
pub fn reorder(shown: &[Row], full: &[Row], kept: &[Order], key: &str, up: bool) -> Result<Order, &'static str> {
    let at = shown
        .iter()
        .position(|r| r.key().as_deref() == Some(key))
        .ok_or("nothing here to move")?;
    match &shown[at] {
        Row::Group(h) if h.is_everywhere() => return Err("everywhere stays last, below the projects"),
        Row::Group(_) => return move_project(shown, full, kept, key, up),
        Row::Question(_) => return Err("a question stays under the run that asked it"),
        Row::Run(_) | Row::Flow(_) | Row::Outside(_) => {}
    }
    let Some(head) = shown[..at].iter().rposition(|r| matches!(r, Row::Group(_))) else {
        return Err(if shown[at].item().is_some_and(|i| i.run.pinned_at.is_some()) {
            "a pinned run keeps the order it was pinned in · ^t lets it go back"
        } else {
            "it needs you, so it stays at the top until it is dealt with"
        });
    };
    // The row it passes: the next run or flow, over a question between.
    let step = |i: usize| if up { i.checked_sub(1) } else { Some(i + 1) };
    let mut next = step(at);
    while let Some(Row::Question(_)) = next.and_then(|i| shown.get(i)) {
        next = next.and_then(step);
    }
    let other = match next.and_then(|i| shown.get(i)) {
        Some(r @ (Row::Run(_) | Row::Flow(_) | Row::Outside(_))) => r.key().unwrap_or_default(),
        _ if up => return Err("already at the top of its project"),
        _ => return Err("already at the bottom of its project"),
    };
    let group = shown[head].key().unwrap_or_default();
    let mut rows = standing(full, &group);
    rows.retain(|k| k != key);
    let i = rows
        .iter()
        .position(|k| *k == other)
        .ok_or("the list moved under it · try again")?;
    rows.insert(if up { i } else { i + 1 }, key.to_string());
    Ok(Order { group, rows })
}

/// The projects' order once the project `key` moves one place up (or down)
/// among the projects `shown`, kept whole from `full`.
fn move_project(shown: &[Row], full: &[Row], kept: &[Order], key: &str, up: bool) -> Result<Order, &'static str> {
    let heads: Vec<String> = shown.iter().filter_map(project_key).collect();
    let at = heads.iter().position(|k| k == key).ok_or("nothing here to move")?;
    let next = if up { at.checked_sub(1) } else { Some(at + 1) };
    let other = match next.and_then(|i| heads.get(i)) {
        Some(k) => k.clone(),
        None if up => return Err("already the first project"),
        None => return Err("already the last project"),
    };
    let mut rows = projects_standing(full, kept);
    rows.retain(|k| k != key);
    let i = rows
        .iter()
        .position(|k| *k == other)
        .ok_or("the list moved under it · try again")?;
    rows.insert(if up { i } else { i + 1 }, key.to_string());
    Ok(Order {
        group: Order::PROJECTS.into(),
        rows,
    })
}

/// `(questions, need you, working, finished, flows)` over what the list shows.
/// A flow counts once, and as needing you when its last run failed; one of
/// an agent's own session as a run in its state would.
pub fn counts(rows: &[Row]) -> (usize, usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0, 0);
    let by_state = |s: RunState, c: &mut (usize, usize, usize, usize, usize)| {
        use rl_proto::types::RunState::*;
        match s {
            NeedsInput | Blocked | Failed => c.1 += 1,
            Starting | Working | Stale | Idle => c.2 += 1,
            Finished | Stopped => c.3 += 1,
        }
    };
    for r in rows {
        match r {
            Row::Group(..) => {}
            Row::Question(_) => c.0 += 1,
            Row::Run(i) => by_state(i.run.state, &mut c),
            Row::Outside(s) => by_state(s.run_state(), &mut c),
            // A flow that has not run is not what is moving: its header says it is there.
            Row::Flow(f) if f.runs.is_empty() => {}
            Row::Flow(f) => {
                c.4 += 1;
                if f.failed_last() {
                    c.1 += 1;
                }
            }
        }
    }
    c
}

/// What a restart took that is still on the list, as the headline says it,
/// with the command that picks it up. `/resume all` leaves plain commands
/// out, so when they are all there is, it says the command that takes them.
pub fn lost_note(d: &Digest) -> Option<String> {
    if d.lost.is_empty() {
        return None;
    }
    let command = |id: &String| {
        d.items
            .iter()
            .any(|i| &i.run.id == id && i.run.kind == RunKind::Command)
    };
    let how = if d.lost.iter().all(command) {
        "/resume all commands"
    } else {
        "/resume all"
    };
    Some(format!("{} lost in a restart · {how}", d.lost.len()))
}

/// The flows with no `in`, as the headline says them: how many there are —
/// `↻ 2 everywhere` — and, when any waits for a yes, that it does and where
/// to give it. `chips` are those ones (`chips(flows, None, ..)`); the flag
/// says one is waiting, which is drawn amber. Nothing when there are none.
pub fn everywhere(chips: &[Chip]) -> Option<(String, bool)> {
    if chips.is_empty() {
        return None;
    }
    let waiting = chips.iter().filter(|c| c.needs_yes()).count();
    let mut s = format!("{FLOW} {} everywhere", chips.len());
    let need = |n: usize| if n == 1 { "needs" } else { "need" };
    match waiting {
        0 => {}
        // All of them wait: the count already says how many.
        n if n == chips.len() => s.push_str(&format!(" {} a yes · /flow approve", need(n))),
        n => s.push_str(&format!(" · {n} {} a yes · /flow approve", need(n))),
    }
    Some((s, waiting > 0))
}

/// On a header whose flows did not load — or on `everywhere` when one with
/// no `in`, or not known, did not — in red, before any flow waiting for a
/// yes: the folder is not what is loaded until it is fixed.
pub const BROKEN_CHIP: &str = "! flows";

/// How a piece of the headline is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Amber: yours to give.
    Needs,
    /// Red: broken.
    Bad,
}

/// What the headline says of the flows with no `in`, piece by piece: the
/// tally (`everywhere`), amber when one waits for a yes, then — when a flow
/// of theirs did not load — `flows broken` in red beside it.
pub fn global_parts(chips: &[Chip], broken: bool) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    if let Some((note, waiting)) = everywhere(chips) {
        out.push((note, if waiting { Tone::Needs } else { Tone::Plain }));
    }
    if broken {
        out.push(("flows broken".into(), Tone::Bad));
    }
    out
}

/// The header line: what needs you first, then what is moving.
pub fn headline(rows: &[Row], news: usize, filter: &str) -> String {
    let (asked, need, working, fin, flows) = counts(rows);
    let mut parts = Vec::new();
    if asked > 0 {
        parts.push(format!("{asked} to decide"));
    }
    if need > 0 {
        parts.push(format!("{need} need{} you", if need == 1 { "s" } else { "" }));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if fin > 0 {
        parts.push(format!("{fin} finished"));
    }
    if flows > 0 {
        parts.push(format!("{flows} flow{}", if flows == 1 { "" } else { "s" }));
    }
    if news > 0 {
        parts.push(format!("{news} new"));
    }
    let mut s = String::from("relay");
    if parts.is_empty() {
        s.push_str(" · nothing running");
    }
    for p in parts {
        s.push_str(" · ");
        s.push_str(&p);
    }
    if !filter.is_empty() {
        s.push_str(&format!(" · ⌕ {filter}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::decision::{DecisionOption, DecisionStatus};
    use rl_proto::flow::{FiringStatus, StepStatus};
    use rl_proto::types::{Confidence, RunKind, RunState};

    #[test]
    fn a_flows_marks_are_one_per_firing_and_say_its_worst() {
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let ran = |id: &str, chain: &str, state: RunState| {
            let mut r = Run::new(id, RunKind::Command, id, vec![], "/w", "/w", t);
            r.chain = chain.into();
            r.state = state;
            DigestItem {
                run: r,
                summary: String::new(),
                at: t,
                unseen: false,
                joined_by: None,
                emitted: vec![],
                cwd: None,
                recap: None,
            }
        };
        // Newest first, as a row keeps them: fi3 stopped; fi2 two steps, well; fi1 a step failed.
        let items = [
            ran("c", "fi3", RunState::Stopped),
            ran("b2", "fi2", RunState::Finished),
            ran("b1", "fi2", RunState::Finished),
            ran("a2", "fi1", RunState::Failed),
            ran("a1", "fi1", RunState::Finished),
        ];
        let f = FlowRow {
            key: "f:x".into(),
            name: "x".into(),
            runs: items.iter().collect(),
            when: None,
            firing: None,
        };
        assert_eq!(f.marks(10), "×·○", "oldest first, each firing's worst step");
        assert_eq!(f.marks(2), "·○", "the newest firings");
    }

    fn item(id: &str, state: RunState, ws: &str, at: &str, unseen: bool) -> DigestItem {
        let t: Timestamp = at.parse().unwrap();
        let mut r = Run::new(id, RunKind::Agent, format!("title {id}"), vec![], ws, ws, t);
        r.state = state;
        r.confidence = Confidence::Exact;
        r.state_at = t;
        if state.is_terminal() {
            r.ended_at = Some(t);
        }
        DigestItem {
            run: r,
            summary: format!("summary {id}"),
            at: t,
            unseen,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: None,
        }
    }

    #[test]
    fn a_row_says_its_recap_while_it_waits_and_what_it_does_at_work() {
        let at = "2026-09-23T10:00:00Z";
        let with = |state: RunState| {
            let mut i = item("a1", state, "/w", at, false);
            i.run.cli = Some("claude".into());
            i.summary = "tool: Bash".into();
            i.recap = Some("Search fixed; nothing committed. Next: say commit.".into());
            i
        };
        for state in [RunState::Starting, RunState::Working] {
            assert_eq!(said(&with(state)), "tool: Bash", "{state:?}");
        }
        for state in [
            RunState::Idle,
            RunState::NeedsInput,
            RunState::Blocked,
            RunState::Stale,
            RunState::Finished,
            RunState::Failed,
            RunState::Stopped,
        ] {
            assert_eq!(
                said(&with(state)),
                "Search fixed; nothing committed. Next: say commit.",
                "{state:?}"
            );
        }
        // No recap kept: where it stands, as ever.
        let mut none = with(RunState::Idle);
        none.recap = None;
        assert_eq!(said(&none), "tool: Bash");
        // What the row says is what a search finds.
        let d = Digest {
            since: at.parse().unwrap(),
            items: vec![with(RunState::Idle)],
            decisions: vec![],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::default();
        let now: Timestamp = at.parse().unwrap();
        assert_eq!(keys(&build_with(&d, &names, "commit", now)), ["[w]", "r:a1"]);
    }

    #[test]
    fn what_a_restart_took_is_said_with_the_command_that_picks_it_up() {
        let at = "2026-09-23T10:00:00Z";
        let agent = item("a1", RunState::Failed, "/w", at, false);
        let mut cmd = item("c1", RunState::Failed, "/w", at, false);
        cmd.run.kind = RunKind::Command;
        let d = |lost: &[&str], items: Vec<DigestItem>| Digest {
            since: at.parse().unwrap(),
            items,
            decisions: vec![],
            checkouts: vec![],
            lost: lost.iter().map(|s| s.to_string()).collect(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        assert_eq!(
            lost_note(&d(&[], vec![agent.clone()])),
            None,
            "nothing lost, nothing said"
        );
        assert_eq!(
            lost_note(&d(&["a1", "c1"], vec![agent.clone(), cmd.clone()])).as_deref(),
            Some("2 lost in a restart · /resume all")
        );
        // `/resume all` would leave a command out: only commands, then the
        // words that take them.
        assert_eq!(
            lost_note(&d(&["c1"], vec![agent, cmd])).as_deref(),
            Some("1 lost in a restart · /resume all commands")
        );
    }

    fn question(id: &str, ws: &str, run: Option<&str>, at: &str) -> Decision {
        let t: Timestamp = at.parse().unwrap();
        Decision {
            id: id.into(),
            workspace: ws.into(),
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
            created_at: t,
            updated_at: t,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        }
    }

    fn keys(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Group(h) => format!("[{}]", h.name),
                other => other.key().unwrap(),
            })
            .collect()
    }

    /// The header of the project called `name`, among the built rows.
    fn head_of<'a>(rows: &'a [Row], name: &str) -> &'a Head {
        rows.iter()
            .find_map(|r| match r {
                Row::Group(h) if h.name == name => Some(h),
                _ => None,
            })
            .expect("a project header")
    }

    /// The default toggles: flows and agents on, hidden off.
    const DEFAULT: (bool, bool, bool, bool) = (true, true, true, false);

    fn build_with<'a>(d: &'a Digest, names: &Names, filter: &str, now: Timestamp) -> Vec<Row<'a>> {
        toggles_build(d, names, &[], DEFAULT, filter, now)
    }

    fn lens_build<'a>(d: &'a Digest, names: &Names, t: (bool, bool, bool, bool), now: Timestamp) -> Vec<Row<'a>> {
        toggles_build(d, names, &[], t, "", now)
    }

    /// The list as it stands with these flows loaded, at the default toggles.
    fn flows_build<'a>(d: &'a Digest, names: &Names, flows: &[Flow], filter: &str, now: Timestamp) -> Vec<Row<'a>> {
        toggles_build(d, names, flows, DEFAULT, filter, now)
    }

    /// The list with the flows loaded and the toggles `(flows, agents, shells, hidden)`.
    fn toggles_build<'a>(
        d: &'a Digest,
        names: &Names,
        flows: &[Flow],
        (show_flows, show_agents, show_shells, show_hidden): (bool, bool, bool, bool),
        filter: &str,
        now: Timestamp,
    ) -> Vec<Row<'a>> {
        build(
            d,
            &Context {
                names,
                flows,
                show_flows,
                show_agents,
                show_shells,
                show_hidden,
                filter,
                now,
                relay: None,
            },
        )
    }

    /// A flow as `flow.list` gives it: on, every 30m, `in` the folder given.
    fn flow(name: &str, in_: Option<&str>) -> Flow {
        Flow {
            name: name.into(),
            hash: "h".into(),
            def: serde_json::json!({
                "in_": in_,
                "trigger": { "when": { "Every": 1800 }, "on": [] },
            }),
            source: format!("/h/flows/{name}").into(),
            folder: in_.map(PathBuf::from),
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    /// A step of a flow, as the cause of a run.
    fn step_of(flow: &str) -> Cause {
        Cause::Flow {
            flow: flow.into(),
            step: "probe".into(),
        }
    }

    #[test]
    fn a_search_narrows_by_words_and_by_what_things_are() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, web) = ("/home/f/api", "/home/f/web");
        let at = "2026-09-10T11:00:00Z";
        let mut agent = item("agent", RunState::Working, api, at, false);
        agent.run.kind = RunKind::Agent;
        agent.run.title = "fix the login".into();
        let mut failed = item("failed", RunState::Failed, web, at, false);
        failed.run.title = "cargo test".into();
        failed.run.kind = RunKind::Command;
        let mut probe = item("probe", RunState::Working, web, at, false);
        probe.run.kind = RunKind::Command;
        probe.run.cause = step_of("nightly");
        // The question is a firing's: its chain is the firing's id.
        let asking = Firing {
            id: "q1".into(),
            flow: "tests-failed".into(),
            ..firing(FiringStatus::Waiting, &[])
        };
        let d = Digest {
            since: now,
            items: vec![agent, failed, probe],
            decisions: vec![question("q1", api, None, at)],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: vec![asking],
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(web), "web".to_string()),
            (PathBuf::from("/home/f/idle"), "idle".to_string()),
        ]);
        let shown = |f: &str| keys(&build_with(&d, &names, f, now));
        // Every word somewhere: the title and the project both count.
        assert_eq!(shown("login api"), ["[api]", "r:agent"]);
        assert_eq!(shown("login web"), Vec::<String>::new());
        // A project's name keeps all of it; an idle project answers to its name.
        assert_eq!(shown("in:api"), ["[api]", "r:agent", "q:q1"]);
        assert_eq!(shown("idle"), ["[idle]"]);
        // Tokens pick by what a thing is, and take no bare project along.
        assert_eq!(shown("is:failed"), ["[web]", "r:failed"]);
        assert_eq!(shown("kind:agent"), ["[api]", "r:agent"]);
        assert_eq!(
            shown("kind:question"),
            ["[api]", "q:q1"],
            "a question stands in its project"
        );
        assert_eq!(shown("by:nightly"), ["[web]", "r:probe"]);
        assert_eq!(shown("by:tests"), ["[api]", "q:q1"], "a question by its flow's firing");
        // A word nothing says finds nothing.
        assert_eq!(shown("global"), Vec::<String>::new());
    }

    #[test]
    fn a_summary_of_everything_is_not_a_project_of_its_own() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, relay) = ("/home/f/api", "/home/f/.relay");
        let at = "2026-09-10T11:00:00Z";
        // The top, written in the Relay home; a recap, in the project's folder.
        let mut top = item("s1", RunState::Finished, relay, at, true);
        top.run.cause = Cause::Assistant("summary".into());
        let mut recap = item("s2", RunState::Finished, api, at, true);
        recap.run.cause = Cause::Assistant("summary".into());
        // Anything else run there is the list's as ever.
        let shell = item("sh1", RunState::Finished, relay, at, true);
        let d = Digest {
            since: now,
            items: vec![top, recap, shell],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([(PathBuf::from(api), "api".to_string())]);
        let cx = |relay: Option<&'static str>| Context {
            names: &names,
            flows: &[],
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            filter: "",
            now,
            relay: relay.map(Path::new),
        };
        assert_eq!(
            keys(&build(&d, &cx(Some(relay)))),
            ["[.relay]", "r:sh1", "[api]", "r:s2"]
        );
        let mut d = d;
        d.items.retain(|i| i.run.id != "sh1");
        assert_eq!(
            keys(&build(&d, &cx(Some(relay)))),
            ["[api]", "r:s2"],
            "no group for the top alone"
        );
        assert_eq!(keys(&build(&d, &cx(None))), ["[.relay]", "r:s1", "[api]", "r:s2"]);
    }

    #[test]
    fn a_search_lands_on_the_project_it_names_not_what_mentions_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, web) = ("/home/f/api", "/home/f/web");
        let at = "2026-09-10T11:00:00Z";
        let mut pinned = item("pinned", RunState::Working, api, at, false);
        pinned.run.pinned_at = Some(now);
        pinned.summary = "waiting on web to deploy".into();
        let mut deploy = item("deploy", RunState::Working, api, at, false);
        deploy.run.title = "web deploy".into();
        let d = Digest {
            since: now,
            items: vec![pinned, deploy, item("w1", RunState::Working, web, at, false)],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(web), "web".to_string()),
        ]);
        let best = |f: &str| {
            let rows = build_with(&d, &names, f, now);
            let named: Vec<Named> = rows.iter().map(|r| named(r, &d, &names, &[])).collect();
            let list = keys(&rows);
            let order = crate::tui::query::ranked(&Query::parse(f).words, &named);
            let order: Vec<String> = order.iter().map(|&i| list[i].clone()).collect();
            (list, order)
        };
        let (list, order) = best("web");
        assert_eq!(
            list,
            ["r:pinned", "[api]", "r:deploy", "[web]", "r:w1"],
            "the list keeps its order"
        );
        assert_eq!(order, ["[web]", "r:deploy", "r:w1", "r:pinned"]);
        assert_eq!(best("deploy").1, ["r:deploy", "r:pinned"], "a title before a summary");
    }

    #[test]
    fn the_toggles_keep_a_run_by_what_it_is_or_what_started_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let ws = "/home/f/relay";
        let agent = item("agent", RunState::Working, ws, "2026-09-10T11:00:00Z", false);
        let mut shell = item("shell", RunState::Working, ws, "2026-09-10T11:01:00Z", false);
        // What a warm shell really is: the `sh` profile's kind wins over the
        // `Command` hint it was started with. Not an agent, nothing started
        // it — a shell, which the shells toggle governs.
        shell.run.kind = RunKind::Shell;
        // A flow's agent step: both an agent and a flow's.
        let mut probe = item("probe", RunState::Working, ws, "2026-09-10T11:02:00Z", false);
        probe.run.cause = step_of("nightly");
        // A flow's shell step: a flow's, and not an agent.
        let mut onfail = item("onfail", RunState::Working, ws, "2026-09-10T11:03:00Z", false);
        onfail.run.cause = step_of("on-fail");
        onfail.run.kind = RunKind::Shell;
        let d = Digest {
            since: now,
            items: vec![agent, shell, probe, onfail],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
        let runs = |t| {
            lens_build(&d, &names, t, now)
                .iter()
                .filter_map(|r| r.item().map(|i| i.run.id.clone()))
                .collect::<Vec<_>>()
        };
        // All on, in the order they started.
        assert_eq!(runs((true, true, true, false)), ["agent", "shell", "probe", "onfail"]);
        // Agents off drops the lone agent; the shell stays (shells on), and
        // the flow's steps stay — the agent step as a flow's, the shell step
        // as a shell.
        assert_eq!(runs((true, false, true, false)), ["shell", "probe", "onfail"]);
        // Flows off, with agents and shells on, keeps everything: the agent
        // step shows as an agent, the shell step as a shell.
        assert_eq!(runs((false, true, true, false)), ["agent", "shell", "probe", "onfail"]);
        // Shells off drops the plain shell and the flow's shell step; the
        // agents — the lone one and the flow's step — stay.
        assert_eq!(runs((false, true, false, false)), ["agent", "probe"]);
    }

    #[test]
    fn pinned_runs_stand_above_everything_however_old_whatever_the_toggles() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let mut old = item(
            "old01",
            RunState::Finished,
            "/home/f/api",
            "2026-09-01T10:00:00Z",
            false,
        );
        old.run.pinned_at = Some("2026-09-02T10:00:00Z".parse().unwrap());
        let mut shell = item(
            "shell",
            RunState::Working,
            "/home/f/relay",
            "2026-09-10T11:00:00Z",
            false,
        );
        shell.run.kind = RunKind::Shell;
        shell.run.pinned_at = Some("2026-09-03T10:00:00Z".parse().unwrap());
        let d = Digest {
            since: now,
            items: vec![
                shell,
                item("fail1", RunState::Failed, "/home/f/relay", "2026-09-10T11:30:00Z", true),
                old,
            ],
            decisions: vec![question("q1", "/home/f/relay", Some("fail1"), "2026-09-10T11:31:00Z")],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/api"), "api".to_string()),
        ]);
        // In the order they were pinned, above the projects; a week-old run
        // stays, and its folder keeps its place by name.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["r:old01", "r:shell", "[api]", "[relay]", "r:fail1", "q:q1"]
        );
        // No toggle takes a pin away.
        let flows = lens_build(&d, &names, (true, false, true, false), now);
        assert_eq!(&keys(&flows)[..2], ["r:old01", "r:shell"]);
        // The filter does, and matches a pin by its folder too.
        assert_eq!(keys(&build_with(&d, &names, "api", now)), ["r:old01", "[api]"]);
    }

    fn hid(key: &str, at: &str) -> Hidden {
        Hidden {
            key: key.into(),
            at: at.parse().unwrap(),
        }
    }

    #[test]
    fn what_you_hid_shows_inline_when_hidden_is_on() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, relay, scratch) = ("/home/f/api", "/home/f/relay", "/home/f/scratch");
        let at = "2026-09-10T11:00:00Z";
        let mut mine = item("mine", RunState::Working, relay, at, false);
        mine.run.hidden_at = Some("2026-09-10T11:10:00Z".parse().unwrap());
        let mut fired = item("fired", RunState::Finished, relay, at, false);
        fired.run.cause = step_of("nightly");
        let mut pinned = item("pinned", RunState::Working, relay, at, false);
        pinned.run.pinned_at = Some(at.parse().unwrap());
        pinned.run.hidden_at = Some("2026-09-10T11:10:00Z".parse().unwrap());
        let d = Digest {
            since: now,
            items: vec![
                mine,
                item("kept", RunState::Working, relay, at, false),
                fired,
                pinned,
                item("inapi", RunState::Finished, api, at, false),
                item("loose", RunState::Working, scratch, at, false),
            ],
            decisions: Vec::new(),
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: vec![
                // A registered project, a folder that is not one, a flow, and
                // a folder with nothing on the list at all.
                hid(&format!("g:{api}"), "2026-09-10T11:10:00Z"),
                hid(&format!("g:{scratch}"), "2026-09-10T11:10:00Z"),
                hid("f:nightly", "2026-09-10T11:10:00Z"),
                hid("g:/home/f/gone", "2026-09-10T11:10:00Z"),
            ],
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(relay), "relay".to_string()),
        ]);
        let nightly = [flow("nightly", Some(relay))];
        let view = |t| keys(&toggles_build(&d, &names, &nightly, t, "", now));
        // Hidden off, the default: what you hid is off the list — the project's
        // header too, the pin too. Nothing hidden needs you, so nothing comes
        // through. `kept` is an agent, so agents off takes it away.
        assert_eq!(view((true, true, true, false)), ["[relay]", "r:kept"]);
        assert_eq!(view((false, true, true, false)), ["[relay]", "r:kept"]);
        assert_eq!(view((true, false, true, false)), ["[relay]"]);
        // Hidden on, the hidden rows stand inline in their places, alongside
        // the kept ones: a hidden project's header, registered or not, even
        // with nothing under it; the flow folded as ever; a hidden pin in the
        // pinned band; `kept`, not hidden, among them.
        assert_eq!(
            view((true, true, true, true)),
            [
                "r:pinned",
                "[api]",
                "r:inapi",
                "[gone]",
                "[relay]",
                "r:kept",
                "r:mine",
                "f:nightly",
                "[scratch]",
                "r:loose",
            ]
        );
        // The search reads the word too: with hidden on, `is:hidden` keeps
        // only what is hidden.
        assert_eq!(
            keys(&build(
                &d,
                &Context {
                    names: &names,
                    flows: &nightly,
                    filter: "is:hidden",
                    show_flows: true,
                    show_agents: true,
                    show_shells: true,
                    show_hidden: true,
                    now,
                    relay: None,
                },
            )),
            ["r:pinned", "[relay]", "r:mine"]
        );
    }

    #[test]
    fn what_you_hid_comes_through_when_it_comes_to_need_you() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, relay) = ("/home/f/api", "/home/f/relay");
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        // Hidden while it worked, and failed after: it comes through.
        let mut failed = item("failed", RunState::Failed, relay, "2026-09-10T11:00:00Z", true);
        failed.run.hidden_at = Some(t("2026-09-10T10:00:00Z"));
        // Hidden once it had failed: that was you saying so; it stays away.
        let mut seen = item("seen", RunState::Failed, relay, "2026-09-10T11:00:00Z", true);
        seen.run.hidden_at = Some(t("2026-09-10T11:30:00Z"));
        // In a hidden project: one that asks after comes through, one still
        // working does not.
        let asks = item("asks", RunState::NeedsInput, api, "2026-09-10T10:30:00Z", true);
        let busy = item("busy", RunState::Working, api, "2026-09-10T10:30:00Z", false);
        // A hidden flow's run that failed after comes through as a run.
        let mut firing = item("firing", RunState::Failed, relay, "2026-09-10T11:20:00Z", true);
        firing.run.cause = step_of("nightly");
        let d = Digest {
            since: now,
            items: vec![failed, seen, asks, busy, firing],
            decisions: vec![question("q1", relay, None, "2026-09-10T11:31:00Z")],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: vec![
                hid(&format!("g:{api}"), "2026-09-10T10:00:00Z"),
                hid("f:nightly", "2026-09-10T10:00:00Z"),
            ],
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(relay), "relay".to_string()),
        ]);
        let nightly = [flow("nightly", Some(relay))];
        let all = flows_build(&d, &names, &nightly, "", now);
        // Above the projects, what asks before what failed, then in the
        // order it came; its project stays hidden.
        assert_eq!(keys(&all), ["r:asks", "r:failed", "r:firing", "[relay]", "q:q1"]);
        let (_, need, ..) = counts(&all);
        assert_eq!(need, 3, "the headline counts them as needing you");
        // A toggle still narrows it, as it would any run that needs you:
        // agents off leaves only the flow's run among those come through.
        assert_eq!(
            keys(&toggles_build(
                &d,
                &names,
                &nightly,
                (true, false, true, false),
                "",
                now
            )),
            ["r:firing", "[relay]", "q:q1"]
        );
        // With hidden on they are in their places, not above them: the run
        // folds into its flow again, which stands by its run's start. The
        // question, which ignores the toggles, is in its project here too.
        assert_eq!(
            keys(&toggles_build(&d, &names, &nightly, (true, true, true, true), "", now)),
            [
                "[api]",
                "r:asks",
                "r:busy",
                "[relay]",
                "r:failed",
                "r:seen",
                "f:nightly",
                "q:q1"
            ]
        );
        // Hiding it again is how it goes back: the time moves on.
        let mut again = d.clone();
        again.items[0].run.hidden_at = Some(t("2026-09-10T11:45:00Z"));
        assert_eq!(
            keys(&flows_build(&again, &names, &nightly, "", now)),
            ["r:asks", "r:firing", "[relay]", "q:q1"]
        );
        let r = &d.items[2].run;
        assert_eq!(hidden_since(r, &names, &[], &d.hidden), Some(t("2026-09-10T10:00:00Z")));
        assert_eq!(hidden_with(r, &names, &[], &d.hidden), Some("its project"));
        assert_eq!(hidden_with(&d.items[4].run, &names, &[], &d.hidden), Some("its flow"));
        assert_eq!(hidden_with(&d.items[0].run, &names, &[], &d.hidden), None);
        assert!(comes_through(r, t("2026-09-10T10:00:00Z")));
        assert!(!comes_through(r, t("2026-09-10T10:30:00Z")), "not after, at");
    }

    #[test]
    fn a_folder_is_one_project_whatever_branch_its_runs_are_on() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let on = |mut i: DigestItem, b: &str| {
            i.run.branch = Some(b.into());
            i
        };
        let co = |p: &str, b: &str, worktrees| Checkout {
            path: p.into(),
            branch: Some(b.into()),
            detached: None,
            worktrees,
            badges: vec![],
        };
        let d = Digest {
            since: now,
            items: vec![
                on(
                    item("a1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z", false),
                    "main",
                ),
                on(
                    item("a2", RunState::Working, "/home/f/api", "2026-09-10T11:40:00Z", false),
                    "feat",
                ),
                on(
                    item("w1", RunState::Working, "/home/f/web", "2026-09-10T11:30:00Z", false),
                    "main",
                ),
            ],
            decisions: vec![],
            checkouts: vec![co("/home/f/api", "main", 3), co("/home/f/web", "main", 0)],
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::default();
        let rows = build_with(&d, &names, "", now);
        // Two repos on `main` stay two projects, each named for its folder.
        assert_eq!(keys(&rows), ["[api]", "r:a2", "r:a1", "[web]", "r:w1"]);
        let head = |n: usize| match &rows[n] {
            Row::Group(h) => checkout_words(h.checkout.as_ref().unwrap()),
            _ => panic!("a header"),
        };
        assert_eq!((head(0), head(3)), ("⎇ main · 3 worktrees".into(), "⎇ main".into()));
        // Only the run on another branch than its folder's says so.
        assert_eq!(branch_aside(&d.items[0].run, &d.checkouts), None);
        assert_eq!(branch_aside(&d.items[1].run, &d.checkouts).as_deref(), Some("⎇ feat"));
        assert_eq!(
            branch_aside(&d.items[1].run, &[]),
            None,
            "no checkout, nothing to differ from"
        );

        assert_eq!(keys(&build_with(&d, &names, "on:feat", now)), ["[api]", "r:a2"]);
        assert_eq!(
            keys(&build_with(&d, &names, "on:main", now)),
            ["[api]", "r:a1", "[web]", "r:w1"]
        );
    }

    #[test]
    fn a_checkout_in_words() {
        let mut c = Checkout {
            path: "/w".into(),
            branch: None,
            detached: Some("a1b2c3d".into()),
            worktrees: 0,
            badges: vec![],
        };
        assert_eq!(checkout_words(&c), "⎇ a1b2c3d");
        c.branch = Some("main".into());
        c.detached = None;
        c.worktrees = 1;
        assert_eq!(checkout_words(&c), "⎇ main · 1 worktree");
        c.worktrees = 4;
        c.badges = vec!["PR #95".into()];
        assert_eq!(checkout_words(&c), "⎇ main · PR #95 · 4 worktrees");
    }

    #[test]
    fn a_question_stands_in_its_project_and_projects_go_by_name() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = Digest {
            since: now,
            items: vec![
                item("work1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z", false),
                item("fail1", RunState::Failed, "/home/f/relay", "2026-09-10T11:00:00Z", true),
                item("blok1", RunState::Blocked, "/home/f/api", "2026-09-10T11:55:00Z", true),
                item(
                    "done1",
                    RunState::Finished,
                    "/home/f/relay",
                    "2026-09-10T10:00:00Z",
                    false,
                ),
                item(
                    "old01",
                    RunState::Finished,
                    "/home/f/relay",
                    "2026-09-08T10:00:00Z",
                    false,
                ),
            ],
            decisions: vec![question("q1", "/home/f/relay", Some("fail1"), "2026-09-10T11:01:00Z")],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[api]", "r:work1", "r:blok1", "[relay]", "r:done1", "r:fail1", "q:q1"],
            "projects by name, however urgent; rows as they came; the question under its run; api is named by its folder; old01 is gone"
        );
        assert_eq!(counts(&rows), (1, 2, 1, 1, 0));
        match &rows[3] {
            Row::Group(h) => assert_eq!((h.name.as_str(), h.place.as_str()), ("relay", "/home/f/relay")),
            _ => panic!("relay's header"),
        }
        assert_eq!(
            headline(&rows, 2, ""),
            "relay · 1 to decide · 2 need you · 1 working · 1 finished · 2 new"
        );

        let rows = build_with(&d, &names, "api", now);
        assert_eq!(keys(&rows), ["[api]", "r:work1", "r:blok1"]);
        assert_eq!(headline(&rows, 0, "api"), "relay · 1 needs you · 1 working · ⌕ api");
        assert_eq!(headline(&[], 0, ""), "relay · nothing running");
    }

    #[test]
    fn a_registered_folder_shows_even_with_no_runs() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = Digest {
            since: now,
            items: vec![item(
                "work1",
                RunState::Working,
                "/home/f/relay",
                "2026-09-10T11:50:00Z",
                false,
            )],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/idle"), "idle".to_string()),
        ]);
        // The idle project is a line of its own, in its place by name.
        assert_eq!(keys(&build_with(&d, &names, "", now)), ["[idle]", "[relay]", "r:work1"]);
        // It carries the folder so selecting it lands on the project.
        match build_with(&d, &names, "idle", now).last() {
            Some(Row::Group(h)) => {
                assert_eq!(h.name, "idle");
                assert_eq!(h.workspace, Path::new("/home/f/idle"));
            }
            _ => panic!("idle folder is the last row it filters to"),
        }
        // The filter reaches idle projects too.
        assert_eq!(keys(&build_with(&d, &names, "idle", now)), ["[idle]"]);
        // A toggle narrows what is inside projects, never which projects there
        // are: the idle one and yours stay, even where nothing in them matches.
        assert_eq!(
            keys(&lens_build(&d, &names, (true, false, true, false), now)),
            ["[idle]", "[relay]"]
        );
    }

    #[test]
    fn with_no_runs_and_no_folders_the_list_is_empty() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = digest(now, vec![], vec![]);
        let waiting = [Flow {
            approved_hash: None,
            ..flow("pulse", None)
        }];
        let rows = flows_build(&d, &Names::default(), &waiting, "", now);
        // A flow waiting for its first yes still has a row, under everywhere.
        assert_eq!(keys(&rows), ["[everywhere]", "f:pulse"]);
        // One with a yes has its row, but has not done anything: a fresh home
        // still says it has nothing running.
        let flows = [flow("pulse", None)];
        let rows = flows_build(&d, &Names::default(), &flows, "", now);
        assert_eq!(keys(&rows), ["[everywhere]", "f:pulse"]);
        assert_eq!(headline(&rows, 0, ""), "relay · nothing running");
    }

    /// The list's digest: these runs and firings, and nothing else.
    fn digest(now: Timestamp, items: Vec<DigestItem>, firings: Vec<Firing>) -> Digest {
        Digest {
            since: now,
            items,
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings,
            outside: vec![],
        }
    }

    #[test]
    fn the_headline_says_the_flows_with_no_in_and_a_yes_they_wait_for() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let say = |flows: &[Flow]| everywhere(&chips(flows, None, now));
        let (pulse, nightly, notify) = (flow("pulse", None), flow("nightly", None), flow("notify", None));
        let waiting = Flow {
            approved_hash: None,
            ..flow("deploy", None)
        };
        assert_eq!(say(&[]), None, "no flows, nothing said");
        // A flow `in` a folder is its header's, not the headline's.
        assert_eq!(say(&[flow("nightly", Some("/home/f/relay"))]), None);
        assert_eq!(
            say(&[pulse.clone(), nightly.clone(), notify.clone()]),
            Some(("↻ 3 everywhere".into(), false))
        );
        // One waiting, and it is all there is: the count already says which.
        assert_eq!(
            say(std::slice::from_ref(&waiting)),
            Some(("↻ 1 everywhere needs a yes · /flow approve".into(), true))
        );
        assert_eq!(
            say(&[pulse, nightly, notify, waiting]),
            Some(("↻ 4 everywhere · 1 needs a yes · /flow approve".into(), true))
        );
    }

    #[test]
    fn the_headline_says_a_flow_did_not_load_beside_them() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let parts = |flows: &[Flow], broken| global_parts(&chips(flows, None, now), broken);
        let flows = [flow("pulse", None), flow("nightly", None)];
        assert_eq!(parts(&flows, false), [("↻ 2 everywhere".to_string(), Tone::Plain)]);
        assert_eq!(
            parts(&flows, true),
            [
                ("↻ 2 everywhere".to_string(), Tone::Plain),
                ("flows broken".to_string(), Tone::Bad)
            ]
        );
        // None loaded: it still says so.
        assert_eq!(parts(&[], true), [("flows broken".to_string(), Tone::Bad)]);
        assert!(parts(&[], false).is_empty());
    }

    fn fired(id: &str, flow: &str, state: RunState, at: &str, unseen: bool) -> DigestItem {
        let mut i = item(id, state, "/home/f/relay", at, unseen);
        i.run.cause = step_of(flow);
        i.summary = if state == RunState::Failed {
            "exit 1 · 0s".into()
        } else {
            "exit 0 · 0s".into()
        };
        i
    }

    #[test]
    fn a_flows_runs_are_one_row() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let mut items: Vec<DigestItem> = (0..30)
            .map(|n| {
                fired(
                    &format!("p{n:02}"),
                    "pulse",
                    RunState::Finished,
                    &format!("2026-09-10T11:{n:02}:00Z"),
                    false,
                )
            })
            .collect();
        items.push(fired("bad", "pulse", RunState::Failed, "2026-09-10T10:30:00Z", true));
        // Its run still going stays its own row; so does one started by hand.
        let mut going = fired("go", "pulse", RunState::Working, "2026-09-10T11:59:30Z", false);
        going.run.ended_at = None;
        items.push(going);
        items.push(item(
            "hand",
            RunState::Finished,
            "/home/f/relay",
            "2026-09-10T11:58:00Z",
            false,
        ));
        let d = digest(now, items, vec![]);
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let flows = [flow("pulse", Some("/home/f/relay"))];
        let rows = flows_build(&d, &names, &flows, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", "f:pulse", "r:hand", "r:go"],
            "in the order they came: a flow by its first run on the list"
        );
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row");
        };
        assert_eq!(f.runs.len(), 31);
        assert_eq!(f.last().unwrap().run.id, "p29");
        assert_eq!((f.ok(), f.failed(), f.stopped()), (30, 1, 0));
        assert!(f.unseen(), "the failed run is still news");
        assert_eq!(f.when.as_deref(), Some("every 30m"));
        assert_eq!(
            f.summary(),
            "exit 0 · 0s",
            "the last run's own line; the tally is the marks', the cadence the pane's"
        );
        assert_eq!(f.marks(31).chars().filter(|c| *c == '×').count(), 1);
        assert!(f.marks(31).starts_with('×'), "oldest first: {}", f.marks(31));
        assert_eq!(counts(&rows), (0, 0, 1, 1, 1));
        assert_eq!(
            headline(&rows, 1, ""),
            "relay · 1 working · 1 finished · 1 flow · 1 new"
        );

        // Never opened: whatever happens, a flow is one row.
        let opened_rows = flows_build(&d, &names, &flows, "", now);
        assert_eq!(keys(&opened_rows), keys(&rows), "no row grows into its runs");

        // The filter finds a flow by its name.
        let rows = flows_build(&d, &names, &flows, "puls", now);
        assert_eq!(keys(&rows), ["[relay]", "f:pulse"], "{:?}", keys(&rows));

        // A flow whose last run failed ranks as failed and says so.
        let d2 = digest(
            now,
            vec![
                fired("a", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false),
                fired("b", "pulse", RunState::Failed, "2026-09-10T11:30:00Z", true),
                item("w", RunState::Working, "/home/f/relay", "2026-09-10T11:40:00Z", false),
            ],
            vec![],
        );
        let rows = flows_build(&d2, &names, &flows, "", now);
        assert_eq!(keys(&rows), ["[relay]", "f:pulse", "r:w"], "the flow came first");
        assert_eq!(counts(&rows), (0, 1, 1, 0, 1));
    }

    #[test]
    fn a_flow_filtered_by_one_runs_text_keeps_its_full_history() {
        // A filter that matches only one run's own summary (not the flow or
        // group name) must still show the flow's FULL history — not just the
        // matching run — since `ok()`/`failed()`/`marks()`/`summary()` are
        // all computed from `FlowRow.runs`.
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let mut items: Vec<DigestItem> = (0..30)
            .map(|n| {
                fired(
                    &format!("p{n:02}"),
                    "pulse",
                    RunState::Finished,
                    &format!("2026-09-10T11:{n:02}:00Z"),
                    false,
                )
            })
            .collect();
        items.push(fired("bad", "pulse", RunState::Failed, "2026-09-10T11:30:30Z", true));
        let d = digest(now, items, vec![]);
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let flows = [flow("pulse", Some("/home/f/relay"))];
        // "exit 1" only appears in the failing run's own summary, not in the
        // flow's name ("pulse") or the group name ("relay").
        let rows = flows_build(&d, &names, &flows, "exit 1", now);
        assert_eq!(keys(&rows), ["[relay]", "f:pulse"], "{:?}", keys(&rows));
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row");
        };
        assert_eq!(f.runs.len(), 31, "the flow's full history, not just the matching run");
        assert_eq!(
            (f.ok(), f.failed(), f.stopped()),
            (30, 1, 0),
            "stats reflect the flow's true, overwhelmingly-successful history"
        );
    }

    /// The latest firing of `prod-logs`, with its steps where they stand.
    fn firing(status: FiringStatus, steps: &[(&str, StepStatus)]) -> Firing {
        Firing {
            id: "fi1".into(),
            flow: "prod-logs".into(),
            trigger: "when".into(),
            event: None,
            status,
            started_at: "2026-09-10T11:00:00Z".parse().unwrap(),
            ended_at: None,
            outcome: None,
            steps: steps
                .iter()
                .map(|(name, status)| rl_proto::flow::StepState {
                    name: name.to_string(),
                    status: *status,
                    run: None,
                    decision: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_flow_row_says_where_its_firing_stands() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = digest(
            now,
            vec![
                fired("probe", "prod-logs", RunState::Finished, "2026-09-10T11:01:00Z", false),
                fired("triage", "prod-logs", RunState::Finished, "2026-09-10T11:02:00Z", false),
            ],
            vec![firing(
                FiringStatus::Waiting,
                &[
                    ("probe", StepStatus::Done),
                    ("triage", StepStatus::Done),
                    ("review", StepStatus::Waiting),
                ],
            )],
        );
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let flows = [flow("prod-logs", Some("/home/f/relay"))];
        let rows = flows_build(&d, &names, &flows, "", now);
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row: {:?}", keys(&rows));
        };
        assert_eq!(f.runs.len(), 2);
        let at = f.firing.expect("its latest firing");
        assert_eq!(flows::state_words(at), "waiting at review");
        // A flow that has not fired since the digest knew it says nothing.
        let d = digest(now, d.items.clone(), vec![]);
        let rows = flows_build(&d, &names, &flows, "", now);
        assert!(rows.iter().any(|r| matches!(r, Row::Flow(f) if f.firing.is_none())));
    }

    #[test]
    fn a_flow_row_sits_in_its_in_project_wherever_its_runs_ran() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (api, other) = ("/w/api", "/w/other");
        let mut ran = item("ran", RunState::Finished, other, "2026-09-10T11:00:00Z", false);
        ran.run.cause = step_of("prod-logs");
        let d = digest(now, vec![ran], vec![]);
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(other), "other".to_string()),
        ]);
        let flows = [flow("prod-logs", Some(api))];
        assert_eq!(
            keys(&flows_build(&d, &names, &flows, "", now)),
            ["[api]", "f:prod-logs", "[other]"],
            "under the project it is in, not the folder its run ran in"
        );
        assert_eq!(group_key(&d.items[0].run, &names, &flows), api);
        // Not loaded now, where it is `in` is not known: everywhere.
        assert_eq!(
            keys(&flows_build(&d, &names, &[], "", now)),
            ["[api]", "[other]", "[everywhere]", "f:prod-logs"]
        );
        // With no `in`, everywhere too.
        assert_eq!(
            keys(&flows_build(&d, &names, &[flow("prod-logs", None)], "", now)),
            ["[api]", "[other]", "[everywhere]", "f:prod-logs"]
        );
        // Hiding the project it is in hides it with the project.
        let mut hidden = d.clone();
        hidden.hidden = vec![hid(&format!("g:{api}"), "2026-09-10T11:30:00Z")];
        assert_eq!(keys(&flows_build(&hidden, &names, &flows, "", now)), ["[other]"]);
    }

    #[test]
    fn a_flow_with_a_yes_has_its_row_before_it_ever_fired() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let d = digest(now, vec![], vec![]);
        let nightly = Flow {
            next_at: Some("2026-09-10T12:12:00Z".parse().unwrap()),
            ..flow("nightly", Some(relay))
        };
        let flows = [nightly.clone()];
        for t in [(true, true, true, false), (true, false, true, false)] {
            assert_eq!(
                keys(&toggles_build(&d, &names, &flows, t, "", now)),
                ["[relay]", "f:nightly"],
                "{t:?}"
            );
        }
        assert_eq!(
            keys(&toggles_build(&d, &names, &flows, (false, true, true, false), "", now)),
            ["[relay]"],
            "flows off"
        );
        let rows = flows_build(&d, &names, &flows, "", now);
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row");
        };
        assert!(f.runs.is_empty() && f.last().is_none());
        assert_eq!(flows::stands(f, flows.first(), now), "every 30m · next in 12m");
        assert_eq!(f.marks(10), "");
        assert_eq!(counts(&rows), (0, 0, 0, 0, 0));

        // Paused, or changed since its yes, it says so.
        let says = |x: Flow| {
            let flows = [x];
            flows_build(&d, &names, &flows, "", now).iter().find_map(|r| match r {
                Row::Flow(f) => Some(flows::stands(f, flows.first(), now)),
                _ => None,
            })
        };
        let paused = Flow {
            paused: true,
            ..nightly.clone()
        };
        assert_eq!(says(paused).as_deref(), Some("paused · every 30m"));
        let changed = Flow {
            hash: "changed".into(),
            ..nightly.clone()
        };
        assert_eq!(says(changed).as_deref(), Some("needs a yes again · every 30m"));

        // Never said yes to: still a row, where you approve it — it says so.
        // Removed, with no run of its own still on the list: no row.
        let waiting = Flow {
            approved_hash: None,
            ..nightly.clone()
        };
        assert_eq!(says(waiting).as_deref(), Some("needs a yes · every 30m"));
        let old = fired("old", "nightly", RunState::Finished, "2026-09-07T12:00:00Z", false);
        let d = digest(now, vec![old], vec![]);
        let removed = [Flow {
            removed_at: Some(now),
            ..nightly
        }];
        assert_eq!(keys(&flows_build(&d, &names, &removed, "", now)), ["[relay]"]);
    }

    #[test]
    fn every_defined_flow_stands_as_a_row_even_before_a_yes() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let d = digest(now, vec![], vec![]);
        // A flow still waiting for its first yes: it has a row of its own —
        // that is where you see it and approve it, not a header chip.
        let waiting = [Flow {
            approved_hash: None,
            ..flow("deploy", Some(relay))
        }];
        let rows = flows_build(&d, &names, &waiting, "", now);
        assert_eq!(keys(&rows), ["[relay]", "f:deploy"]);
        // Flows off takes it away like any flow row.
        assert_eq!(
            keys(&toggles_build(
                &d,
                &names,
                &waiting,
                (false, true, true, false),
                "",
                now
            )),
            ["[relay]"]
        );
        // The row says it needs a yes, with what starts it.
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row");
        };
        assert_eq!(flows::stands(f, waiting.first(), now), "needs a yes · every 30m");
    }

    #[test]
    fn a_project_hints_when_something_hidden_needs_you() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        // Hidden once it had already failed, so it stays off the list and does
        // not come through — it is genuinely buried.
        let mut failed = item("failed", RunState::Failed, relay, "2026-09-10T11:00:00Z", false);
        failed.run.hidden_at = Some("2026-09-10T11:30:00Z".parse().unwrap());
        let working = item("work", RunState::Working, relay, "2026-09-10T11:40:00Z", false);
        let d = digest(now, vec![failed, working], vec![]);
        let rows = build_with(&d, &names, "", now);
        assert!(
            !keys(&rows).contains(&"r:failed".to_string()),
            "the failed run is off the list"
        );
        assert_eq!(
            head_of(&rows, "relay").attention,
            1,
            "the header hints that one needs you"
        );
        // Hidden on: it stands inline, so nothing is buried and no hint.
        let shown = toggles_build(&d, &names, &[], (true, true, true, true), "", now);
        assert_eq!(head_of(&shown, "relay").attention, 0);
    }

    #[test]
    fn agents_off_hints_a_needy_agent_on_its_project() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        // An agent waiting on you, recent so it is live.
        let needy = item("ask", RunState::NeedsInput, relay, "2026-09-10T11:50:00Z", false);
        let d = digest(now, vec![needy], vec![]);
        // Agents on: it shows, nothing buried.
        let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
        assert!(keys(&on).contains(&"r:ask".to_string()));
        assert_eq!(head_of(&on, "relay").attention, 0);
        // Agents off: it is off the list, and the header hints.
        let off = toggles_build(&d, &names, &[], (true, false, true, false), "", now);
        assert!(!keys(&off).contains(&"r:ask".to_string()));
        assert_eq!(head_of(&off, "relay").attention, 1);
    }

    #[test]
    fn the_hint_says_how_many_need_you() {
        assert_eq!(needs_hint(0), None);
        assert_eq!(needs_hint(1).as_deref(), Some("! 1 needs you"));
        assert_eq!(needs_hint(3).as_deref(), Some("! 3 need you"));
    }

    #[test]
    fn a_shell_is_hidden_when_shells_are_off() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        // A plain warm shell, nothing started it.
        let mut shell = item("sh", RunState::Working, relay, "2026-09-10T11:50:00Z", false);
        shell.run.kind = RunKind::Shell;
        let d = digest(now, vec![shell], vec![]);
        // Shells on: it is on the list.
        let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
        assert!(keys(&on).contains(&"r:sh".to_string()));
        // Shells off: it is gone.
        let off = toggles_build(&d, &names, &[], (true, true, false, false), "", now);
        assert!(!keys(&off).contains(&"r:sh".to_string()));
    }

    #[test]
    fn shells_off_hints_a_needy_shell_on_its_project() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        // A shell blocked on you, recent so it is live.
        let mut needy = item("sh", RunState::Blocked, relay, "2026-09-10T11:50:00Z", false);
        needy.run.kind = RunKind::Shell;
        let d = digest(now, vec![needy], vec![]);
        // Shells on: it shows, nothing buried.
        let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
        assert!(keys(&on).contains(&"r:sh".to_string()));
        assert_eq!(head_of(&on, "relay").attention, 0);
        // Shells off: it is off the list, and the header hints it needs you.
        let off = toggles_build(&d, &names, &[], (true, true, false, false), "", now);
        assert!(!keys(&off).contains(&"r:sh".to_string()));
        assert_eq!(head_of(&off, "relay").attention, 1);
    }

    #[test]
    fn a_flows_row_keeps_its_history_however_old() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let days_ago = "2026-09-07T12:00:00Z";
        let d = digest(
            now,
            vec![
                fired("old", "pulse", RunState::Failed, days_ago, false),
                item("hand", RunState::Finished, relay, days_ago, false),
            ],
            vec![],
        );
        let flows = [flow("pulse", Some(relay))];
        let rows = flows_build(&d, &names, &flows, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", "f:pulse"],
            "a run by hand that old is off the list"
        );
        let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
            panic!("no flow row");
        };
        assert_eq!(f.marks(10), "×", "its run of three days ago");
        // A flow not loaded now has no row of its own: its old runs go as
        // any other.
        assert_eq!(keys(&flows_build(&d, &names, &[], "", now)), ["[relay]"]);
    }

    #[test]
    fn a_search_and_hide_take_an_unfired_flows_row_as_any_other() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let d = digest(now, vec![], vec![]);
        let flows = [flow("nightly", Some(relay))];
        let shown = |d: &Digest, filter: &str| keys(&flows_build(d, &names, &flows, filter, now));
        let hidden_on = |d: &Digest| keys(&toggles_build(d, &names, &flows, (true, true, true, true), "", now));
        assert_eq!(shown(&d, "night"), ["[relay]", "f:nightly"]);
        assert_eq!(shown(&d, "kind:flow"), ["[relay]", "f:nightly"]);
        assert_eq!(shown(&d, "by:nightly"), ["[relay]", "f:nightly"]);
        assert_eq!(shown(&d, "in:relay kind:flow"), ["[relay]", "f:nightly"]);
        assert_eq!(shown(&d, "kind:agent"), Vec::<String>::new());
        assert_eq!(shown(&d, "tidy"), Vec::<String>::new());

        // `/hide` on it, or on its project: off the list, back only with
        // hidden on.
        for key in ["f:nightly".to_string(), format!("g:{relay}")] {
            let mut hidden = d.clone();
            hidden.hidden = vec![hid(&key, "2026-09-10T11:00:00Z")];
            assert!(!shown(&hidden, "").contains(&"f:nightly".to_string()), "{key}");
            assert_eq!(hidden_on(&hidden), ["[relay]", "f:nightly"], "{key}");
        }
        // Hidden on with nothing hidden is just the list as it stands.
        assert_eq!(hidden_on(&d), ["[relay]", "f:nightly"], "nothing hidden");
    }

    #[test]
    fn a_flow_that_has_not_run_comes_after_those_that_have() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let d = digest(
            now,
            vec![fired("p1", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false)],
            vec![],
        );
        let flows = [
            flow("beta", Some(relay)),
            flow("pulse", Some(relay)),
            flow("alpha", Some(relay)),
        ];
        assert_eq!(
            keys(&flows_build(&d, &names, &flows, "", now)),
            ["[relay]", "f:pulse", "f:alpha", "f:beta"],
            "what ran first, then the rest by name"
        );

        // In an order you kept, where it is new it still comes last, not first
        // as what is new to the order otherwise does — and moving a row keeps it there.
        let mut d = digest(
            now,
            vec![
                fired("p1", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false),
                item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false),
            ],
            vec![],
        );
        d.order = kept(&["r:work", "f:pulse"]);
        let rows = flows_build(&d, &names, &flows, "", now);
        assert_eq!(keys(&rows), ["[relay]", "r:work", "f:pulse", "f:alpha", "f:beta"]);
        let moved = reorder(&rows, &rows, &d.order, "f:pulse", true).unwrap();
        assert_eq!(moved.rows, ["f:pulse", "r:work", "f:alpha", "f:beta"]);
    }

    #[test]
    fn words_for_causes_and_askers() {
        assert_eq!(cause_words(&Cause::Manual), "started by hand");
        assert_eq!(
            cause_words(&Cause::Flow {
                flow: "nightly".into(),
                step: "probe".into()
            }),
            "flow nightly · probe"
        );
        let q = question("q", "/w", None, "2026-09-10T11:01:00Z");
        let run = item("r", RunState::Failed, "/w", "2026-09-10T11:00:00Z", false).run;
        assert_eq!(asked_by(&q, Some(&run), None), "title r");
        assert_eq!(asked_by(&q, None, None), "you");
        // A flow's ask step: its firing is the question's chain, and the step
        // that asks is the one holding it.
        let mut asking = Firing {
            id: "q".into(),
            ..firing(
                FiringStatus::Waiting,
                &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
            )
        };
        let firings = [asking.clone()];
        assert_eq!(firing_of(&q, &firings).map(|f| f.id.as_str()), Some("q"));
        assert_eq!(asked_by(&q, None, firing_of(&q, &firings)), "flow prod-logs · review");
        asking.steps[1].status = StepStatus::Done;
        asking.steps[1].decision = Some("q".into());
        assert_eq!(asked_by(&q, None, Some(&asking)), "flow prod-logs · review");
        assert!(firing_of(&question("other", "/w", None, "2026-09-10T11:01:00Z"), &firings).is_none());
    }

    #[test]
    fn a_groups_header_carries_the_flows_in_its_folder() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let next = |name: &str, in_: Option<&str>, at: &str| Flow {
            next_at: Some(at.parse().unwrap()),
            ..flow(name, in_)
        };
        let flows = vec![
            next("nightly", Some(relay), "2026-09-10T16:00:00Z"),
            flow("tests-failed", Some(relay)),
            Flow {
                approved_hash: None,
                ..flow("deploy", Some(relay))
            },
            Flow {
                paused: true,
                ..flow("tidy", Some(relay))
            },
            flow("elsewhere", Some("/home/f/api")),
            next("pulse", None, "2026-09-10T12:00:30Z"),
        ];
        let texts = |c: &[Chip]| c.iter().map(Chip::text).collect::<Vec<_>>();
        assert_eq!(
            texts(&chips(&flows, Some(Path::new(relay)), now)),
            ["! deploy", "↻ nightly 4h", "↻ tests-failed", "↻ tidy paused"],
            "what needs a yes first, then by name; api's flow stays out"
        );
        assert_eq!(texts(&chips(&flows, None, now)), ["↻ pulse 30s"]);

        // On the list: the folder's flows ride its header as the tally, and
        // every flow defined has its row under it, run or not — the one
        // waiting for its first yes among them; those with no `in` stand
        // under `everywhere`.
        let d = digest(
            now,
            vec![item("work1", RunState::Working, relay, "2026-09-10T11:59:00Z", false)],
            vec![],
        );
        let names = Names::new([(relay.into(), "relay".to_string())]);
        let build_flows = |flows: &[Flow]| {
            flows_build(&d, &names, flows, "", now)
                .iter()
                .map(|r| match r {
                    Row::Group(h) => format!("[{}] {}", h.name, texts(&h.flows).join(" · ")),
                    other => other.key().unwrap(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            build_flows(&flows),
            [
                "[api] ↻ elsewhere",
                "f:elsewhere",
                "[relay] ! deploy · ↻ nightly 4h · ↻ tests-failed · ↻ tidy paused",
                "r:work1",
                "f:deploy",
                "f:nightly",
                "f:tests-failed",
                "f:tidy",
                "[everywhere] ",
                "f:pulse"
            ],
            "what has run first, then the rest by name"
        );
    }

    #[test]
    fn a_row_wears_what_it_is_and_says_how_it_stands() {
        let mut i = item("sh", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
        i.run.kind = RunKind::Shell;
        assert!(live_shell(&i.run));
        assert_eq!(icon(&i.run), SHELL);
        assert_eq!(
            run_words(&i.run),
            "",
            "a shell at rest says nothing: its icon says what it is"
        );
        // One that ended is news like any other ended run, and reads as one.
        i.run.state = RunState::Stopped;
        assert!(!live_shell(&i.run));
        assert_eq!((icon(&i.run), run_words(&i.run)), (SHELL, "stopped".to_string()));
        // So is one that wants you: the word says so.
        i.run.state = RunState::NeedsInput;
        assert!(!live_shell(&i.run));
        assert_eq!(run_words(&i.run), "needs you");
        // A warm shell is one by its tag, whatever its argv resolved to.
        let mut warm = item("warm", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
        warm.run.kind = RunKind::Command;
        warm.run.tags = vec![SHELL_TAG.into()];
        assert_eq!((icon(&warm.run), run_words(&warm.run)), (SHELL, String::new()));
        // An agent says its state; a command, a plugin too, is a wrench.
        let mut a = item("ag", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
        assert_eq!((icon(&a.run), run_words(&a.run)), (AGENT, "idle".to_string()));
        a.run.kind = RunKind::Command;
        a.run.state = RunState::Finished;
        assert_eq!((icon(&a.run), run_words(&a.run)), (COMMAND, "done".to_string()));
        a.run.kind = RunKind::Plugin;
        assert_eq!(icon(&a.run), COMMAND);
    }

    #[test]
    fn an_agent_wears_its_clis_glyph_in_its_clis_colour() {
        use ratatui::style::{Color, Style};
        let mut a = item("ag", RunState::Working, "/w", "2026-09-10T11:00:00Z", false);
        let mut look = |cli: Option<&str>| {
            a.run.cli = cli.map(String::from);
            let i = icon(&a.run);
            (i, icon_style(i, cli, false))
        };
        assert_eq!(
            look(Some("claude")),
            ("✻", Style::default().fg(Color::Rgb(0xD9, 0x77, 0x57)))
        );
        assert_eq!(look(Some("codex")), ("⬡", Style::default()));
        assert_eq!(look(Some("gemini")).0, "✧");
        assert_eq!(look(Some("cursor")).0, "❖");
        assert_eq!(look(Some("opencode")).0, "⊡");
        // One Relay has no profile for, or none at all, is a plain agent.
        assert_eq!(look(Some("fake-cli")), (AGENT, Style::default()));
        assert_eq!(look(None), (AGENT, Style::default()));
        // Ended, it is quiet whatever it is.
        assert_eq!(icon_style("✻", Some("claude"), true), crate::tui::theme::dim());
        for l in looks().values() {
            if let Some(i) = &l.icon {
                assert_eq!(ratatui::text::Span::raw(i.as_str()).width(), 1, "{i}");
            }
        }
    }

    #[test]
    fn a_shell_says_where_it_is_from_its_project() {
        let home = std::env::var("HOME").unwrap();
        let project = PathBuf::from(&home).join("Projects/foodai");
        assert_eq!(shell_place(&project.join("app/src"), &project), "app/src");
        assert_eq!(shell_place(&project, &project), "", "at the root, nothing to say");
        assert_eq!(shell_place(&PathBuf::from(&home).join("tmp/x"), &project), "~/tmp/x");
        assert_eq!(shell_place(Path::new(&home), &project), "~");
        assert_eq!(shell_place(Path::new("/etc/nginx"), &project), "/etc/nginx");
        // A sibling that shares the project's start is not inside it.
        assert_eq!(
            shell_place(&PathBuf::from(&home).join("Projects/foodai-old"), &project),
            "~/Projects/foodai-old"
        );
        assert_eq!(
            short_path(&PathBuf::from(format!("{home}2"))),
            format!("{home}2"),
            "not home"
        );
    }

    #[test]
    fn every_icon_is_one_quiet_column() {
        use ratatui::text::Span;
        for i in [AGENT, SHELL, COMMAND, FLOW, QUESTION] {
            assert_eq!(Span::raw(i).width(), 1, "{i}");
            assert_eq!(i.chars().count(), 1, "{i} carries no variation selector");
        }
        assert_eq!(icon_style(SHELL, None, false), crate::tui::theme::accent());
        assert_eq!(icon_style(QUESTION, None, false), crate::tui::theme::needs());
        assert_eq!(icon_style(SHELL, None, true), crate::tui::theme::dim());
    }

    #[test]
    fn a_header_is_its_path_and_a_name_of_its_own() {
        let names = Names::new([
            (PathBuf::from("/home/f/api"), "api".to_string()),
            (PathBuf::from("/home/f/scratch-2"), "scratch".to_string()),
        ]);
        let own = |k: &str| names.own(k, Path::new(if k.starts_with('/') { k } else { "/home/f/api" }));
        assert_eq!(own("/home/f/api"), None, "named for its folder: nothing to add");
        assert_eq!(own("/home/f/scratch-2").as_deref(), Some("scratch"), "renamed");
        assert_eq!(own("/home/f/web"), None, "never registered");
        assert_eq!(own("RLX-12").as_deref(), Some("RLX-12"), "a ref");

        let parts = |place: &str, own: Option<&str>, room: usize| {
            let (a, b, c) = place_parts(place, own, room);
            [a, b, c]
        };
        assert_eq!(parts("~/Projects/foodai", None, 40), ["~/Projects/", "foodai", ""]);
        assert_eq!(
            parts("~/tmp/scratch-2", Some("scratch"), 40),
            ["~/tmp/", "scratch-2", " · scratch"]
        );
        assert_eq!(parts("~", None, 40), ["", "~", ""]);
        assert_eq!(parts("/", None, 40), ["", "/", ""]);
        // Too long: the start goes, a folder at a time; the last stays whole.
        let long = "~/Projects/clients/acme/very/deep/tree/leaf-folder";
        let [a, b, c] = parts(long, None, 30);
        assert_eq!(
            [a.as_str(), b.as_str(), c.as_str()],
            ["…/very/deep/tree/", "leaf-folder", ""]
        );
        assert!(a.chars().count() + b.chars().count() <= 30);
        // With no room for any of the start, the name gives way, then the
        // folder stands alone, uncut, even past the room.
        assert_eq!(parts(long, Some("x"), 14), ["", "leaf-folder", ""]);
        assert_eq!(
            parts("~/a/an-extremely-long-folder-name", None, 10)[1],
            "an-extremely-long-folder-name"
        );
    }

    #[test]
    fn a_shell_stands_where_it_came_as_any_other_row() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let ws = "/home/f/relay";
        let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
        let mut shell = item("shell", RunState::Idle, ws, "2026-09-10T11:20:00Z", false);
        shell.run.kind = RunKind::Shell;
        let asks = item("asks", RunState::NeedsInput, ws, "2026-09-10T11:10:00Z", false);
        let work = item("work", RunState::Working, ws, "2026-09-10T11:55:00Z", false);
        let d = Digest {
            since: now,
            items: vec![work, shell, asks],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order: Vec::new(),
            firings: Vec::new(),
            outside: vec![],
        };
        // Neither where you work nor what asks for you jumps ahead: each in
        // the order it started.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[relay]", "r:asks", "r:shell", "r:work"]
        );
    }

    #[test]
    fn a_flows_runs_outside_every_project_are_everywhere() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let at = "2026-09-10T11:00:00Z";
        let (home, relay, web) = ("/home/f", "/home/f/relay", "/home/f/web");
        let by = |id: &str, state: RunState, ws: &str, cause: Cause| {
            let mut i = item(id, state, ws, at, false);
            i.run.cause = cause;
            i
        };
        let d = digest(
            now,
            vec![
                // A flow with no `in`, in home and in a project: one row.
                by("mentions", RunState::Finished, home, step_of("gchat-mentions")),
                by("mentions2", RunState::Failed, home, step_of("gchat-mentions")),
                by("inproj", RunState::Finished, relay, step_of("gchat-mentions")),
                // A flow's run still going, outside every project.
                by("fix", RunState::Working, home, step_of("look-at-it")),
                // A flow `in` a folder that is not a registered project.
                by("webs", RunState::Finished, web, step_of("pulse")),
                // By hand, in home.
                by("hand", RunState::Working, home, Cause::Manual),
            ],
            vec![],
        );
        let flows = vec![
            flow("gchat-mentions", None),
            flow("look-at-it", None),
            flow("pulse", Some(web)),
        ];
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let shown = |filter: &str| keys(&flows_build(&d, &names, &flows, filter, now));
        let home_name = Names::default().workspace(Path::new(home));
        assert_eq!(
            shown(""),
            [
                // By hand, a run keeps its folder.
                format!("[{home_name}]"),
                "r:hand".into(),
                "[relay]".into(),
                // A flow's row is in the folder it is `in`.
                "[web]".into(),
                "f:pulse".into(),
                // The rest, after the projects, however they sort.
                "[everywhere]".into(),
                "r:fix".into(),
                "f:gchat-mentions".into(),
                // Its run still going is its own row; the flow has its row
                // anyway, quiet, until that run ends and folds into it.
                "f:look-at-it".into(),
            ],
            "outside every project: everywhere, after the projects"
        );

        // It answers to its word, as a project answers to its name.
        assert_eq!(
            shown("in:everywhere"),
            ["[everywhere]", "r:fix", "f:gchat-mentions", "f:look-at-it"]
        );

        // Its header is no folder, and its key says so.
        let rows = flows_build(&d, &names, &flows, "", now);
        let head = rows
            .iter()
            .find_map(|r| match r {
                Row::Group(h) if h.is_everywhere() => Some(h.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (head.place.as_str(), head.workspace.as_os_str().is_empty()),
            ("everywhere", true)
        );
        assert_eq!(Row::Group(head).key().as_deref(), Some("g:everywhere"));

        // A flow not loaded now leaves its row everywhere: where it is `in`
        // is not known. A run by hand keeps its folder.
        let bare = keys(&build_with(&d, &names, "", now));
        let at = |k: &str| {
            bare.iter()
                .position(|x| x == k)
                .unwrap_or_else(|| panic!("{k}: {bare:?}"))
        };
        assert!(at("[everywhere]") < at("f:gchat-mentions"));
        assert!(at("[everywhere]") < at("f:pulse"));
        assert!(at(&format!("[{home_name}]")) < at("r:hand"));
    }

    /// One project of four runs, started a minute apart in the order of
    /// their names (`one` first, `four` last), whatever they came to; and
    /// another project of one.
    fn four(order: Vec<Order>) -> (Digest, Names) {
        let (api, relay) = ("/home/f/api", "/home/f/relay");
        let d = Digest {
            since: "2026-09-10T09:00:00Z".parse().unwrap(),
            items: vec![
                item("one", RunState::Working, relay, "2026-09-10T10:00:00Z", false),
                item("two", RunState::Working, relay, "2026-09-10T10:01:00Z", false),
                item("three", RunState::Idle, relay, "2026-09-10T10:02:00Z", false),
                item("four", RunState::Finished, relay, "2026-09-10T10:03:00Z", false),
                item("there", RunState::Working, api, "2026-09-10T10:00:00Z", false),
            ],
            decisions: Vec::new(),
            checkouts: Vec::new(),
            lost: Vec::new(),
            hidden: Vec::new(),
            order,
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(relay), "relay".to_string()),
        ]);
        (d, names)
    }

    fn kept(rows: &[&str]) -> Vec<Order> {
        vec![Order {
            group: "g:/home/f/relay".into(),
            rows: rows.iter().map(|s| s.to_string()).collect(),
        }]
    }

    /// What becomes of a run later: its state, and when that came.
    fn comes_to(d: &mut Digest, id: &str, state: RunState, at: &str) {
        let t: Timestamp = at.parse().unwrap();
        let i = d.items.iter_mut().find(|i| i.run.id == id).unwrap();
        i.run.state = state;
        i.run.state_at = t;
        i.at = t;
        i.unseen = true;
    }

    const NOW: &str = "2026-09-10T12:00:00Z";

    #[test]
    fn a_project_stands_in_the_order_its_rows_came_until_you_put_it_in_yours() {
        let now: Timestamp = NOW.parse().unwrap();
        // Nothing kept: the order they started in, the newest at the bottom,
        // whatever state each is in.
        let (d, names) = four(Vec::new());
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"]
        );
        // An order kept for a project is its order; the other project's own
        // is untouched, and an empty one is no order at all.
        let (d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[api]", "r:there", "[relay]", "r:one", "r:three", "r:two", "r:four"]
        );
        let (d, names) = four(kept(&[]));
        assert_eq!(
            keys(&build_with(&d, &names, "", now))[3..],
            ["r:one", "r:two", "r:three", "r:four"]
        );
        // What has left the list since is no matter: the rest keep theirs.
        let (d, names) = four(kept(&["r:gone", "r:one", "r:three", "r:two", "r:four"]));
        assert_eq!(
            keys(&build_with(&d, &names, "", now))[3..],
            ["r:one", "r:three", "r:two", "r:four"]
        );
    }

    #[test]
    fn a_row_stays_where_it_stands_whatever_comes_of_it() {
        let now: Timestamp = NOW.parse().unwrap();
        for (order, stands) in [
            (Vec::new(), ["r:one", "r:two", "r:three", "r:four"]),
            (
                kept(&["r:one", "r:three", "r:two", "r:four"]),
                ["r:one", "r:three", "r:two", "r:four"],
            ),
        ] {
            let (mut d, names) = four(order);
            // It asks, another fails, another blocks, the last finishes:
            // each says so where it is, and none of them moves.
            comes_to(&mut d, "two", RunState::NeedsInput, "2026-09-10T11:50:00Z");
            comes_to(&mut d, "four", RunState::Failed, "2026-09-10T11:51:00Z");
            comes_to(&mut d, "three", RunState::Blocked, "2026-09-10T11:52:00Z");
            comes_to(&mut d, "one", RunState::Finished, "2026-09-10T11:53:00Z");
            assert_eq!(keys(&build_with(&d, &names, "", now))[3..], stands);
            // Answered, nothing moves either.
            comes_to(&mut d, "two", RunState::Working, "2026-09-10T11:55:00Z");
            assert_eq!(keys(&build_with(&d, &names, "", now))[3..], stands);
        }
        // A shell is a row as any other: where it came, not first.
        let (mut d, names) = four(Vec::new());
        d.items[2].run.kind = RunKind::Shell;
        assert_eq!(
            keys(&build_with(&d, &names, "", now))[3..],
            ["r:one", "r:two", "r:three", "r:four"]
        );
    }

    #[test]
    fn a_new_run_enters_at_the_bottom() {
        let now: Timestamp = NOW.parse().unwrap();
        let new =
            |d: &mut Digest, id: &str, at: &str| d.items.push(item(id, RunState::Working, "/home/f/relay", at, false));
        let (mut d, names) = four(Vec::new());
        new(&mut d, "new", "2026-09-10T11:00:00Z");
        assert_eq!(
            keys(&build_with(&d, &names, "", now))[3..],
            ["r:one", "r:two", "r:three", "r:four", "r:new"]
        );
        // Below your order too, in the order they came.
        let (mut d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
        new(&mut d, "newer", "2026-09-10T11:30:00Z");
        new(&mut d, "new", "2026-09-10T11:00:00Z");
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows)[3..],
            ["r:one", "r:three", "r:two", "r:four", "r:new", "r:newer"]
        );
        // Moved, it takes its place in your order, and the rest keep theirs.
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:new", true).unwrap().rows,
            ["r:one", "r:three", "r:two", "r:new", "r:four", "r:newer"]
        );
    }

    #[test]
    fn a_question_stands_under_the_run_that_asked_it() {
        let now: Timestamp = NOW.parse().unwrap();
        let relay = "/home/f/relay";
        let asked = |order: Vec<Order>| {
            let (mut d, names) = four(order);
            comes_to(&mut d, "two", RunState::NeedsInput, "2026-09-10T11:00:00Z");
            d.decisions = vec![
                // Asked by no run that is on the list: at the bottom of its
                // project, though it was asked first.
                question("q0", relay, None, "2026-09-10T09:30:00Z"),
                question("q1", relay, Some("two"), "2026-09-10T11:00:00Z"),
                // A folder with nothing else on the list has its header for it.
                question("q2", "/home/f/web", None, "2026-09-10T11:10:00Z"),
            ];
            (d, names)
        };
        let (d, names) = asked(Vec::new());
        let rows = build_with(&d, &names, "", now);
        // No questions above the projects: each in its own.
        assert_eq!(
            keys(&rows),
            [
                "[api]", "r:there", "[relay]", "r:one", "r:two", "q:q1", "r:three", "r:four", "q:q0", "[web]", "q:q2"
            ]
        );
        assert_eq!(counts(&rows), (3, 1, 3, 1, 0));
        // In your order, it goes where its run goes.
        let (d, names) = asked(kept(&["r:two", "r:one", "r:three", "r:four"]));
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows)[3..9],
            ["r:two", "q:q1", "r:one", "r:three", "r:four", "q:q0"]
        );
        // A row moves past a question as past nothing, and the question is
        // not part of the order kept.
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:one", true).unwrap().rows,
            ["r:one", "r:two", "r:three", "r:four"]
        );
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:two", false).unwrap().rows,
            ["r:one", "r:two", "r:three", "r:four"]
        );
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:four", false),
            Err("already at the bottom of its project")
        );
        assert_eq!(
            reorder(&rows, &rows, &d.order, "q:q1", true),
            Err("a question stays under the run that asked it")
        );
        // A toggle leaves its run out, never the question.
        let flows = lens_build(&d, &names, (true, false, true, false), now);
        assert_eq!(keys(&flows), ["[api]", "[relay]", "q:q0", "q:q1", "[web]", "q:q2"]);
    }

    #[test]
    fn a_row_moves_one_place_inside_its_project_and_no_further() {
        let now: Timestamp = NOW.parse().unwrap();
        let (mut d, names) = four(Vec::new());
        let mut pinned = item(
            "pinned",
            RunState::Working,
            "/home/f/relay",
            "2026-09-10T11:00:00Z",
            false,
        );
        pinned.run.pinned_at = Some("2026-09-10T11:00:00Z".parse().unwrap());
        d.items.push(pinned);
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            [
                "r:pinned", "[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"
            ]
        );
        let mv = |key: &str, up: bool| reorder(&rows, &rows, &d.order, key, up);
        // From the list's own order, the whole of it is kept.
        assert_eq!(
            mv("r:two", true).unwrap(),
            Order {
                group: "g:/home/f/relay".into(),
                rows: ["r:two", "r:one", "r:three", "r:four"].map(String::from).to_vec(),
            }
        );
        assert_eq!(
            mv("r:one", false).unwrap().rows,
            ["r:two", "r:one", "r:three", "r:four"]
        );
        // Never out of its project, nor into the band, nor anything but a row.
        assert_eq!(mv("r:one", true), Err("already at the top of its project"));
        assert_eq!(mv("r:four", false), Err("already at the bottom of its project"));
        assert_eq!(mv("r:there", false), Err("already at the bottom of its project"));
        assert_eq!(mv("r:there", true), Err("already at the top of its project"));
        assert!(mv("r:pinned", false).unwrap_err().starts_with("a pinned run"));
        assert_eq!(mv("r:nope", true), Err("nothing here to move"));

        // What needs you moves as anything else, and is passed as anything else.
        let (mut d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
        comes_to(&mut d, "two", RunState::Blocked, "2026-09-10T11:00:00Z");
        let rows = build_with(&d, &names, "", now);
        assert_eq!(keys(&rows)[3..], ["r:one", "r:three", "r:two", "r:four"]);
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:two", true).unwrap().rows,
            ["r:one", "r:two", "r:three", "r:four"]
        );
        assert_eq!(
            reorder(&rows, &rows, &d.order, "r:four", true).unwrap().rows,
            ["r:one", "r:three", "r:four", "r:two"]
        );
    }

    #[test]
    fn a_project_moves_up_and_down_among_the_projects_and_keeps_its_place() {
        let now: Timestamp = NOW.parse().unwrap();
        let (api, relay, web) = ("g:/home/f/api", "g:/home/f/relay", "g:/home/f/web");
        let (mut d, _) = four(Vec::new());
        // A flow's run outside every project: `everywhere`, after them all.
        let mut fix = item("fix", RunState::Working, "/home/f", "2026-09-10T11:00:00Z", false);
        fix.run.cause = step_of("look-at-it");
        d.items.push(fix);
        let names = |more: &[(&str, &str)]| {
            let mut all = vec![
                (PathBuf::from("/home/f/api"), "api".to_string()),
                (PathBuf::from("/home/f/relay"), "relay".to_string()),
                (PathBuf::from("/home/f/web"), "web".to_string()),
            ];
            all.extend(more.iter().map(|(p, n)| (PathBuf::from(p), n.to_string())));
            Names::new(all)
        };
        let heads = |d: &Digest, names: &Names| -> Vec<String> {
            keys(&build_with(d, names, "", now))
                .into_iter()
                .filter(|k| k.starts_with('['))
                .collect()
        };
        let three = names(&[]);
        // By name until you move one.
        assert_eq!(heads(&d, &three), ["[api]", "[relay]", "[web]", "[everywhere]"]);
        let rows = build_with(&d, &three, "", now);
        let mv = |key: &str, up: bool| reorder(&rows, &rows, &d.order, key, up);
        assert_eq!(
            mv(relay, true),
            Ok(Order {
                group: Order::PROJECTS.into(),
                rows: [relay, api, web].map(String::from).to_vec(),
            })
        );
        assert_eq!(mv(relay, false).unwrap().rows, [api, web, relay]);
        assert_eq!(mv(api, true), Err("already the first project"));
        assert_eq!(mv(web, false), Err("already the last project"));
        assert_eq!(
            mv("g:everywhere", true),
            Err("everywhere stays last, below the projects")
        );

        // Kept, the projects stand in it, each with what is under it; a
        // project's own rows keep theirs.
        d.order = vec![Order {
            group: Order::PROJECTS.into(),
            rows: [relay, api, web].map(String::from).to_vec(),
        }];
        assert_eq!(
            keys(&build_with(&d, &three, "", now)),
            [
                "[relay]",
                "r:one",
                "r:two",
                "r:three",
                "r:four",
                "[api]",
                "r:there",
                "[web]",
                "[everywhere]",
                "r:fix"
            ]
        );
        // A project the order does not know — new since — enters at the
        // bottom, by name, however its name sorts; everywhere stays last.
        let four_of = names(&[("/home/f/aaa", "aaa")]);
        assert_eq!(
            heads(&d, &four_of),
            ["[relay]", "[api]", "[web]", "[aaa]", "[everywhere]"]
        );
        // Moving builds on the order kept, and takes the new one into it.
        let rows = build_with(&d, &four_of, "", now);
        assert_eq!(
            reorder(&rows, &rows, &d.order, "g:/home/f/aaa", true).unwrap().rows,
            [relay, api, "g:/home/f/aaa", web]
        );
        // One the list does not show now — hidden, or gone — keeps its place.
        d.order[0].rows.insert(1, "g:/home/f/gone".into());
        let rows = build_with(&d, &three, "", now);
        assert_eq!(
            reorder(&rows, &rows, &d.order, api, true).unwrap().rows,
            [api, relay, "g:/home/f/gone", web]
        );
        // A search that leaves projects out moves among what it shows.
        let (shown, full) = (build_with(&d, &three, "in:web", now), build_with(&d, &three, "", now));
        assert_eq!(
            reorder(&shown, &full, &d.order, web, true),
            Err("already the first project")
        );
    }

    #[test]
    fn words_and_columns() {
        assert_eq!(state_words(RunState::NeedsInput, Confidence::Exact), "needs you");
        assert_eq!(state_words(RunState::Working, Confidence::Likely), "working~");
        assert_eq!(state_words(RunState::Finished, Confidence::Guess), "done?");
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 5), "abcd…");
        assert_eq!(fit("abcdef", 0), "");
        assert_eq!(fit("héllo wörld", 6), "héllo…");
        // Wide glyphs take two columns each, padded or cut by columns.
        use unicode_width::UnicodeWidthStr;
        assert_eq!(fit("権限", 6), "権限  ");
        assert_eq!(
            fit("権限の確認", 6),
            "権限… ",
            "a glyph that would straddle the cut gives way"
        );
        assert_eq!(fit("権限の確認", 7), "権限の…");
        assert_eq!(fit("a権限の確認", 4), "a権…");
        for w in 0..12 {
            assert_eq!(fit("権限の確認 x", w).width(), w, "{w}");
        }
    }

    /// One of Claude's own sessions in `cwd`, as the digest carries it.
    fn claude_in(id: &str, cwd: &str, state: Option<&str>, status: &str) -> Outside {
        Outside {
            cwd: PathBuf::from(cwd),
            ..outside::session(id, state, status, None)
        }
    }

    #[test]
    fn claudes_own_sessions_stand_under_their_folders_project() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let mut d = digest(
            now,
            vec![item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false)],
            vec![],
        );
        d.outside = vec![
            // In the project's folder, and deeper in it — a worktree there.
            claude_in("bg", relay, Some("working"), "busy"),
            claude_in("deep", "/home/f/relay/.claude/worktrees/x", None, "idle"),
            // Outside every registered folder: its own folder's group.
            claude_in("away", "/home/f/scratch", None, "busy"),
        ];
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            [
                "[relay]",
                "o:claude:sess-bg",
                "o:claude:sess-deep",
                "r:work",
                "[scratch]",
                "o:claude:sess-away"
            ]
        );
        let Some(Row::Group(h)) = rows.iter().find(|r| matches!(r, Row::Group(h) if h.name == "scratch")) else {
            panic!("no scratch header");
        };
        assert_eq!(h.workspace, PathBuf::from("/home/f/scratch"));
        // A run there would be filed the same way.
        let run_there = item("r", RunState::Working, "/home/f/scratch", "2026-09-10T11:30:00Z", false);
        assert_eq!(group_key(&run_there.run, &names, &[]), h.workspace.to_string_lossy());
    }

    #[test]
    fn a_session_stands_where_rld_filed_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let mut d = digest(now, vec![], vec![]);
        // Deep in a repo no project registers: under the repo's root, as a
        // run started there is, not under its own folder.
        let mut deep = claude_in("deep", "/home/f/tools/src/bin", None, "idle");
        deep.workspace = Some(PathBuf::from("/home/f/tools"));
        d.outside = vec![deep, claude_in("guess", "/home/f/relay/sub", None, "idle")];
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", "o:claude:sess-guess", "[tools]", "o:claude:sess-deep"],
            "filed by rld, else guessed by the registered folder"
        );
    }

    #[test]
    fn a_hidden_session_comes_through_when_it_comes_to_need_you() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let session = |id: &str, state: &str, status: &str, at: &str| Outside {
            state_at: Some(t(at)),
            ..claude_in(id, relay, Some(state), status)
        };
        let mut d = digest(now, vec![], vec![]);
        d.outside = vec![
            // Blocked after its project was hidden: comes through.
            session("after", "blocked", "waiting", "2026-09-10T11:30:00Z"),
            // Blocked before: stays hidden.
            session("before", "blocked", "waiting", "2026-09-10T10:30:00Z"),
            // At work after: nothing needs you.
            session("busy", "working", "busy", "2026-09-10T11:30:00Z"),
        ];
        d.hidden = vec![Hidden {
            key: format!("g:{relay}"),
            at: t("2026-09-10T11:00:00Z"),
        }];
        assert_eq!(keys(&build_with(&d, &names, "", now)), ["o:claude:sess-after"]);
        // Its own `/hide`, later than it came to need you: hidden again.
        d.hidden.push(Hidden {
            key: "o:claude:sess-after".into(),
            at: t("2026-09-10T11:45:00Z"),
        });
        assert!(keys(&build_with(&d, &names, "", now)).is_empty());
        assert_eq!(
            keys(&lens_build(&d, &names, (true, true, true, true), now)),
            [
                "[relay]",
                "o:claude:sess-after",
                "o:claude:sess-before",
                "o:claude:sess-busy"
            ]
        );
    }

    #[test]
    fn claudes_own_sessions_are_agents_not_flows() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let mut d = digest(now, vec![], vec![]);
        d.outside = vec![claude_in("bg", relay, Some("working"), "busy")];
        assert_eq!(
            keys(&lens_build(&d, &names, DEFAULT, now)),
            ["[relay]", "o:claude:sess-bg"]
        );
        assert_eq!(
            keys(&lens_build(&d, &names, (false, true, true, false), now)),
            ["[relay]", "o:claude:sess-bg"],
            "agents on, flows off: a session shows"
        );
        assert_eq!(
            keys(&lens_build(&d, &names, (true, false, true, false), now)),
            ["[relay]"],
            "agents off: no session, it is not a flow"
        );
    }

    #[test]
    fn agents_own_sessions_stand_with_runs_in_the_order_they_came() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let mut d = digest(
            now,
            vec![item("idle", RunState::Idle, relay, "2026-09-10T11:30:00Z", false)],
            vec![],
        );
        let at = |t: &str, c: Outside| Outside {
            started_at: Some(t.parse().unwrap()),
            ..c
        };
        let blocked = Outside {
            waiting_for: Some("permission prompt".into()),
            ..claude_in("blocked", relay, Some("blocked"), "waiting")
        };
        d.outside = vec![
            at("2026-09-10T11:10:00Z", claude_in("done", relay, Some("done"), "")),
            at(
                "2026-09-10T11:50:00Z",
                claude_in("working", relay, Some("working"), "busy"),
            ),
            at("2026-09-10T11:20:00Z", blocked),
            at("2026-09-10T11:40:00Z", claude_in("asks", relay, None, "waiting")),
        ];
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            [
                "[relay]",
                "o:claude:sess-done",
                "o:claude:sess-blocked",
                "r:idle",
                "o:claude:sess-asks",
                "o:claude:sess-working"
            ],
            "what needs you does not jump ahead: each in the order it started"
        );
        // Those that need you say so where they stand.
        assert!(wants_you(&rows[2]) && wants_you(&rows[4]) && !wants_you(&rows[5]));
        // The headline counts them as runs: two need you, one works.
        assert_eq!(counts(&rows), (0, 2, 2, 1, 0));
        assert_eq!(headline(&rows, 0, ""), "relay · 2 need you · 2 working · 1 finished");
    }

    #[test]
    fn a_claude_session_hides_by_its_own_key_or_its_projects() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let mut d = digest(now, vec![], vec![]);
        d.outside = vec![claude_in("bg", relay, Some("working"), "busy")];
        for key in ["o:claude:sess-bg".to_string(), format!("g:{relay}")] {
            let mut hidden = d.clone();
            hidden.hidden = vec![hid(&key, "2026-09-10T11:00:00Z")];
            assert!(
                !keys(&lens_build(&hidden, &names, DEFAULT, now)).contains(&"o:claude:sess-bg".to_string()),
                "{key}"
            );
            assert_eq!(
                keys(&lens_build(&hidden, &names, (true, true, true, true), now)),
                ["[relay]", "o:claude:sess-bg"],
                "{key}"
            );
        }
    }

    #[test]
    fn a_search_finds_claudes_own_sessions() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let relay = "/home/f/relay";
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let mut d = digest(
            now,
            vec![item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false)],
            vec![],
        );
        d.outside = vec![
            claude_in("bg", relay, Some("blocked"), "waiting"),
            claude_in("away", "/home/f/scratch", None, "idle"),
        ];
        let shown = |f: &str| keys(&build_with(&d, &names, f, now));
        assert_eq!(
            shown("kind:claude"),
            ["[relay]", "o:claude:sess-bg", "[scratch]", "o:claude:sess-away"]
        );
        assert_eq!(shown("kind:agent in:relay"), ["[relay]", "o:claude:sess-bg", "r:work"]);
        assert_eq!(shown("is:needs"), ["[relay]", "o:claude:sess-bg"]);
        assert_eq!(shown("is:going kind:claude"), ["[scratch]", "o:claude:sess-away"]);
        assert_eq!(shown("name away"), ["[scratch]", "o:claude:sess-away"]);
        assert_eq!(shown("scratch"), ["[scratch]", "o:claude:sess-away"]);
        assert_eq!(shown("kind:flow"), Vec::<String>::new());
        // `kind:<agent>`: that agent's runs, and its own sessions.
        let mut both = d.clone();
        both.items[0].run.cli = Some("codex".into());
        both.outside.push(Outside {
            agent: "codex".into(),
            ..claude_in("cx", relay, None, "busy")
        });
        assert_eq!(
            keys(&build_with(&both, &names, "kind:codex", now)),
            ["[relay]", "o:codex:sess-cx", "r:work"]
        );
        assert!(!keys(&build_with(&both, &names, "kind:claude", now)).contains(&"r:work".to_string()));
        let kinds = list_kinds();
        for k in ["agent", "question", "claude", "codex", "gemini"] {
            assert!(kinds.iter().any(|x| x == k), "{k}: {kinds:?}");
        }
        // By its session id, and by where it lives.
        assert_eq!(shown("sess-aw"), ["[scratch]", "o:claude:sess-away"]);
        assert_eq!(shown("terminal"), ["[scratch]", "o:claude:sess-away"]);
        assert_eq!(shown("background"), ["[relay]", "o:claude:sess-bg"]);
        // The search ranks it by its name.
        let rows = build_with(&d, &names, "", now);
        let named: Vec<Named> = rows.iter().map(|r| named(r, &d, &names, &[])).collect();
        let order = crate::tui::query::ranked(&Query::parse("away").words, &named);
        assert_eq!(rows[order[0]].key().as_deref(), Some("o:claude:sess-away"));
    }
}
