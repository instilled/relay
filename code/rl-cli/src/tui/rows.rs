//! The one list: questions and runs, ranked by attention, grouped by what
//! they belong to, and named in words rather than ids and paths.
//!
//! A group is a run's group key (ref → branch → workspace). Groups come in
//! order of their most urgent member; inside a group, questions come first,
//! then runs by attention, newest change first. A workspace path reads as the
//! name it was registered under.
//!
//! Runs a schedule or a rule started, once they have ended, fold into one
//! line per job — the name, the cadence, how many went well, and the last
//! outcome — so a job firing every thirty seconds is one row, not a screen.
//! A run still going, or waiting on you, is never folded; `enter` on a job
//! opens it to its recent runs.

use crate::rank;
use jiff::{SignedDuration, Timestamp};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::rule::{Rule, RuleKind, RuleStatus};
use rl_proto::types::{Cause, Confidence, Run, RunKind, RunState};
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
    /// folder header is selectable (its path is the key); the global "yours"
    /// rules row, which stands for no folder, is not.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Group(h) => h.workspace.as_ref().map(|p| format!("g:{}", p.display())),
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
    /// The folder, with `~` for home; none on the line of your own rules.
    pub place: Option<String>,
    /// The folder itself, so actions can act on it; none for your own rules.
    pub workspace: Option<PathBuf>,
    /// A ref, a branch, or a folder's registered name.
    pub name: String,
    /// The rules of that folder, needing a yes first, then by name.
    pub rules: Vec<Chip>,
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

/// The rules of one folder (`None`: yours) as chips.
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
    pub runs: Vec<&'a DigestItem>,
    /// `every 30s`, `on run.failed` — when the rule is known.
    pub when: Option<String>,
}

impl<'a> Job<'a> {
    pub fn last(&self) -> &'a DigestItem {
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
    questions: Vec<&'a Decision>,
    runs: Vec<&'a DigestItem>,
    jobs: Vec<Job<'a>>,
}

impl Group<'_> {
    /// Lower is more urgent; a question beats any run.
    fn score(&self) -> i16 {
        if !self.questions.is_empty() {
            return -1;
        }
        self.runs
            .iter()
            .map(|i| rank::score(&i.run) as i16)
            .chain(self.jobs.iter().map(|j| j.score() as i16))
            .min()
            .unwrap_or(i16::MAX)
    }
    fn latest(&self) -> Timestamp {
        self.runs
            .iter()
            .map(|i| i.at)
            .chain(self.jobs.iter().map(|j| j.last().at))
            .chain(self.questions.iter().map(|q| q.created_at))
            .max()
            .unwrap_or(Timestamp::UNIX_EPOCH)
    }
}

/// What the list is built from besides the digest.
pub struct Context<'a> {
    pub names: &'a Names,
    /// Every rule, for a job's cadence; may be empty.
    pub rules: &'a [Rule],
    /// Jobs opened to their runs, by key.
    pub opened: &'a HashSet<String>,
    pub filter: &'a str,
    /// Which slice of the list to show; questions ignore it.
    pub lens: Lens,
    pub now: Timestamp,
}

