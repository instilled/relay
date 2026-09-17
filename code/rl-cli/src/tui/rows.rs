//! The one list: questions and runs, ranked by attention, grouped by what
//! they belong to, and named in words rather than ids and paths.
//!
//! A group is a run's ref, else its folder — never its branch: a folder is one
//! project whichever branch it is on, and its header says which. Groups come in
//! order of name, the global row last; inside a group, questions come first,
//! then runs by attention, newest change first. A workspace path reads as the
//! name it was registered under.
//!
//! Runs a schedule or a rule started, once they have ended, fold into one
//! line per job — the name, the cadence, how many went well, and the last
//! outcome — so a job firing every thirty seconds is one row, not a screen.
//! A run still going, or waiting on you, is never folded; `enter` on a job
//! opens it to its recent runs.
//!
//! Before all of it, the questions waiting on you, from every project, oldest
//! first — what needs you does not hide under a project's name — then pinned
//! runs, in the order they were pinned.

use super::query::{Query, Subject};
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

/// Ended runs a job shows when opened.
pub const OPENED: usize = 20;

pub enum Row<'a> {
    /// A group: where it is, what it is called, and the rules that live there.
    Group(Head),
    Question(&'a Decision),
    Run(&'a DigestItem),
    /// A schedule's or rule's ended runs, as one line.
    Job(Job<'a>),
    /// One of an opened job's runs, under it.
    Fold(&'a DigestItem),
}

impl<'a> Row<'a> {
    /// Selectable rows carry a stable key, so the cursor survives a re-rank. A
    /// folder header is selectable by its path; the global rules row,
    /// which stands for no folder, is selectable too, under a fixed key.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Group(h) => Some(match &h.workspace {
                Some(p) => format!("g:{}", p.display()),
                None => "g:*global*".into(),
            }),
            Row::Question(d) => Some(format!("q:{}", d.id)),
            Row::Run(i) | Row::Fold(i) => Some(format!("r:{}", i.run.id)),
            Row::Job(j) => Some(j.key.clone()),
        }
    }

    /// The run a row is, whether on its own or under a job.
    pub fn item(&self) -> Option<&'a DigestItem> {
        match self {
            Row::Run(i) | Row::Fold(i) => Some(i),
            _ => None,
        }
    }
}

/// A group's header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The folder, with `~` for home; on the global row, the Relay home.
    pub place: Option<String>,
    /// The folder itself, so actions can act on it; none on the global row.
    pub workspace: Option<PathBuf>,
    /// A ref, or a folder by the name it was registered under.
    pub name: String,
    /// The rules of that folder, needing a yes first, then by name.
    pub rules: Vec<Chip>,
    /// The folder's git checkout, when it is in one.
    pub checkout: Option<Checkout>,
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

/// The mark at the left of a run's row: `✱` needs you, `·` going, `•` ended.
pub fn glyph(state: RunState) -> &'static str {
    match state {
        RunState::NeedsInput | RunState::Blocked | RunState::Failed => "✱",
        RunState::Finished | RunState::Stopped => "•",
        _ => "·",
    }
}

/// A shell that is simply there to be typed in. One that ended, failed or
/// wants you is news like any other run and is left to read as one.
pub fn live_shell(r: &Run) -> bool {
    r.kind == RunKind::Shell && !r.state.is_terminal() && !matches!(r.state, RunState::NeedsInput | RunState::Blocked)
}

/// The mark a row wears. A shell at rest is not a run that happens to be
/// idle — it is where you work — so it wears the prompt caret.
pub fn run_glyph(r: &Run) -> &'static str {
    if live_shell(r) { "❯" } else { glyph(r.state) }
}

