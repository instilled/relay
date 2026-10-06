//! The catch-up: what happened while you were away, as the list's own rows.
//!
//! The list's rows for the stretch, under the list's own headers: a question,
//! a run waiting on you, a run with news since you left — or that you have
//! not looked at yet — and a job's firings in that time; never an assistant's
//! own run, which is Relay's doing. What needs you leads each project, and
//! the projects that need you lead; a project with nothing new keeps its
//! header and folds to one line. Counted — only what happened
//! in the stretch is counted as ran or failed — they are the top without an
//! assistant; in a line each, what an assistant is handed to write it.

use crate::digest::since_items;
use crate::tui::rows::{self, Context, Head, Lens, Row};
use jiff::Timestamp;
use rl_core::time::{fmt_age, fmt_duration, fmt_when};
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::rule::Rule;
use rl_proto::types::{Cause, Run, RunState, Summary};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How many of a job's firings in the stretch its line shows as marks.
const MARKS: usize = 10;

pub struct CatchUp<'a> {
    pub since: Timestamp,
    pub projects: Vec<Project<'a>>,
    /// When it was built, for the ages on its lines.
    now: Timestamp,
}

pub struct Project<'a> {
    pub head: Head,
    /// The list's group key: a ref, a folder, or `everywhere`'s.
    key: String,
    /// Questions, runs that need you, and jobs whose last firing failed.
    pub need: usize,
    /// Runs and job firings in the stretch.
    pub ran: usize,
    /// Of those, the ones that failed.
    pub failed: usize,
    pub rows: Vec<Row<'a>>,
}

impl<'a> Project<'a> {
    fn new(head: Head, key: String) -> Self {
        Self {
            head,
            key,
            need: 0,
            ran: 0,
            failed: 0,
            rows: Vec::new(),
        }
    }

    /// When the newest thing in it happened.
    fn latest(&self) -> Option<Timestamp> {
        self.rows.iter().filter_map(at).max()
    }
}

impl<'a> CatchUp<'a> {
    /// The list's rows for the stretch, grouped as the list groups them; what
    /// needs you first, projects that need you first, a project with nothing
    /// new keeps an empty `rows`.
    pub fn build(d: &'a Digest, cx: &Context<'_>) -> Self {
        // The whole list, whatever lens it is in now but narrowed by its
        // search, with its day of ended runs counted back from when you left
        // rather than from now, so a long stretch keeps what ended early in
        // it.
        let list = Context {
            names: cx.names,
            rules: cx.rules,
            filter: cx.filter,
            lens: Lens::All,
            now: d.since,
        };
        let since = d.since;
        let news: HashSet<&str> = since_items(d).into_iter().map(|i| i.run.id.as_str()).collect();
        let news = |i: &DigestItem| news.contains(i.run.id.as_str());
        let waiting = |i: &DigestItem| matches!(i.run.state, RunState::NeedsInput | RunState::Blocked);
        let mut projects: Vec<Project<'a>> = Vec::new();
        // Whether the last header's key has been read off a row under it.
        let mut keyed = true;
        // Questions, and runs pinned or come through, stand above every
        // header on the list; here they go under their project's.
        let mut loose: Vec<Row<'a>> = Vec::new();
        for row in rows::build(d, &list) {
            if let (false, Some(p), Some(r)) = (keyed, projects.last_mut(), run_of(&row)) {
                p.key = rows::group_key(r, cx.names, cx.rules);
                keyed = true;
            }
            let row = match row {
                Row::Group(mut h) => {
                    // Its rules' times from now, not from when you left.
                    if !h.is_everywhere() {
                        h.rules = rows::chips(cx.rules, Some(&h.workspace), cx.now);
                    }
                    let key = h.workspace.to_string_lossy().into_owned();
                    projects.push(Project::new(h, key));
                    keyed = false;
                    continue;
                }
                Row::Question(q) => Row::Question(q),
                // An assistant's own run — the top being written, a recap —
                // is Relay's doing, not what happened.
                Row::Run(i) if matches!(i.run.cause, Cause::Assistant(_)) => continue,
                Row::Run(i) if news(i) || waiting(i) => Row::Run(i),
                Row::Run(_) => continue,
                Row::Job(mut j) => {
                    if !j.runs.iter().any(|i| news(i)) {
                        continue;
                    }
                    // Its marks speak for its firings in the stretch; one
                    // you have not looked at from before it keeps the row,
                    // and is all it shows when there is nothing since.
                    let stretch: Vec<&DigestItem> = j.runs.iter().copied().filter(|i| i.at >= since).collect();
                    if stretch.is_empty() {
                        j.runs.retain(|i| i.unseen);
                    } else {
                        j.runs = stretch;
                    }
                    Row::Job(j)
                }
            };
            match projects.last_mut() {
                Some(p) => p.rows.push(row),
                None => loose.push(row),
            }
        }
        for row in loose {
            let (run, ws) = match &row {
                Row::Question(q) => {
                    let asker = q.run.as_deref().and_then(|id| d.items.iter().find(|i| i.run.id == id));
                    (asker.map(|i| &i.run), q.workspace.as_path())
                }
                Row::Run(i) => (Some(&i.run), i.run.workspace.as_path()),
                Row::Group(_) | Row::Job(_) => continue,
            };
            // Under the header the list would give it: its ref's, else its
            // folder's — not another folder that goes by the same name.
            let key = match run {
                Some(r) => rows::group_key(r, cx.names, cx.rules),
                None => ws.to_string_lossy().into_owned(),
            };
            let p = match projects.iter().position(|p| p.key == key) {
                Some(p) => p,
                None => {
                    projects.push(Project::new(head_of(d, cx, run, ws), key));
                    projects.len() - 1
                }
            };
            projects[p].rows.push(row);
        }
        let ran = |i: &DigestItem| i.at >= since;
        let failed = |i: &DigestItem| ran(i) && i.run.state == RunState::Failed;
        for p in &mut projects {
            p.rows.sort_by_key(|r| (tier(r), std::cmp::Reverse(at(r))));
            for r in &p.rows {
                match r {
                    Row::Question(_) => p.need += 1,
                    Row::Run(i) => {
                        p.need += usize::from(i.run.state.needs_attention());
                        p.ran += usize::from(ran(i));
                        p.failed += usize::from(failed(i));
                    }
                    Row::Job(j) => {
                        p.need += usize::from(j.last().run.state == RunState::Failed);
                        p.ran += j.runs.iter().filter(|i| ran(i)).count();
                        p.failed += j.runs.iter().filter(|i| failed(i)).count();
                    }
                    Row::Group(_) => {}
                }
            }
        }
        projects.sort_by(|a, b| b.need.cmp(&a.need).then_with(|| b.latest().cmp(&a.latest())));
        Self {
            since: d.since,
            projects,
            now: cx.now,
        }
    }