/// Everything on the list, in order. The filter is matched case-insensitively
/// against titles, group names, CLIs and summaries.
pub fn build<'a>(d: &'a Digest, cx: &Context<'_>) -> Vec<Row<'a>> {
    let names = cx.names;
    let now = cx.now;
    let f = cx.filter.to_lowercase();
    let hit = |parts: &[&str]| f.is_empty() || parts.iter().any(|s| s.to_lowercase().contains(&f));
    let mut groups: Vec<Group<'a>> = Vec::new();
    let group_of = |key: String, workspace: &Path, groups: &mut Vec<Group<'a>>| -> usize {
        match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    workspace: workspace.to_path_buf(),
                    questions: Vec::new(),
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
    for q in &d.decisions {
        let asker = q.run.as_deref().and_then(run_by_id);
        let key = asker
            .map(Run::group_key)
            .unwrap_or_else(|| q.workspace.to_string_lossy().into_owned());
        let name = names.group(&key);
        if !hit(&[&q.title, &name, q.summary.as_deref().unwrap_or("")]) {
            continue;
        }
        let g = group_of(key, &q.workspace, &mut groups);
        groups[g].questions.push(q);
    }
    for i in &d.items {
        let r = &i.run;
        let live = !r.state.is_terminal() || i.unseen || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT);
        if !live {
            continue;
        }
        if !cx.lens.keeps(r) {
            continue;
        }
        let key = r.group_key();
        let name = names.group(&key);
        if let Some((kind, job)) = job_of(r) {
            if !hit(&[job, &name, &r.title, &i.summary]) {
                continue;
            }
            let g = group_of(key.clone(), &r.workspace, &mut groups);
            let jkey = job_key(kind, job, &key);
            match groups[g].jobs.iter_mut().find(|j| j.key == jkey) {
                Some(j) => j.runs.push(i),
                None => groups[g].jobs.push(Job {
                    key: jkey,
                    kind,
                    name: job.to_string(),
                    runs: vec![i],
                    when: when_of(kind, job, r),
                }),
            }
            continue;
        }
        if !hit(&[&r.title, &name, r.cli.as_deref().unwrap_or(""), &i.summary]) {
            continue;
        }
        let g = group_of(key, &r.workspace, &mut groups);
        groups[g].runs.push(i);
    }
    for g in &mut groups {
        g.questions.sort_by_key(|q| q.created_at);
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
    groups.sort_by(|a, b| a.score().cmp(&b.score()).then_with(|| b.latest().cmp(&a.latest())));
    let mut rows = Vec::new();
    // Your own rules get a line of their own only when one waits for a yes.
    let yours = chips(cx.rules, None, now);
    if yours.iter().any(Chip::needs_yes) {
        rows.push(Row::Group(Head {
            place: None,
            workspace: None,
            name: "yours".into(),
            rules: yours,
        }));
    }
    for g in groups {
        rows.push(Row::Group(Head {
            place: Some(names.place(&g.key, &g.workspace)),
            workspace: Some(g.workspace.clone()),
            name: names.group(&g.key),
            rules: chips(cx.rules, Some(&g.workspace), now),
        }));
        rows.extend(g.questions.into_iter().map(Row::Question));
        // Runs and jobs share one order: attention first, newest first.
        let mut mixed: Vec<(u8, Timestamp, Row<'a>)> = g
            .runs
            .into_iter()
            .map(|i| (rank::score(&i.run), i.at, Row::Run(i)))
            .collect();
        mixed.extend(g.jobs.into_iter().map(|j| (j.score(), j.last().at, Row::Job(j))));
        mixed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        for (_, _, row) in mixed {
            let folded: Vec<&DigestItem> = match &row {
                Row::Job(j) if cx.opened.contains(&j.key) => j.runs.iter().copied().take(OPENED).collect(),
                _ => vec![],
            };
            rows.push(row);
            rows.extend(folded.into_iter().map(Row::Fold));
        }
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
        s.push_str(&format!(" · /{filter}"));
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
                now,
            },
        )
    }

    #[test]
    fn a_lens_keeps_a_run_by_what_it_is_or_what_started_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let ws = "/home/f/relay";
        let at = "2026-09-10T11:00:00Z";
        let agent = item("agent", RunState::Working, ws, at, false);
        let mut shell = item("shell", RunState::Working, ws, at, false);
        shell.run.kind = RunKind::Command;
        let mut sched = item("sched", RunState::Working, ws, at, false);
        sched.run.cause = Cause::Schedule("nightly".into());
        let mut react = item("react", RunState::Working, ws, at, false);
        react.run.cause = Cause::Reaction("on-fail".into());
        let d = Digest {
            since: now,
            items: vec![agent, shell, sched, react],
            decisions: vec![],
        };
        let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
        let runs = |lens| {
            lens_build(&d, &names, lens, now)
                .iter()
                .filter_map(|r| r.item().map(|i| i.run.id.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(runs(Lens::All), ["agent", "shell", "sched", "react"]);
        // Agents is by kind, so it keeps the scheduled and reacted agents too.
        assert_eq!(runs(Lens::Agents), ["agent", "sched", "react"]);
        // Jobs and reactions are by cause.
        assert_eq!(runs(Lens::Jobs), ["sched"]);
        assert_eq!(runs(Lens::Reactions), ["react"]);
    }

    #[test]
    fn questions_lead_their_group_and_the_group_leads_the_list() {
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
        };
        let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
        let rows = build_with(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", "q:q1", "r:fail1", "r:done1", "[api]", "r:blok1", "r:work1"],
            "the question puts relay first; api is named by its folder; old01 is gone"
        );
        assert_eq!(counts(&rows), (1, 2, 1, 1, 0));
        match &rows[0] {
            Row::Group(h) => assert_eq!((h.name.as_str(), h.place.as_deref()), ("relay", Some("/home/f/relay"))),
            _ => panic!("first row is the group"),
        }
        assert_eq!(
            headline(&rows, 2, ""),
            "relay · 1 to decide · 2 need you · 1 working · 1 finished · 2 new"
        );

        let rows = build_with(&d, &names, "api", now);
        assert_eq!(keys(&rows), ["[api]", "r:blok1", "r:work1"]);
        assert_eq!(headline(&rows, 0, "api"), "relay · 1 needs you · 1 working · /api");
        assert_eq!(headline(&[], 0, ""), "relay · nothing running");
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
                now,
            },
        );
        let k = keys(&rows);
        assert_eq!(k.len(), 4 + OPENED);
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

        // On the list: the folder's rules ride its header; your own rules get
        // a line only when one of them waits for a yes.
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
                    now,
                },
            );
            rows.iter()
                .map(|r| match r {
                    Row::Group(h) => format!(
                        "[{}{}] {}",
                        h.name,
                        if h.place.is_none() { "*" } else { "" },
                        texts(&h.rules).join(" · ")
                    ),
                    other => other.key().unwrap(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            build_rules(&rules),
            ["[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed", "r:work1"]
        );
        let mut with_yes = rules.clone();
        with_yes.push(schedule("global", "backup", false, None));
        assert_eq!(
            build_rules(&with_yes),
            [
                "[yours*] ! backup · ↻ pulse 30s",
                "[relay] ! deploy · ↻ nightly 4h · ⚡ tests-failed",
                "r:work1"
            ]
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