/// The word beside the mark. `idle` says nothing about a shell, which is
/// idle whenever you are not typing; what it is, is the useful part.
pub fn run_words(r: &Run) -> String {
    if live_shell(r) {
        "shell".to_string()
    } else {
        state_words(r.state, r.confidence)
    }
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
    /// `every 30s · ok ×118 · 1 failed · last exit 0 · 0s`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(w) = &self.when {
            parts.push(w.clone());
        }
        let ok = self.ok();
        if ok > 0 {
            parts.push(format!("ok ×{ok}"));
        }
        let failed = self.failed();
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        let stopped = self.stopped();
        if stopped > 0 {
            parts.push(format!("{stopped} stopped"));
        }
        parts.push(format!("last {}", self.last().summary));
        parts.join(" · ")
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

    /// A group key in words: a ref or branch as it is, a path by name.
    pub fn group(&self, key: &str) -> String {
        if key.starts_with('/') {
            self.workspace(Path::new(key))
        } else {
            key.to_string()
        }
    }
}

/// A path with `~` for home.
pub fn short_path(p: &Path) -> String {
    let path = p.to_string_lossy();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.into_owned(),
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
    /// Jobs opened to their runs, by key.
    pub opened: &'a HashSet<String>,
    pub filter: &'a str,
    /// The Relay home as shown (`~/.relay`): where the global row's rules live.
    pub home: &'a str,
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
        by,
        on: r.branch.as_deref(),
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
            by,
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
        let key = r.place_key();
        let name = names.group(&key);
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
                    workspace: r.workspace.clone(),
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
        let g = group_of(key, &r.workspace, &mut groups);
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
    for g in groups {
        let name = names.group(&g.key);
        let mut block = vec![Row::Group(Head {
            place: Some(names.place(&g.key, &g.workspace)),
            workspace: Some(g.workspace.clone()),
            name: name.clone(),
            rules: chips(cx.rules, Some(&g.workspace), now),
            checkout: checkout_of(&d.checkouts, &g.workspace).cloned(),
        })];
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
            let folded: Vec<&DigestItem> = match &row {
                Row::Job(j) if cx.opened.contains(&j.key) => j.runs.iter().copied().take(OPENED).collect(),
                _ => vec![],
            };
            block.push(row);
            block.extend(folded.into_iter().map(Row::Fold));
        }
        blocks.push((name.to_lowercase(), g.key, block));
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
            place: Some(place),
            workspace: Some(p.clone()),
            name: name.clone(),
            rules: chips(cx.rules, Some(&p), now),
            checkout: checkout.cloned(),
        });
        blocks.push((name.to_lowercase(), p.to_string_lossy().into_owned(), vec![head]));
    }
    blocks.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    rows.extend(blocks.into_iter().flat_map(|(_, _, block)| block));
    // The global rules get a line of their own — a project row for the things
    // that live outside any folder — always last, rules or none, so the cursor
    // has somewhere to stand to reach them. It answers to the filter like any
    // other project.
    if query.hits_project(&["global", cx.home], None) {
        rows.push(Row::Group(Head {
            place: Some(cx.home.to_string()),
            workspace: None,
            name: "global".into(),
            rules: chips(cx.rules, None, now),
            checkout: None,
        }));
    }
    rows
}