    /// The catch-up of `d` as `rl since` builds it: the folders by the names
    /// they were registered under, the rules as they stand, nothing filtered.
    pub fn of(
        d: &'a Digest,
        workspaces: impl IntoIterator<Item = (PathBuf, String)>,
        rules: &[Rule],
        now: Timestamp,
    ) -> Self {
        let names = rows::Names::new(workspaces);
        Self::build(
            d,
            &Context {
                names: &names,
                rules,
                filter: "",
                lens: Lens::All,
                now,
            },
        )
    }

    /// The top without an assistant: a bullet per project with something to
    /// say, from counts.
    pub fn bullets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let need: usize = self.projects.iter().map(|p| p.need).sum();
        if need > 0 {
            let who: Vec<&str> = self
                .projects
                .iter()
                .filter(|p| p.need > 0)
                .map(|p| p.head.name.as_str())
                .collect();
            let s = if need == 1 { "s" } else { "" };
            out.push(format!("{need} need{s} you: {}", who.join(", ")));
        }
        for p in self.projects.iter().filter(|p| !p.rows.is_empty()) {
            let mut said = Vec::new();
            if p.ran > 0 {
                said.push(format!("{} ran", p.ran));
            }
            if p.failed > 0 {
                said.push(format!("{} failed", p.failed));
            }
            if !said.is_empty() {
                out.push(format!("{}: {}", p.head.name, said.join(" · ")));
            }
        }
        out
    }

    /// The top from counts, a bullet a line; `nothing happened` when there
    /// is nothing to count.
    pub fn counted(&self) -> Vec<String> {
        let b = self.bullets();
        if b.is_empty() {
            return vec!["• nothing happened".into()];
        }
        b.iter().map(|b| format!("• {b}")).collect()
    }

    /// Whether a run or a job is in it: questions alone give an assistant
    /// nothing to tell, and the counts say them.
    pub fn has_runs(&self) -> bool {
        self.projects
            .iter()
            .flat_map(|p| &p.rows)
            .any(|r| matches!(r, Row::Run(_) | Row::Job(_)))
    }

    /// What the assistant is handed for the top: (project name, its rows'
    /// lines). A project with nothing new hands nothing.
    pub fn lines(&self) -> Vec<(String, String)> {
        self.projects
            .iter()
            .filter(|p| !p.rows.is_empty())
            .map(|p| {
                let lines: Vec<String> = p.rows.iter().map(|r| row_line(r, self.now)).collect();
                (p.head.name.clone(), lines.join("\n"))
            })
            .collect()
    }
}

