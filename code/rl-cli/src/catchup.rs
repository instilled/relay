//! The catch-up: what happened while you were away, as the list's own rows.
//!
//! The list's rows for the stretch, under the list's own headers: a question,
//! a run with news since you left, a job's firings in that time. What needs
//! you leads each project, and the projects that need you lead; a project
//! with nothing new keeps its header and folds to one line. Counted, they are
//! the top without an assistant; in a line each, what an assistant is handed
//! to write it.

use crate::digest::since_items;
use crate::tui::rows::{self, Context, Head, Lens, Row};
use jiff::Timestamp;
use rl_core::time::fmt_age;
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::types::{Run, RunState};
use std::collections::HashSet;
use std::path::Path;

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
    /// Questions, runs that need you, and jobs whose last firing failed.
    pub need: usize,
    /// Runs and job firings in the stretch.
    pub ran: usize,
    /// Of those, the ones that failed.
    pub failed: usize,
    pub rows: Vec<Row<'a>>,
}

impl<'a> Project<'a> {
    fn new(head: Head) -> Self {
        Self {
            head,
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
        // The whole list, whatever it is narrowed to now.
        let cx = Context {
            names: cx.names,
            rules: cx.rules,
            filter: "",
            lens: Lens::All,
            now: cx.now,
        };
        let news: HashSet<&str> = since_items(d).into_iter().map(|i| i.run.id.as_str()).collect();
        let fresh = |i: &DigestItem| news.contains(i.run.id.as_str());
        let mut projects: Vec<Project<'a>> = Vec::new();
        // Questions, and runs pinned or come through, stand above every
        // header on the list; here they go under their project's.
        let mut loose: Vec<Row<'a>> = Vec::new();
        for row in rows::build(d, &cx) {
            let row = match row {
                Row::Group(h) => {
                    projects.push(Project::new(h));
                    continue;
                }
                Row::Question(q) => Row::Question(q),
                Row::Run(i) if fresh(i) => Row::Run(i),
                Row::Job(mut j) => {
                    // Only its firings in the stretch; a job with none is not news.
                    j.runs.retain(|i| fresh(i));
                    if j.runs.is_empty() {
                        continue;
                    }
                    Row::Job(j)
                }
                Row::Run(_) => continue,
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
            let head = head_of(d, &cx, run, ws);
            let p = match projects
                .iter()
                .position(|p| p.head.name == head.name && p.head.is_everywhere() == head.is_everywhere())
            {
                Some(p) => p,
                None => {
                    projects.push(Project::new(head));
                    projects.len() - 1
                }
            };
            projects[p].rows.push(row);
        }
        for p in &mut projects {
            p.rows.sort_by_key(|r| (tier(r), std::cmp::Reverse(at(r))));
            for r in &p.rows {
                match r {
                    Row::Question(_) => p.need += 1,
                    Row::Run(i) => {
                        p.ran += 1;
                        p.need += usize::from(i.run.state.needs_attention());
                        p.failed += usize::from(i.run.state == RunState::Failed);
                    }
                    Row::Job(j) => {
                        p.ran += j.runs.len();
                        p.need += usize::from(j.last().run.state == RunState::Failed);
                        p.failed += j.failed();
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
    use rl_proto::types::{Cause, Confidence, Run, RunKind, RunState};
    use std::path::PathBuf;

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
}
