//! The one list: questions and runs, ranked by attention, grouped by what
//! they belong to, and named in words rather than ids and paths.
//!
//! A group is a run's ref, else its folder — never its branch: a folder is one
//! project whichever branch it is on, and its header says which. Groups come in
//! order of name; inside a group, questions come first,
//! then runs by attention, newest change first. A workspace path reads as the
//! name it was registered under.
//!
//! Runs a schedule or a rule started, once they have ended, fold into one
//! line per job — the name, how the last few went, how many there have been,
//! and what the last one came to — so a job firing every thirty seconds is
//! one row, not a screen, and it never grows into one: its firings are in the
//! pane. A run still going, or waiting on you, is never folded.
//!
//! Before all of it, the questions waiting on you, from every project, oldest
//! first — what needs you does not hide under a project's name — then pinned
//! runs, in the order they were pinned.

use super::query::{Named, Query, Subject};
use crate::rank;
use jiff::{SignedDuration, Timestamp};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::rule::{Rule, RuleKind, RuleStatus};
use rl_proto::types::{Cause, Checkout, Confidence, Run, RunKind, RunState};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Ended runs stay on the list this long.
pub const RECENT: SignedDuration = SignedDuration::from_hours(24);

/// Which slice of the list you are looking at. Not exclusive: a scheduled
/// agent is both an agent and a job, and shows under either — the lens is the
/// question you are asking, not a partition. Questions ignore it; they are
/// what needs you, whatever slice you are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lens {
    #[default]
    All,
    Agents,
    Jobs,
    Reactions,
}

impl Lens {
    /// In the order the strip lists them, and cycles through them.
    pub const ALL: [Lens; 4] = [Lens::All, Lens::Agents, Lens::Jobs, Lens::Reactions];

    pub fn label(self) -> &'static str {
        match self {
            Lens::All => "all",
            Lens::Agents => "agents",
            Lens::Jobs => "jobs",
            Lens::Reactions => "reactions",
        }
    }

    /// Whether a run belongs in this lens — by what it is (agents) or by what
    /// started it (jobs are scheduled, reactions are event-driven).
    fn keeps(self, r: &Run) -> bool {
        match self {
            Lens::All => true,
            Lens::Agents => r.kind == RunKind::Agent,
            Lens::Jobs => matches!(r.cause, Cause::Schedule(_)),
            Lens::Reactions => matches!(r.cause, Cause::Reaction(_)),
        }
    }
}