impl Project<'_> {
    /// How its stretch went, in a few words: what needs you, what ran, what
    /// failed; `nothing new` when it is folded.
    pub fn tally(&self) -> String {
        if self.rows.is_empty() {
            return "nothing new".into();
        }
        let mut parts = Vec::new();
        if self.need > 0 {
            parts.push(format!(
                "{} need{} you",
                self.need,
                if self.need == 1 { "s" } else { "" }
            ));
        }
        if self.ran > 0 {
            parts.push(format!("{} ran", self.ran));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if parts.is_empty() {
            parts.push(format!("{} new", self.rows.len()));
        }
        parts.join(" · ")
    }
}

/// The catch-up's first words: since when, and how long that is.
pub fn headline(since: Timestamp, now: Timestamp) -> String {
    format!(
        "while you were away · since {} ({})",
        fmt_when(since, now),
        fmt_duration(now.duration_since(since))
    )
}

/// Whether a summary of everything can stand as the top: one that only says
/// nothing ran cannot — the counts say so, and what waits on you too.
pub fn top_worthy(s: &Summary) -> bool {
    s.source != "nothing new"
}

/// The top as the assistant wrote it, a line each, and last who wrote it and
/// from what (`# by haiku · from 23 runs`).
pub fn written(s: &Summary) -> Vec<String> {
    s.text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim_end().to_string())
        .chain(std::iter::once(format!("# {}", top_label(s))))
        .collect()
}

/// Who wrote the top and from what: `by haiku · from 23 runs`, `deep` first
/// when it read the logs, `moved on since` when more has happened.
pub fn top_label(s: &Summary) -> String {
    let mut parts = Vec::new();
    if s.deep {
        parts.push("deep".to_string());
    }
    parts.push(format!("by {}", s.agent));
    parts.push(format!("from {} run{}", s.runs, if s.runs == 1 { "" } else { "s" }));
    if s.stale {
        parts.push("moved on since".into());
    }
    parts.join(" · ")
}

/// The catch-up as `rl since` prints it: the headline, the `top` (a line
/// each, as given), then each project's line and under it its rows — a run
/// by its id, a question by its id and the keys that answer it.
pub fn text(cu: &CatchUp, top: &[String], now: Timestamp) -> String {
    let mut out = headline(cu.since, now);
    out.push_str("\n\n");
    for l in top {
        out.push_str(&format!("  {l}\n"));
    }
    for p in &cu.projects {
        let lead = if p.rows.is_empty() { '▸' } else { '▾' };
        out.push_str(&format!("\n{lead} {} · {}", p.head.name, p.tally()));
        for r in &p.rows {
            let line = row_line(r, now);
            out.push_str(&match r {
                Row::Question(q) => format!("\n    {} {}  {line}  [{}]", rows::QUESTION, q.id, q.keys()),
                Row::Run(i) => format!("\n    {} {}  {line}", rows::icon(&i.run), i.run.id),
                Row::Job(_) => format!("\n    {} {line}", rows::JOB),
                Row::Group(_) => continue,
            });
        }
    }
    out.push('\n');
    out
}

/// The header a question or a run goes under where the list gives it none:
/// its run's project — `everywhere`, a ref, or its folder — else the
/// question's folder.
fn head_of(d: &Digest, cx: &Context<'_>, run: Option<&Run>, ws: &Path) -> Head {
    match run {
        Some(r) if rows::everywhere_run(r, cx.names, cx.rules) => Head::everywhere(),
        Some(r) => rows::head(&r.place_key(), &r.workspace, d, cx),
        None => rows::head(&ws.to_string_lossy(), ws, d, cx),
    }
}

/// The run a row is, or a job's last firing.
fn run_of<'r>(r: &'r Row) -> Option<&'r Run> {
    match r {
        Row::Run(i) => Some(&i.run),
        Row::Job(j) => Some(&j.last().run),
        Row::Group(_) | Row::Question(_) => None,
    }
}