/// `(questions, need you, working, finished, jobs)` over what the list shows.
/// A job counts once, and as needing you when its last run failed.
pub fn counts(rows: &[Row]) -> (usize, usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0, 0);
    for r in rows {
        match r {
            Row::Group(..) | Row::Fold(_) => {}
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
        }
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
                Row::Fold(i) => format!("  r:{}", i.run.id),
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
                opened: &HashSet::new(),
                lens: Lens::All,
                filter,
                home: "~/.relay",
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
                opened: &HashSet::new(),
                lens,
                filter: "",
                home: "~/.relay",
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
        assert_eq!(shown("global"), ["[global]"]);
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
        };
        let names = Names::new([
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/api"), "api".to_string()),
        ]);
        // In the order they were pinned, after only the questions; a week-old
        // run stays, and its folder keeps its place by name.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["q:q1", "r:old01", "r:shell", "[api]", "[relay]", "r:fail1", "[global]"]
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
        };
        let names = Names::default();
        let rows = build_with(&d, &names, "", now);
        // Two repos on `main` stay two projects, each named for its folder.
        assert_eq!(keys(&rows), ["[api]", "r:a1", "r:a2", "[web]", "r:w1", "[global]"]);
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
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            [
                "q:q1", "[api]", "r:blok1", "r:work1", "[relay]", "r:fail1", "r:done1", "[global]"
            ],
            "projects by name, however urgent; the question leads the list; api is named by its folder; old01 is gone; global last"
        );
        assert_eq!(counts(&rows), (1, 2, 1, 1, 0));
        match &rows[4] {
            Row::Group(h) => assert_eq!((h.name.as_str(), h.place.as_deref()), ("relay", Some("/home/f/relay"))),
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
        };
        let names = Names::new([
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/idle"), "idle".to_string()),
        ]);
        // The idle project is a line of its own, in its place by name.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[idle]", "[relay]", "r:work1", "[global]"]
        );
        // It carries the folder so selecting it lands on the project.
        match build_with(&d, &names, "idle", now).last() {
            Some(Row::Group(h)) => {
                assert_eq!(h.name, "idle");
                assert_eq!(h.workspace.as_deref(), Some(Path::new("/home/f/idle")));
            }
            _ => panic!("idle folder is the last row it filters to"),
        }
        // The filter reaches idle projects too.
        assert_eq!(keys(&build_with(&d, &names, "idle", now)), ["[idle]"]);
        // A lens narrows what is inside projects, never which projects there are:
        // the idle one and yours stay, even where nothing in them matches.
        assert_eq!(
            keys(&lens_build(&d, &names, Lens::Jobs, now)),
            ["[idle]", "[relay]", "[global]"]
        );
    }

    #[test]
    fn the_global_row_is_last_and_selectable() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let d = Digest {
            since: now,
            items: vec![item(
                "w",
                RunState::Working,
                "/home/f/relay",
                "2026-09-10T11:00:00Z",
                false,
            )],
            decisions: vec![],
            checkouts: Vec::new(),
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let build_with_rules = |rules: &[Rule]| {
            build(
                &d,
                &Context {
                    names: &names,
                    rules,
                    opened: &HashSet::new(),
                    lens: Lens::All,
                    filter: "",
                    home: "~/.relay",
                    now,
                },
            )
            .into_iter()
            .map(|r| (r.key(), matches!(r, Row::Group(h) if h.workspace.is_none())))
            .collect::<Vec<_>>()
        };
        // The global row is last, under a stable key.
        let calm = build_with_rules(&[schedule("global", "pulse", true, Some("2026-09-10T12:00:30Z"))]);
        let last = calm.last().unwrap();
        assert!(last.1, "the global row is last");
        assert_eq!(last.0.as_deref(), Some("g:*global*"), "and is selectable");
        // Even with a rule waiting on a yes: its place does not move.
        let waiting = build_with_rules(&[schedule("global", "backup", false, None)]);
        assert!(waiting.last().unwrap().1, "still last");
        assert!(!waiting[0].1);
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
    fn a_jobs_ended_runs_are_one_line_until_opened() {
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
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        let job_key = job_key(RuleKind::Schedule, "pulse", "/home/f/relay");
        assert_eq!(
            keys(&rows),
            ["[relay]", "r:hand", &job_key, "r:go", "[global]"],
            "finished before working, newest first among equals"
        );
        let Some(Row::Job(j)) = rows.iter().find(|r| matches!(r, Row::Job(_))) else {
            panic!("no job row");
        };
        assert_eq!(j.runs.len(), 31);
        assert_eq!(j.last().run.id, "p29");
        assert_eq!((j.ok(), j.failed(), j.stopped()), (30, 1, 0));
        assert!(j.unseen(), "the failed run is still news");
        assert_eq!(j.summary(), "ok ×30 · 1 failed · last exit 0 · 0s");
        assert_eq!(j.marks(31).chars().filter(|c| *c == '×').count(), 1);
        assert!(j.marks(31).starts_with('×'), "oldest first: {}", j.marks(31));
        assert_eq!(counts(&rows), (0, 0, 1, 1, 1));
        assert_eq!(headline(&rows, 1, ""), "relay · 1 working · 1 finished · 1 job · 1 new");

        // Opened: its newest runs, capped, under it.
        let opened: HashSet<String> = [job_key.clone()].into_iter().collect();
        let rows = build(
            &d,
            &Context {
                names: &names,
                rules: &[],
                opened: &opened,
                lens: Lens::All,
                filter: "",
                home: "~/.relay",
                now,
            },
        );
        let k = keys(&rows);
        assert_eq!(k.len(), 5 + OPENED, "…and global trails");
        assert_eq!(k[3], "  r:p29");
        assert_eq!(k[3 + OPENED - 1], "  r:p10");
        assert_eq!(k[3 + OPENED], "r:go");

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
        };
        let rows = build_with(&d2, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", &job_key, "r:w", "[global]"],
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
        // rules get a line of their own, always.
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
        };
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let build_rules = |rules: &[Rule]| {
            let rows = build(
                &d,
                &Context {
                    names: &names,
                    rules,
                    opened: &HashSet::new(),
                    lens: Lens::All,
                    filter: "",
                    home: "~/.relay",
                    now,
                },
            );
            rows.iter()
                .map(|r| match r {
                    Row::Group(h) => format!(
                        "[{}{}] {}",
                        h.name,
                        if h.workspace.is_none() { "*" } else { "" },
                        texts(&h.rules).join(" · ")
                    ),
                    other => other.key().unwrap(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            build_rules(&rules),
            [
                "[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed",
                "r:work1",
                "[global*] ↻ pulse 30s"
            ],
            "global rules are last"
        );
        let mut with_yes = rules.clone();
        with_yes.push(schedule("global", "backup", false, None));
        assert_eq!(
            build_rules(&with_yes),
            [
                "[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed",
                "r:work1",
                "[global*] ! backup · ↻ pulse 30s"
            ],
            "a waiting yes shows on its chip, not by moving the row"
        );
    }

    #[test]
    fn a_shell_at_rest_wears_the_caret_and_says_what_it_is() {
        let mut i = item("sh", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
        i.run.kind = RunKind::Shell;
        assert!(live_shell(&i.run));
        assert_eq!(run_glyph(&i.run), "❯");
        assert_eq!(run_words(&i.run), "shell", "`idle` says nothing about a shell");
        // One that ended is news like any other ended run, and reads as one.
        i.run.state = RunState::Stopped;
        assert!(!live_shell(&i.run));
        assert_eq!((run_glyph(&i.run), run_words(&i.run)), ("•", "stopped".to_string()));
        // So is one that wants you: it keeps the mark that says so.
        i.run.state = RunState::NeedsInput;
        assert!(!live_shell(&i.run));
        assert_eq!(run_glyph(&i.run), "✱");
        // Anything that is not a shell is left exactly as it was.
        let a = item("ag", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
        assert_eq!((run_glyph(&a.run), run_words(&a.run)), ("·", "idle".to_string()));
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
        };
        // The shell leads, though both of the others outrank it by state and
        // both are newer than it.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[relay]", "r:shell", "r:stale", "r:work", "[global]"]
        );

        let mut shell = item("shell", RunState::Idle, ws, "2026-09-10T11:00:00Z", false);
        shell.run.kind = RunKind::Shell;
        let asks = item("asks", RunState::NeedsInput, ws, "2026-09-10T11:10:00Z", false);
        let d = Digest {
            since: now,
            items: vec![shell, asks],
            decisions: vec![],
            checkouts: Vec::new(),
        };
        // Where you work gives way to what wants you.
        assert_eq!(
            keys(&build_with(&d, &names, "", now)),
            ["[relay]", "r:asks", "r:shell", "[global]"]
        );
    }

    #[test]
    fn words_and_columns() {
        assert_eq!(state_words(RunState::NeedsInput, Confidence::Exact), "needs you");
        assert_eq!(state_words(RunState::Working, Confidence::Likely), "working~");
        assert_eq!(state_words(RunState::Finished, Confidence::Guess), "done?");
        assert_eq!(glyph(RunState::Blocked), "✱");
        assert_eq!(glyph(RunState::Working), "·");
        assert_eq!(glyph(RunState::Stopped), "•");
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 5), "abcd…");
        assert_eq!(fit("abcdef", 0), "");
        assert_eq!(fit("héllo wörld", 6), "héllo…");
    }
}