pub enum Row<'a> {
    /// A group: where it is, what it is called, and the rules that live there.
    Group(Head),
    Question(&'a Decision),
    Run(&'a DigestItem),
    /// A schedule's or rule's ended runs, as one line.
    Job(Job<'a>),
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
            Row::Job(j) => Some(j.key.clone()),
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
    /// The rules of that folder, needing a yes first, then by name.
    pub rules: Vec<Chip>,
    /// The folder's git checkout, when it is in one.
    pub checkout: Option<Checkout>,
}

impl Head {
    /// The `everywhere` group's header: no folder, and not a project.
    pub fn everywhere() -> Self {
        Self {
            place: EVERYWHERE.into(),
            workspace: PathBuf::new(),
            name: EVERYWHERE.into(),
            own: None,
            rules: Vec::new(),
            checkout: None,
        }
    }

    /// Whether this is the `everywhere` group, which has no folder to act on.
    pub fn is_everywhere(&self) -> bool {
        self.workspace.as_os_str().is_empty()
    }
}

/// The group, and the word, for the global rules' runs outside every project
/// — the headline's word for those rules. It comes after the projects.
pub const EVERYWHERE: &str = "everywhere";
/// Its key among the groups: nothing a folder or a ref could be.
const EVERYWHERE_KEY: &str = "\u{0}everywhere";

/// Whether a run is a global rule's doing outside every project: started by
/// a schedule or reaction of the Relay home's (and not by one of the same
/// name in its own folder), in a folder that is not a registered project, and
/// filed under no ref. Its group is `everywhere`, not a path like `~`.
pub fn everywhere_run(r: &Run, names: &Names, rules: &[Rule]) -> bool {
    let (kind, name) = match &r.cause {
        Cause::Schedule(n) => (RuleKind::Schedule, n.as_str()),
        Cause::Reaction(n) => (RuleKind::Reaction, n.as_str()),
        _ => return false,
    };
    if r.ref_.is_some() || names.registered(&r.workspace).is_some() {
        return false;
    }
    let named: Vec<&Rule> = rules.iter().filter(|x| x.kind() == kind && x.name() == name).collect();
    named.iter().any(|x| x.is_global())
        && !named
            .iter()
            .any(|x| !x.is_global() && Path::new(x.scope()) == r.workspace)
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

/// A rule on a group's header: `↻ nightly 4h`, `⚡ tests-failed`, `! deploy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub name: String,
    pub kind: RuleKind,
    pub status: RuleStatus,
    /// A schedule that is on: how long until it fires.
    pub due: Option<String>,
}

impl Chip {
    pub fn needs_yes(&self) -> bool {
        self.status == RuleStatus::NeedsApproval
    }

    pub fn text(&self) -> String {
        let glyph = match self.kind {
            RuleKind::Schedule => "↻",
            RuleKind::Reaction => "⚡",
        };
        match (&self.status, &self.due) {
            (RuleStatus::NeedsApproval, _) => format!("! {}", self.name),
            (RuleStatus::Paused, _) => format!("{glyph} {} paused", self.name),
            (_, Some(d)) => format!("{glyph} {} {d}", self.name),
            (_, None) => format!("{glyph} {}", self.name),
        }
    }
}

/// The rules of one folder (`None`: global) as chips.
pub fn chips(rules: &[Rule], scope: Option<&Path>, now: Timestamp) -> Vec<Chip> {
    let mut out: Vec<Chip> = rules
        .iter()
        .filter(|r| r.removed_at().is_none())
        .filter(|r| match scope {
            Some(p) => !r.is_global() && Path::new(r.scope()) == p,
            None => r.is_global(),
        })
        .map(|r| Chip {
            name: r.name().to_string(),
            kind: r.kind(),
            status: r.status(),
            due: match (r.kind(), r.status(), r.next_at()) {
                (RuleKind::Schedule, RuleStatus::On, Some(t)) if t > now => Some(fmt_age(now, t)),
                (RuleKind::Schedule, RuleStatus::On, Some(_)) => Some("due".into()),
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

/// The icon slot at the left of every row: what a thing is, in two columns —
/// how it stands is its word's, and its colour. Only emoji that are wide by
/// default (no variation selector), so every width table, ratatui's and
/// tmux's alike, puts the title after them in the same column.
pub const AGENT: &str = "🤖";
pub const SHELL: &str = "🐚";
pub const COMMAND: &str = "🔧";
/// A schedule's or a rule's ended runs, folded into one row.
pub const JOB: &str = "⏰";
/// A question waiting on you.
pub const QUESTION: &str = "❓";

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

/// A run's icon: an agent, a shell, or a command (a plugin is one).
pub fn icon(r: &Run) -> &'static str {
    if is_shell(r) {
        return SHELL;
    }
    match r.kind {
        RunKind::Agent => AGENT,
        RunKind::Shell => SHELL,
        RunKind::Command | RunKind::Plugin => COMMAND,
    }
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

/// `s` in exactly `w` columns: padded, or cut with `…`.
pub fn fit(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        format!("{s:<w$}")
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}

/// A schedule's or rule's ended runs, folded: what the job is and how it has
/// been going. `runs` is newest first.
#[derive(Clone)]
pub struct Job<'a> {
    /// `j:<kind>:<name>@<group key>`.
    pub key: String,
    pub kind: RuleKind,
    pub name: String,
    /// The job's full run history (not just what a filter happened to match),
    /// newest first. Invariant: never empty — a `Job` is always built with at
    /// least the run that first revealed it.
    pub runs: Vec<&'a DigestItem>,
    /// `every 30s`, `on run.failed` — when the rule is known.
    pub when: Option<String>,
}

impl<'a> Job<'a> {
    pub fn last(&self) -> &'a DigestItem {
        debug_assert!(!self.runs.is_empty(), "Job::runs must never be empty");
        self.runs[0]
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
    /// The job ranks as its last run did.
    pub fn score(&self) -> u8 {
        rank::score(&self.last().run)
    }
    /// What the last firing came to, in its own words — `5 feedback rows to
    /// process`, or `exit 1 · 2s` where it failed. The cadence and the tally
    /// are the pane's; the marks say how it has been going.
    pub fn summary(&self) -> String {
        self.last().summary.clone()
    }
    /// One mark per run, oldest first: `·` went well, `×` failed, `○` stopped.
    pub fn marks(&self, n: usize) -> String {
        self.runs
            .iter()
            .take(n)
            .rev()
            .map(|i| match i.run.state {
                RunState::Failed => '×',
                RunState::Stopped => '○',
                _ => '·',
            })
            .collect()
    }
}

/// Which job an ended run folds into, if any.
fn job_of(r: &Run) -> Option<(RuleKind, &str)> {
    if !r.state.is_terminal() {
        return None;
    }
    match &r.cause {
        Cause::Schedule(n) => Some((RuleKind::Schedule, n)),
        Cause::Reaction(n) => Some((RuleKind::Reaction, n)),
        _ => None,
    }
}

pub fn job_key(kind: RuleKind, name: &str, group: &str) -> String {
    format!("j:{kind}:{name}@{group}")
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
        Cause::Schedule(n) => format!("schedule {n}"),
        Cause::Decision(_) => "your answer".into(),
        Cause::Reaction(n) => format!("rule {n}"),
        Cause::Workflow { name, step } => format!("workflow {name} · {step}"),
        Cause::Assistant(job) => format!("asked, to {job}"),
    }
}

/// Who asked a question, in words.
pub fn asked_by(d: &Decision, run: Option<&Run>) -> String {
    if let Some(r) = run {
        return r.title.clone();
    }
    match d.group.as_deref().and_then(|g| g.strip_prefix("reaction:")) {
        Some(rule) => format!("rule {rule}"),
        None => "you".into(),
    }
}

struct Group<'a> {
    key: String,
    /// The folder of whatever came first; a group's members share one.
    workspace: PathBuf,
    runs: Vec<&'a DigestItem>,
    jobs: Vec<Job<'a>>,
}

/// What the list is built from besides the digest.
pub struct Context<'a> {
    pub names: &'a Names,
    /// Every rule, for a job's cadence; may be empty.
    pub rules: &'a [Rule],
    pub filter: &'a str,
    /// Which slice of the list to show; questions ignore it.
    pub lens: Lens,
    pub now: Timestamp,
}

/// What a search matches a run on: its words, its project, what state it is
/// in, what it is, and the rule behind it.
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
    if i.unseen {
        is.push("new");
    }
    let mut kind = vec![match r.kind {
        RunKind::Agent => "agent",
        RunKind::Shell => "shell",
        RunKind::Command | RunKind::Plugin => "command",
    }];
    let by = match &r.cause {
        Cause::Schedule(n) => {
            kind.push("job");
            Some(n.as_str())
        }
        Cause::Reaction(n) => {
            kind.push("reaction");
            Some(n.as_str())
        }
        _ => None,
    };
    Subject {
        text: vec![&r.title, r.cli.as_deref().unwrap_or(""), &i.summary],
        project: vec![project, r.workspace.to_str().unwrap_or("")],
        is,
        kind,
        by: by.into_iter().collect(),
        on: r.branch.as_deref(),
    }
}

/// A row as the search ranks it (`query::ranked`): a header by its name, the
/// rest by title, project and summary.
pub fn named(row: &Row, d: &Digest, names: &Names) -> Named {
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
            summary: i.summary.clone(),
            rest: vec![i.run.cli.clone().unwrap_or_default(), path(&i.run.workspace)],
            ..Named::default()
        },
        Row::Job(j) => {
            let r = &j.last().run;
            Named {
                title: j.name.clone(),
                project: names.group(&r.place_key()),
                summary: j.summary(),
                rest: vec![r.title.clone(), r.cli.clone().unwrap_or_default(), path(&r.workspace)],
                ..Named::default()
            }
        }
    }
}