/// Where a row stands in its project: a question, then what else needs you,
/// then the rest.
fn tier(r: &Row) -> u8 {
    match r {
        Row::Question(_) => 0,
        r if rows::floats(r) => 1,
        _ => 2,
    }
}

/// When a row last changed.
fn at(r: &Row) -> Option<Timestamp> {
    match r {
        Row::Group(_) => None,
        Row::Question(q) => Some(q.updated_at),
        Row::Run(i) => Some(i.at),
        Row::Job(j) => Some(j.last().at),
    }
}

/// One line for a row: an agent's recap or state, a job's marks and last
/// outcome, a shell's last line, a question's ask.
pub fn row_line(r: &Row, now: Timestamp) -> String {
    let ago = |t: Timestamp| format!("{} ago", fmt_age(t, now));
    let parts: Vec<String> = match r {
        Row::Group(h) => return h.name.clone(),
        Row::Question(q) => vec![
            format!("asks: {}", q.title),
            q.summary.clone().unwrap_or_default(),
            ago(q.updated_at),
        ],
        Row::Run(i) => vec![
            i.run.title.clone(),
            rows::run_words(&i.run),
            rows::said(i).to_string(),
            ago(i.at),
        ],
        Row::Job(j) => {
            let last = &j.last().run;
            vec![
                format!("{} {}", j.marks(MARKS), j.name),
                rows::state_words(last.state, last.confidence),
                j.summary(),
                ago(j.last().at),
            ]
        }
    };
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::rows::{Lens, Names};
    use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
    use rl_proto::methods::DigestItem;
    use rl_proto::types::{Cause, Confidence, Run, RunKind, RunState, Summary};
    use std::path::{Path, PathBuf};

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

    fn fired(id: &str, job: &str, state: RunState, at: &str) -> DigestItem {
        let mut i = item(id, state, API, at, true);
        i.run.cause = Cause::Schedule(job.into());
        i.summary = if state == RunState::Failed {
            "exit 1 · 0s".into()
        } else {
            "exit 0 · 0s".into()
        };
        i
    }

    fn question(id: &str, ws: &str, at: &str) -> Decision {
        let t: Timestamp = at.parse().unwrap();
        Decision {
            id: id.into(),
            workspace: ws.into(),
            run: None,
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
            created_at: t,
            updated_at: t,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        }
    }

    const API: &str = "/home/f/api";
    const RELAY: &str = "/home/f/relay";

    fn now() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    /// `api`: a question, a failed run, a nightly job that went ok, ok,
    /// failed, and a run you saw before you left; `relay`: only a run you saw
    /// before you left.
    fn fixture() -> Digest {
        Digest {
            since: "2026-10-06T00:00:00Z".parse().unwrap(),
            items: vec![
                item("old", RunState::Finished, API, "2026-10-05T20:00:00Z", false),
                item("a1", RunState::Failed, API, "2026-10-06T09:00:00Z", true),
                fired("n1", "nightly", RunState::Finished, "2026-10-06T02:00:00Z"),
                fired("n2", "nightly", RunState::Finished, "2026-10-06T03:00:00Z"),
                fired("n3", "nightly", RunState::Failed, "2026-10-06T04:00:00Z"),
                item("r1", RunState::Finished, RELAY, "2026-10-05T21:00:00Z", false),
            ],
            decisions: vec![question("q1", API, "2026-10-06T08:00:00Z")],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
        }
    }

    fn names() -> Names {
        Names::new([
            (PathBuf::from(API), "api".to_string()),
            (PathBuf::from(RELAY), "relay".to_string()),
        ])
    }

    fn build<'a>(d: &'a Digest, names: &Names) -> CatchUp<'a> {
        CatchUp::build(
            d,
            &Context {
                names,
                rules: &[],
                filter: "",
                lens: Lens::All,
                now: now(),
            },
        )
    }

    fn keys(p: &Project) -> Vec<String> {
        p.rows.iter().filter_map(Row::key).collect()
    }

    #[test]
    fn what_needs_you_comes_first_and_a_quiet_project_folds() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        assert_eq!(cu.since, d.since);
        assert_eq!(cu.projects[0].head.name, "api");
        assert!(matches!(cu.projects[0].rows[0], Row::Question(_)));
        assert_eq!(
            keys(&cu.projects[0]),
            ["q:q1", "r:a1", "j:schedule:nightly@/home/f/api"],
            "the question, then the run and the job that failed, newest first; what you saw before is not news"
        );
        assert_eq!(cu.projects[0].need, 3, "the question, the failed run, the failed job");
        assert_eq!(cu.projects[0].ran, 4, "the run and three firings");
        assert_eq!(cu.projects[0].failed, 2, "the run and one firing");
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert!(relay.rows.is_empty(), "nothing since: folded");
        assert_eq!((relay.need, relay.ran, relay.failed), (0, 0, 0));
    }

    #[test]
    fn projects_that_need_you_come_first() {
        let mut d = fixture();
        // A failed run in relay, newer than anything in api: one need is fewer than three.
        d.items
            .push(item("r2", RunState::Failed, RELAY, "2026-10-06T11:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let order: Vec<&str> = cu.projects.iter().map(|p| p.head.name.as_str()).collect();
        assert_eq!(order, ["api", "relay"]);
        assert_eq!(cu.projects[1].need, 1);
    }

    #[test]
    fn what_the_list_puts_above_every_header_goes_under_its_project() {
        let mut d = fixture();
        // A pinned run stands above the projects on the list; a question from
        // a folder with no header has none to go under.
        let mut pinned = item("p1", RunState::Finished, RELAY, "2026-10-06T10:00:00Z", true);
        pinned.run.pinned_at = Some("2026-10-01T00:00:00Z".parse().unwrap());
        d.items.push(pinned);
        d.decisions
            .push(question("q2", "/home/f/scratch", "2026-10-06T07:00:00Z"));
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(keys(relay), ["r:p1"]);
        assert_eq!(relay.ran, 1);
        let scratch = cu.projects.iter().find(|p| p.head.name == "scratch").unwrap();
        assert_eq!(keys(scratch), ["q:q2"]);
        assert_eq!(scratch.need, 1);
        assert_eq!(scratch.head.workspace, PathBuf::from("/home/f/scratch"));
    }

    #[test]
    fn what_is_unseen_from_before_the_stretch_shows_but_is_not_counted() {
        let mut d = fixture();
        // Before `since`, more than a day before now, and never looked at.
        let mut old = fired("n0", "nightly", RunState::Failed, "2026-10-05T10:00:00Z");
        old.unseen = true;
        d.items.push(old);
        d.items
            .push(item("a0", RunState::Failed, API, "2026-10-05T09:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let api = &cu.projects[0];
        assert!(keys(api).contains(&"r:a0".to_string()), "still shown: {:?}", keys(api));
        assert_eq!((api.ran, api.failed), (4, 2), "only the stretch is counted");
        assert_eq!(api.need, 4, "but it still needs you");
        let job = api.rows.iter().find(|r| matches!(r, Row::Job(_))).unwrap();
        assert!(
            row_line(job, now()).starts_with("··× nightly"),
            "{}",
            row_line(job, now())
        );
    }

    #[test]
    fn a_long_stretch_keeps_what_ended_more_than_a_day_ago() {
        let mut d = fixture();
        d.since = "2026-10-03T12:00:00Z".parse().unwrap();
        // A stopped firing is never news; a run you looked at elsewhere is
        // not news either. Both are in the stretch, and more than a day old.
        let mut stopped = fired("h1", "hourly", RunState::Stopped, "2026-10-04T10:00:00Z");
        stopped.unseen = false;
        d.items.push(stopped);
        d.items
            .push(item("seen", RunState::Finished, API, "2026-10-04T12:00:00Z", false));
        let names = names();
        let cu = build(&d, &names);
        let api = &cu.projects[0];
        let k = keys(api);
        assert!(k.contains(&"r:seen".to_string()), "{k:?}");
        assert!(k.contains(&"j:schedule:hourly@/home/f/api".to_string()), "{k:?}");
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(
            keys(relay),
            ["r:r1"],
            "the run you saw before you left is now in the stretch"
        );
    }

    #[test]
    fn a_run_waiting_on_you_is_kept_however_old() {
        let mut d = fixture();
        d.items
            .push(item("w1", RunState::NeedsInput, RELAY, "2026-10-05T20:00:00Z", false));
        d.items
            .push(item("r3", RunState::Finished, RELAY, "2026-10-06T10:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(keys(relay), ["r:w1", "r:r3"]);
        assert_eq!((relay.need, relay.ran), (1, 1));
    }

    #[test]
    fn only_a_question_is_nothing_for_an_assistant_to_tell() {
        let mut d = fixture();
        d.items.clear();
        let names = names();
        let cu = build(&d, &names);
        assert!(!cu.has_runs(), "a question alone");
        assert_eq!(cu.counted(), ["• 1 needs you: api"]);
        assert!(build(&fixture(), &names).has_runs());
    }

    #[test]
    fn an_assistants_own_run_is_not_news() {
        let mut d = fixture();
        let mut top = item("s1", RunState::Working, RELAY, "2026-10-06T11:00:00Z", true);
        top.run.cause = Cause::Assistant("summary".into());
        d.items.push(top);
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert!(relay.rows.is_empty(), "writing the top is not what happened");
        assert!(!cu.bullets().iter().any(|b| b.starts_with("relay")));
    }

    #[test]
    fn a_question_goes_under_its_own_folder_not_one_of_the_same_name() {
        let (one, two) = ("/home/f/app", "/home/g/app");
        let d = Digest {
            since: "2026-10-06T00:00:00Z".parse().unwrap(),
            items: vec![],
            decisions: vec![question("q1", two, "2026-10-06T08:00:00Z")],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
        };
        let names = Names::new([
            (PathBuf::from(one), "app".to_string()),
            (PathBuf::from(two), "app".to_string()),
        ]);
        let cu = build(&d, &names);
        let under = |ws: &str| {
            let p = cu.projects.iter().find(|p| p.head.workspace == Path::new(ws)).unwrap();
            keys(p)
        };
        assert_eq!(under(two), ["q:q1"]);
        assert!(under(one).is_empty());
        assert_eq!(cu.projects.len(), 2, "no third header for it");
    }

    #[test]
    fn bullets_without_an_assistant_say_counts() {
        let (d, names) = (fixture(), names());
        let b = build(&d, &names).bullets();
        assert_eq!(b[0], "3 need you: api");
        assert!(b.iter().any(|l| l == "api: 4 ran · 2 failed"), "{b:?}");
        assert!(
            !b.iter().any(|l| l.starts_with("relay")),
            "nothing to say is not said: {b:?}"
        );
    }

    #[test]
    fn a_job_line_is_its_marks_and_last_outcome() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        let job = cu.projects[0].rows.iter().find(|r| matches!(r, Row::Job(_))).unwrap();
        let line = row_line(job, now());
        assert!(line.starts_with("··×"), "{line}");
        assert!(line.contains("nightly") && line.contains("failed"), "{line}");
    }

    #[test]
    fn lines_are_what_the_assistant_is_handed_per_project() {
        let (d, names) = (fixture(), names());
        let lines = build(&d, &names).lines();
        assert_eq!(lines.len(), 1, "a project with nothing new hands nothing: {lines:?}");
        let (name, text) = &lines[0];
        assert_eq!(name, "api");
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.lines().next().unwrap().contains("question q1"), "{text}");
    }

    #[test]
    fn a_runs_line_says_its_recap_when_it_waits() {
        let mut i = item("a2", RunState::NeedsInput, API, "2026-10-06T10:00:00Z", true);
        i.recap = Some("Search fixed; nothing committed.".into());
        let line = row_line(&Row::Run(&i), now());
        assert!(line.contains("needs you"), "{line}");
        assert!(line.contains("Search fixed; nothing committed."), "{line}");
        assert!(line.ends_with("2h ago"), "{line}");
    }

    #[test]
    fn the_text_is_the_headline_the_top_then_each_project() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        let t = text(&cu, &["• it went fine".to_string()], now());
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("while you were away · since "), "{t}");
        assert!(lines[0].ends_with("(12h00m)"), "{t}");
        assert_eq!(lines[2], "  • it went fine", "{t}");
        assert!(t.contains("\n▾ api · 3 need you · 4 ran · 2 failed\n"), "{t}");
        assert!(t.contains("\n    ? q1  asks: question q1 · 4h ago  [ok]\n"), "{t}");
        assert!(t.contains("    ✦ a1  title a1 · failed"), "a run by its id: {t}");
        assert!(t.contains("    ↻ ··× nightly · failed"), "{t}");
        assert!(t.ends_with("\n▸ relay · nothing new\n"), "{t}");
    }

    #[test]
    fn the_top_says_who_wrote_it_and_from_how_many_runs() {
        let mut s = Summary {
            run: String::new(),
            agent: "haiku".into(),
            by: "x1".into(),
            at: now(),
            text: "• fine".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: "all".into(),
            deep: false,
            from: Some(now()),
            calls: 1,
            runs: 23,
        };
        assert_eq!(top_label(&s), "by haiku · from 23 runs");
        s.runs = 1;
        s.deep = true;
        s.stale = true;
        assert_eq!(top_label(&s), "deep · by haiku · from 1 run · moved on since");
    }
}