/// Everything on the list, in order, narrowed to what the filter — a search,
/// see `query` — hits.
pub fn build<'a>(d: &'a Digest, cx: &Context<'_>) -> Vec<Row<'a>> {
    let names = cx.names;
    let now = cx.now;
    let query = Query::parse(cx.filter);
    let mut groups: Vec<Group<'a>> = Vec::new();
    let group_of = |key: String, workspace: &Path, groups: &mut Vec<Group<'a>>| -> usize {
        match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    workspace: workspace.to_path_buf(),
                    runs: Vec::new(),
                    jobs: Vec::new(),
                });
                groups.len() - 1
            }
        }
    };
    let when_of = |kind: RuleKind, name: &str, r: &Run| -> Option<String> {
        cx.rules
            .iter()
            .find(|x| x.kind() == kind && x.name() == name && (x.is_global() || Path::new(x.scope()) == r.workspace))
            .map(|x| x.when())
    };
    let run_by_id = |id: &str| d.items.iter().map(|i| &i.run).find(|r| r.id == id);
    // A job's remembered history must be the job's FULL history, independent
    // of the filter — the filter only decides whether the job row is shown at
    // all. So jobs are accumulated here first, unconditionally, and folded
    // into their group afterwards only if the job or one of its own runs hit.
    struct JobAcc<'a> {
        key: String,
        kind: RuleKind,
        name: String,
        group_key: String,
        workspace: PathBuf,
        when: Option<String>,
        runs: Vec<&'a DigestItem>,
        hit: bool,
    }
    let mut jobs_acc: Vec<JobAcc<'a>> = Vec::new();
    // Pinned runs stand above everything, however old, whatever the lens —
    // kept on purpose, so neither a day nor a slice takes them away. Only the
    // filter does, as with anything else.
    let mut pinned: Vec<&'a DigestItem> = Vec::new();
    // Questions lead the list, whichever project asked them.
    let mut asked: Vec<&'a Decision> = Vec::new();
    for q in &d.decisions {
        let asker = q.run.as_deref().and_then(run_by_id);
        let key = asker
            .map(Run::place_key)
            .unwrap_or_else(|| q.workspace.to_string_lossy().into_owned());
        let name = names.group(&key);
        let by = q
            .group
            .as_deref()
            .and_then(|g| g.strip_prefix("reaction:"))
            .or(asker.and_then(|r| match &r.cause {
                Cause::Schedule(n) | Cause::Reaction(n) => Some(n.as_str()),
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
        if query.hits(&subject) {
            asked.push(q);
        }
    }
    for i in &d.items {
        let r = &i.run;
        if r.pinned_at.is_some() {
            if query.hits(&run_subject(i, &names.group(&r.place_key()))) {
                pinned.push(i);
            }
            continue;
        }
        let live = !r.state.is_terminal() || i.unseen || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT);
        if !live {
            continue;
        }
        if !cx.lens.keeps(r) {
            continue;
        }
        let (key, name, workspace) = if everywhere_run(r, names, cx.rules) {
            (EVERYWHERE_KEY.to_string(), EVERYWHERE.to_string(), PathBuf::new())
        } else {
            let key = r.place_key();
            let name = names.group(&key);
            (key, name, r.workspace.clone())
        };
        if let Some((kind, job)) = job_of(r) {
            let mut subject = run_subject(i, &name);
            subject.text.push(job);
            let item_hit = query.hits(&subject);
            let jkey = job_key(kind, job, &key);
            match jobs_acc.iter_mut().find(|acc| acc.key == jkey) {
                Some(acc) => {
                    acc.runs.push(i);
                    acc.hit = acc.hit || item_hit;
                }
                None => jobs_acc.push(JobAcc {
                    key: jkey,
                    kind,
                    name: job.to_string(),
                    group_key: key.clone(),
                    workspace: workspace.clone(),
                    when: when_of(kind, job, r),
                    runs: vec![i],
                    hit: item_hit,
                }),
            }
            continue;
        }
        if !query.hits(&run_subject(i, &name)) {
            continue;
        }
        let g = group_of(key, &workspace, &mut groups);
        groups[g].runs.push(i);
    }
    // Fold each job into its group now, with its full history, but only if
    // the job (or one of its own runs) actually matched the filter.
    for acc in jobs_acc {
        if !acc.hit {
            continue;
        }
        let g = group_of(acc.group_key, &acc.workspace, &mut groups);
        groups[g].jobs.push(Job {
            key: acc.key,
            kind: acc.kind,
            name: acc.name,
            runs: acc.runs,
            when: acc.when,
        });
    }
    for g in &mut groups {
        g.runs.sort_by(|a, b| {
            rank::score(&a.run)
                .cmp(&rank::score(&b.run))
                .then_with(|| b.at.cmp(&a.at))
        });
        for j in &mut g.jobs {
            j.runs.sort_by_key(|i| std::cmp::Reverse(i.at));
        }
        g.jobs
            .sort_by(|a, b| a.score().cmp(&b.score()).then_with(|| b.last().at.cmp(&a.last().at)));
    }
    // Which folders already have a line, so idle projects don't double up.
    let shown: HashSet<PathBuf> = groups.iter().map(|g| g.workspace.clone()).collect();
    pinned.sort_by_key(|i| (i.run.pinned_at, i.run.id.clone()));
    asked.sort_by_key(|q| (q.created_at, q.id.clone()));
    let mut rows: Vec<Row<'a>> = asked.into_iter().map(Row::Question).collect();
    rows.extend(pinned.into_iter().map(Row::Run));
    // Projects in order of name, so each keeps its place as work comes and
    // goes: what needs you says so with its marks and in the headline, not by
    // jumping the queue. Each block is a project's header and what is under it.
    let mut blocks: Vec<(String, String, Vec<Row<'a>>)> = Vec::new();
    let mut everywhere: Vec<Row<'a>> = Vec::new();
    for g in groups {
        let name = names.group(&g.key);
        let head = if g.key == EVERYWHERE_KEY {
            Head::everywhere()
        } else {
            Head {
                place: names.place(&g.key, &g.workspace),
                workspace: g.workspace.clone(),
                name: name.clone(),
                own: names.own(&g.key, &g.workspace),
                rules: chips(cx.rules, Some(&g.workspace), now),
                checkout: checkout_of(&d.checkouts, &g.workspace).cloned(),
            }
        };
        let mut block = vec![Row::Group(head)];
        // Runs and jobs share one order: whatever asks for you leads, then the
        // folder's shells — where you work, rather than something that happens
        // to be idle — then the rest, newest first.
        let cls = |score: u8, shell: bool| match (score, shell) {
            (0, _) => 0u8,
            (_, true) => 1,
            _ => 2,
        };
        let mut mixed: Vec<(u8, u8, Timestamp, Row<'a>)> = g
            .runs
            .into_iter()
            .map(|i| {
                let s = rank::score(&i.run);
                (cls(s, live_shell(&i.run)), s, i.at, Row::Run(i))
            })
            .collect();
        mixed.extend(g.jobs.into_iter().map(|j| {
            let s = j.score();
            (cls(s, false), s, j.last().at, Row::Job(j))
        }));
        mixed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| b.2.cmp(&a.2)));
        for (_, _, _, row) in mixed {
            block.push(row);
        }
        if g.key == EVERYWHERE_KEY {
            everywhere = block;
        } else {
            blocks.push((name.to_lowercase(), g.key, block));
        }
    }
    // A registered folder is always on the list, under every lens, even before
    // anything has run there — so you can select it, read its stats, edit its
    // rules or open its shell. A lens narrows what is shown inside projects,
    // never which projects there are; only the filter takes them away.
    for p in names.paths() {
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
        let head = Row::Group(Head {
            place,
            workspace: p.clone(),
            name: name.clone(),
            own: names.own(&p.to_string_lossy(), &p),
            rules: chips(cx.rules, Some(&p), now),
            checkout: checkout.cloned(),
        });
        blocks.push((name.to_lowercase(), p.to_string_lossy().into_owned(), vec![head]));
    }
    blocks.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    rows.extend(blocks.into_iter().flat_map(|(_, _, block)| block));
    // The global rules — the Relay home's, which apply in every folder — have
    // no row of their own: the headline says them (`everywhere`), and the
    // graph is where they are wired. Their runs fold under the project they
    // ran in; what ran outside every project is gathered after the projects,
    // under one `everywhere`, rather than under whatever folder that was.
    rows.extend(everywhere);
    rows
}

/// `(questions, need you, working, finished, jobs)` over what the list shows.
/// A job counts once, and as needing you when its last run failed.
pub fn counts(rows: &[Row]) -> (usize, usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0, 0);
    for r in rows {
        match r {
            Row::Group(..) => {}
            Row::Question(_) => c.0 += 1,
            Row::Run(i) => {
                use rl_proto::types::RunState::*;
                match i.run.state {
                    NeedsInput | Blocked | Failed => c.1 += 1,
                    Starting | Working | Stale | Idle => c.2 += 1,
                    Finished | Stopped => c.3 += 1,
                }
            }
            Row::Job(j) => {
                c.4 += 1;
                if j.last().run.state == RunState::Failed {
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

/// The global rules, as the headline says them: how many schedules and
/// reactions the Relay home holds — `↻ 2 ⚡ 1 everywhere` — and, when any
/// waits for a yes, that it does and where to give it. `chips` are the global
/// ones (`chips(rules, None, ..)`); the flag says one is waiting, which is
/// drawn amber. Nothing when there are none: a home with no rules of its own
/// has nothing to say here.
pub fn everywhere(chips: &[Chip]) -> Option<(String, bool)> {
    if chips.is_empty() {
        return None;
    }
    let jobs = chips.iter().filter(|c| c.kind == RuleKind::Schedule).count();
    let reactions = chips.len() - jobs;
    let waiting = chips.iter().filter(|c| c.needs_yes()).count();
    let mut s = String::new();
    if jobs > 0 {
        s.push_str(&format!("↻ {jobs} "));
    }
    if reactions > 0 {
        s.push_str(&format!("⚡ {reactions} "));
    }
    s.push_str("everywhere");
    let need = |n: usize| if n == 1 { "needs" } else { "need" };
    match waiting {
        0 => {}
        // All of them wait: the count already says how many.
        n if n == chips.len() => s.push_str(&format!(" {} a yes · /graph", need(n))),
        n => s.push_str(&format!(" · {n} {} a yes · /graph", need(n))),
    }
    Some((s, waiting > 0))
}

/// The header line: what needs you first, then what is moving.
pub fn headline(rows: &[Row], news: usize, filter: &str) -> String {
    let (asked, need, working, fin, jobs) = counts(rows);
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
    if jobs > 0 {
        parts.push(format!("{jobs} job{}", if jobs == 1 { "" } else { "s" }));
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
    use rl_proto::types::{Confidence, RunKind, RunState};

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
        }
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
            flow: id.into(),
            group: Some("reaction:tests-failed".into()),
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

    fn build_with<'a>(d: &'a Digest, names: &Names, filter: &str, now: Timestamp) -> Vec<Row<'a>> {
        build(
            d,
            &Context {
                names,
                rules: &[],
                lens: Lens::All,
                filter,
                now,
            },
        )
    }

    fn lens_build<'a>(d: &'a Digest, names: &Names, lens: Lens, now: Timestamp) -> Vec<Row<'a>> {
        build(
            d,
            &Context {
                names,
                rules: &[],
                lens,
                filter: "",
                now,
            },
        )
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
        let mut sched = item("sched", RunState::Working, web, at, false);
        sched.run.kind = RunKind::Command;
        sched.run.cause = Cause::Schedule("nightly".into());
        let d = Digest {
            since: now,
            items: vec![agent, failed, sched],
            decisions: vec![question("q1", api, None, at)],
            checkouts: Vec::new(),
            lost: Vec::new(),
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
        assert_eq!(shown("in:api"), ["q:q1", "[api]", "r:agent"]);
        assert_eq!(shown("idle"), ["[idle]"]);
        // Tokens pick by what a thing is, and take no bare project along.
        assert_eq!(shown("is:failed"), ["[web]", "r:failed"]);
        assert_eq!(shown("kind:agent"), ["[api]", "r:agent"]);
        assert_eq!(shown("kind:question"), ["q:q1"], "a question stands above the projects");
        assert_eq!(shown("by:nightly"), ["[web]", "r:sched"]);
        assert_eq!(shown("by:tests"), ["q:q1"], "a question by its reaction");
        // The global rules have no row to find.
        assert_eq!(shown("global"), Vec::<String>::new());
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
        };
        let names = Names::new([
            (PathBuf::from(api), "api".to_string()),
            (PathBuf::from(web), "web".to_string()),
        ]);
        let best = |f: &str| {
            let rows = build_with(&d, &names, f, now);
            let named: Vec<Named> = rows.iter().map(|r| named(r, &d, &names)).collect();
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
    fn a_lens_keeps_a_run_by_what_it_is_or_what_started_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let ws = "/home/f/relay";
        let at = "2026-09-10T11:00:00Z";
        let agent = item("agent", RunState::Working, ws, at, false);
        let mut shell = item("shell", RunState::Working, ws, at, false);
        // What a warm shell really is: the `sh` profile's kind wins over the
        // `Command` hint it was started with. Either way it is not an agent.
        shell.run.kind = RunKind::Shell;
        let mut sched = item("sched", RunState::Working, ws, at, false);
        sched.run.cause = Cause::Schedule("nightly".into());
        let mut react = item("react", RunState::Working, ws, at, false);
        react.run.cause = Cause::Reaction("on-fail".into());
        let d = Digest {
            since: now,
            items: vec![agent, shell, sched, react],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
        let runs = |lens| {
            lens_build(&d, &names, lens, now)
                .iter()
                .filter_map(|r| r.item().map(|i| i.run.id.clone()))
                .collect::<Vec<_>>()
        };
        // The shell leads its folder: where you work comes before what merely
        // runs there. The rest keep the order they were in.
        assert_eq!(runs(Lens::All), ["shell", "agent", "sched", "react"]);
        // Agents is by kind, so it keeps the scheduled and reacted agents too.
        assert_eq!(runs(Lens::Agents), ["agent", "sched", "react"]);
        // Jobs and reactions are by cause.
        assert_eq!(runs(Lens::Jobs), ["sched"]);
        assert_eq!(runs(Lens::Reactions), ["react"]);
    }

    #[test]
    fn pinned_runs_stand_above_everything_however_old_whatever_the_lens() {
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
        };
        let names = Names::new([
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/api"), "api".to_string()),
        ]);
        // In the order they were pinned, after only the questions; a week-old
        // run stays, and its folder keeps its place by name.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["q:q1", "r:old01", "r:shell", "[api]", "[relay]", "r:fail1"]
        );
        // No lens takes a pin away.
        let jobs = lens_build(&d, &names, Lens::Jobs, now);
        assert_eq!(&keys(&jobs)[..3], ["q:q1", "r:old01", "r:shell"]);
        // The filter does, and matches a pin by its folder too.
        assert_eq!(keys(&build_with(&d, &names, "api", now)), ["r:old01", "[api]"]);
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
        };
        let names = Names::default();
        let rows = build_with(&d, &names, "", now);
        // Two repos on `main` stay two projects, each named for its folder.
        assert_eq!(keys(&rows), ["[api]", "r:a1", "r:a2", "[web]", "r:w1"]);
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
    fn questions_lead_the_list_and_projects_go_by_name() {
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
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["q:q1", "[api]", "r:blok1", "r:work1", "[relay]", "r:fail1", "r:done1"],
            "projects by name, however urgent; the question leads the list; api is named by its folder; old01 is gone"
        );
        assert_eq!(counts(&rows), (1, 2, 1, 1, 0));
        match &rows[4] {
            Row::Group(h) => assert_eq!((h.name.as_str(), h.place.as_str()), ("relay", "/home/f/relay")),
            _ => panic!("relay's header"),
        }
        assert_eq!(
            headline(&rows, 2, ""),
            "relay · 1 to decide · 2 need you · 1 working · 1 finished · 2 new"
        );

        let rows = build_with(&d, &names, "api", now);
        assert_eq!(keys(&rows), ["[api]", "r:blok1", "r:work1"]);
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
        // A lens narrows what is inside projects, never which projects there are:
        // the idle one and yours stay, even where nothing in them matches.
        assert_eq!(keys(&lens_build(&d, &names, Lens::Jobs, now)), ["[idle]", "[relay]"]);
    }

    #[test]
    fn with_no_runs_and_no_folders_the_list_is_empty() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = Digest {
            since: now,
            items: vec![],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let rules = [schedule("global", "pulse", true, Some("2026-09-10T12:00:30Z"))];
        let rows = build(
            &d,
            &Context {
                names: &Names::default(),
                rules: &rules,
                lens: Lens::All,
                filter: "",
                now,
            },
        );
        // Global rules or none, nothing stands on the list for them, so a
        // fresh home says it has nothing running.
        assert!(rows.is_empty(), "{:?}", keys(&rows));
    }

    #[test]
    fn the_headline_says_the_global_rules_and_a_yes_they_wait_for() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let say = |rules: &[Rule]| everywhere(&chips(rules, None, now));
        let pulse = schedule("global", "pulse", true, Some("2026-09-10T12:00:30Z"));
        let nightly = schedule("global", "nightly", true, None);
        let notify = reaction("global", "notify");
        let mut waiting = reaction("global", "deploy");
        if let Rule::Reaction(r) = &mut waiting {
            r.approved_hash = None;
        }
        assert_eq!(say(&[]), None, "no global rules, nothing said");
        // A folder's rules are its header's, not the headline's.
        assert_eq!(say(&[schedule("/home/f/relay", "nightly", true, None)]), None);
        assert_eq!(
            say(&[pulse.clone(), nightly.clone(), notify.clone()]),
            Some(("↻ 2 ⚡ 1 everywhere".into(), false))
        );
        assert_eq!(
            say(std::slice::from_ref(&notify)),
            Some(("⚡ 1 everywhere".into(), false))
        );
        // One waiting, and it is all there is: the count already says which.
        assert_eq!(
            say(std::slice::from_ref(&waiting)),
            Some(("⚡ 1 everywhere needs a yes · /graph".into(), true))
        );
        assert_eq!(
            say(&[pulse, nightly, notify, waiting]),
            Some(("↻ 2 ⚡ 2 everywhere · 1 needs a yes · /graph".into(), true))
        );
    }

    fn fired(id: &str, job: &str, state: RunState, at: &str, unseen: bool) -> DigestItem {
        let mut i = item(id, state, "/home/f/relay", at, unseen);
        i.run.cause = Cause::Schedule(job.into());
        i.summary = if state == RunState::Failed {
            "exit 1 · 0s".into()
        } else {
            "exit 0 · 0s".into()
        };
        i
    }

    #[test]
    fn a_jobs_ended_runs_are_one_line() {
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
        let d = Digest {
            since: now,
            items,
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        let job_key = job_key(RuleKind::Schedule, "pulse", "/home/f/relay");
        assert_eq!(
            keys(&rows),
            ["[relay]", "r:hand", &job_key, "r:go"],
            "finished before working, newest first among equals"
        );
        let Some(Row::Job(j)) = rows.iter().find(|r| matches!(r, Row::Job(_))) else {
            panic!("no job row");
        };
        assert_eq!(j.runs.len(), 31);
        assert_eq!(j.last().run.id, "p29");
        assert_eq!((j.ok(), j.failed(), j.stopped()), (30, 1, 0));
        assert!(j.unseen(), "the failed run is still news");
        assert_eq!(
            j.summary(),
            "exit 0 · 0s",
            "the last firing's own line; the tally is the marks', the cadence the pane's"
        );
        assert_eq!(j.marks(31).chars().filter(|c| *c == '×').count(), 1);
        assert!(j.marks(31).starts_with('×'), "oldest first: {}", j.marks(31));
        assert_eq!(counts(&rows), (0, 0, 1, 1, 1));
        assert_eq!(headline(&rows, 1, ""), "relay · 1 working · 1 finished · 1 job · 1 new");

        // Never opened: whatever happens, a job is one line.
        let opened_rows = build_with(&d, &names, "", now);
        assert_eq!(keys(&opened_rows), keys(&rows), "no row grows into its runs");

        // The filter finds a job by its name.
        let rows = build_with(&d, &names, "puls", now);
        assert_eq!(keys(&rows), ["[relay]", &job_key], "{:?}", keys(&rows));

        // A job whose last run failed ranks as failed and says so.
        let d2 = Digest {
            since: now,
            items: vec![
                fired("a", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false),
                fired("b", "pulse", RunState::Failed, "2026-09-10T11:30:00Z", true),
                item("w", RunState::Working, "/home/f/relay", "2026-09-10T11:40:00Z", false),
            ],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let rows = build_with(&d2, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", &job_key, "r:w"],
            "failed job above working run"
        );
        assert_eq!(counts(&rows), (0, 1, 1, 0, 1));
    }

    #[test]
    fn a_job_filtered_by_one_runs_text_keeps_its_full_history() {
        // A filter that matches only one run's own summary (not the job or
        // group name) must still show the job's FULL history — not just the
        // matching run — since `ok()`/`failed()`/`marks()`/`summary()` are
        // all computed from `Job.runs`.
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
        let d = Digest {
            since: now,
            items,
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        // "exit 1" only appears in the failing run's own summary, not in the
        // job name ("pulse") or the group name ("relay").
        let rows = build_with(&d, &names, "exit 1", now);
        let job_key = job_key(RuleKind::Schedule, "pulse", "/home/f/relay");
        assert_eq!(keys(&rows), ["[relay]", &job_key], "{:?}", keys(&rows));
        let Some(Row::Job(j)) = rows.iter().find(|r| matches!(r, Row::Job(_))) else {
            panic!("no job row");
        };
        assert_eq!(j.runs.len(), 31, "the job's full history, not just the matching run");
        assert_eq!(
            (j.ok(), j.failed(), j.stopped()),
            (30, 1, 0),
            "stats reflect the job's true, overwhelmingly-successful history"
        );
    }

    #[test]
    fn words_for_causes_and_askers() {
        assert_eq!(cause_words(&Cause::Manual), "started by hand");
        assert_eq!(cause_words(&Cause::Schedule("nightly".into())), "schedule nightly");
        assert_eq!(cause_words(&Cause::Reaction("notify".into())), "rule notify");
        let q = question("q", "/w", None, "2026-09-10T11:01:00Z");
        assert_eq!(asked_by(&q, None), "rule tests-failed");
        let run = item("r", RunState::Failed, "/w", "2026-09-10T11:00:00Z", false).run;
        assert_eq!(asked_by(&q, Some(&run)), "title r");
        let mut plain = q.clone();
        plain.group = None;
        assert_eq!(asked_by(&plain, None), "you");
    }

    fn schedule(scope: &str, name: &str, approved: bool, next: Option<&str>) -> Rule {
        use rl_proto::action::Task;
        use rl_proto::schedule::{Schedule, ScheduleSpec};
        Rule::Schedule(Schedule {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/h/schedules.yaml".into(),
            spec: ScheduleSpec {
                every: Some("4h".into()),
                run: Some(Task::Shell("make test".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: None,
            last_run: None,
            next_at: next.map(|t| t.parse().unwrap()),
            removed_at: None,
            approved_hash: approved.then(|| "h".into()),
        })
    }

    fn reaction(scope: &str, name: &str) -> Rule {
        use rl_proto::action::Task;
        use rl_proto::reaction::{Reaction, ReactionSpec};
        Rule::Reaction(Reaction {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/h/reactions.yaml".into(),
            spec: ReactionSpec {
                on: vec!["run.failed".into()],
                run: Some(Task::Shell("say no".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: None,
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
    }

    #[test]
    fn a_groups_header_carries_its_folders_rules() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let rules = vec![
            schedule("/home/f/relay", "nightly", true, Some("2026-09-10T16:00:00Z")),
            reaction("/home/f/relay", "tests-failed"),
            schedule("/home/f/relay", "deploy", false, None),
            reaction("/home/f/api", "elsewhere"),
            schedule("global", "pulse", true, Some("2026-09-10T12:00:30Z")),
        ];
        let texts = |c: &[Chip]| c.iter().map(Chip::text).collect::<Vec<_>>();
        assert_eq!(
            texts(&chips(&rules, Some(Path::new("/home/f/relay")), now)),
            ["! deploy", "↻ nightly 4h", "⚡ tests-failed"],
            "what needs a yes first, then by name; api's rule stays out"
        );
        assert_eq!(texts(&chips(&rules, None, now)), ["↻ pulse 30s"]);

        // On the list: the folder's rules ride its header; the global
        // rules stand nowhere on it.
        let d = Digest {
            since: now,
            items: vec![item(
                "work1",
                RunState::Working,
                "/home/f/relay",
                "2026-09-10T11:59:00Z",
                false,
            )],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let build_rules = |rules: &[Rule]| {
            let rows = build(
                &d,
                &Context {
                    names: &names,
                    rules,
                    lens: Lens::All,
                    filter: "",
                    now,
                },
            );
            rows.iter()
                .map(|r| match r {
                    Row::Group(h) => format!("[{}] {}", h.name, texts(&h.rules).join(" · ")),
                    other => other.key().unwrap(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            build_rules(&rules),
            ["[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed", "r:work1"],
            "no row for the global rules"
        );
        let mut with_yes = rules.clone();
        with_yes.push(schedule("global", "backup", false, None));
        assert_eq!(
            build_rules(&with_yes),
            ["[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed", "r:work1"],
            "not even for one waiting on a yes: the headline says that"
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
    fn every_icon_is_two_columns_wide() {
        use ratatui::text::Span;
        for i in [AGENT, SHELL, COMMAND, JOB, QUESTION] {
            assert_eq!(Span::raw(i).width(), 2, "{i}");
            assert_eq!(i.chars().count(), 1, "{i} carries no variation selector");
        }
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
    fn a_shell_leads_its_folder_but_never_over_what_asks_for_you() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let ws = "/home/f/relay";
        let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
        let mut shell = item("shell", RunState::Idle, ws, "2026-09-10T11:00:00Z", false);
        shell.run.kind = RunKind::Shell;
        let stale = item("stale", RunState::Stale, ws, "2026-09-10T11:50:00Z", false);
        let work = item("work", RunState::Working, ws, "2026-09-10T11:55:00Z", false);
        let d = Digest {
            since: now,
            items: vec![shell, stale, work],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        // The shell leads, though both of the others outrank it by state and
        // both are newer than it.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[relay]", "r:shell", "r:stale", "r:work"]
        );

        let mut shell = item("shell", RunState::Idle, ws, "2026-09-10T11:00:00Z", false);
        shell.run.kind = RunKind::Shell;
        let asks = item("asks", RunState::NeedsInput, ws, "2026-09-10T11:10:00Z", false);
        let d = Digest {
            since: now,
            items: vec![shell, asks],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        // Where you work gives way to what wants you.
        assert_eq!(keys(&build_with(&d, &names, "", now)), ["[relay]", "r:asks", "r:shell"]);
    }

    #[test]
    fn a_global_rules_runs_outside_every_project_are_everywhere() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let at = "2026-09-10T11:00:00Z";
        let (home, relay, web) = ("/home/f", "/home/f/relay", "/home/f/web");
        let by = |id: &str, state: RunState, ws: &str, cause: Cause| {
            let mut i = item(id, state, ws, at, false);
            i.run.cause = cause;
            i
        };
        let sched = |n: &str| Cause::Schedule(n.into());
        let d = Digest {
            since: now,
            items: vec![
                // The global schedule, in home and in a project.
                by("mentions", RunState::Finished, home, sched("gchat-mentions")),
                by("mentions2", RunState::Failed, home, sched("gchat-mentions")),
                by("inproj", RunState::Finished, relay, sched("gchat-mentions")),
                // A global reaction's run still going, outside every project.
                by("fix", RunState::Working, home, Cause::Reaction("look-at-it".into())),
                // A folder's own rule, of the same name as a global one.
                by("webs", RunState::Finished, web, sched("pulse")),
                // By hand, in home.
                by("hand", RunState::Working, home, Cause::Manual),
            ],
            decisions: vec![],
            checkouts: Vec::new(),
            lost: Vec::new(),
        };
        let rules = vec![
            schedule("global", "gchat-mentions", true, None),
            reaction("global", "look-at-it"),
            schedule("global", "pulse", true, None),
            schedule(web, "pulse", true, None),
        ];
        let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
        let shown = |filter: &str| {
            keys(&build(
                &d,
                &Context {
                    names: &names,
                    rules: &rules,
                    lens: Lens::All,
                    filter,
                    now,
                },
            ))
        };
        let home_name = Names::default().workspace(Path::new(home));
        let everywhere_job = job_key(RuleKind::Schedule, "gchat-mentions", EVERYWHERE_KEY);
        assert_eq!(
            shown(""),
            [
                // By hand, a run keeps its folder.
                format!("[{home_name}]"),
                "r:hand".into(),
                // A global rule's run in a project stays in the project.
                "[relay]".into(),
                job_key(RuleKind::Schedule, "gchat-mentions", relay),
                // A folder's own rule is the folder's, same name or not.
                "[web]".into(),
                job_key(RuleKind::Schedule, "pulse", web),
                // The rest, after the projects, however they sort.
                "[everywhere]".into(),
                everywhere_job.clone(),
                "r:fix".into(),
            ],
            "outside every project: everywhere, after the projects"
        );

        // It answers to its word, as a project answers to its name.
        assert_eq!(
            shown("in:everywhere"),
            ["[everywhere]".to_string(), everywhere_job, "r:fix".into()]
        );

        // Its header is no folder, and its key says so.
        let rows = build(
            &d,
            &Context {
                names: &names,
                rules: &rules,
                lens: Lens::All,
                filter: "",
                now,
            },
        );
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

        // Without the rules known, nothing is everywhere: each run keeps its folder.
        let bare = keys(&build_with(&d, &names, "", now));
        assert!(!bare.contains(&"[everywhere]".to_string()), "{bare:?}");
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
    }
}
