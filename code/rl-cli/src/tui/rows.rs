//! The one list: questions and runs, ranked by attention, grouped by what
//! they belong to, and named in words rather than ids and paths.
//!
//! A group is a run's group key (ref → branch → workspace). Groups come in
//! order of their most urgent member; inside a group, questions come first,
//! then runs by attention, newest change first. A workspace path reads as the
//! name it was registered under.

use crate::rank;
use jiff::{SignedDuration, Timestamp};
use relay_proto::decision::Decision;
use relay_proto::methods::{Digest, DigestItem};
use relay_proto::types::{Cause, Run};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Ended runs stay on the list this long.
pub const RECENT: SignedDuration = SignedDuration::from_hours(24);

pub enum Row<'a> {
    Group(String),
    Question(&'a Decision),
    Run(&'a DigestItem),
}

impl Row<'_> {
    /// Selectable rows carry a stable key, so the cursor survives a re-rank.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Group(_) => None,
            Row::Question(d) => Some(format!("q:{}", d.id)),
            Row::Run(i) => Some(format!("r:{}", i.run.id)),
        }
    }
}

/// Workspace paths → the names they were registered under.
#[derive(Default, Clone)]
pub struct Names(HashMap<PathBuf, String>);

impl Names {
    pub fn new(ws: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        Self(ws.into_iter().collect())
    }

    /// The name a folder was registered under, if it was.
    pub fn registered(&self, p: &Path) -> Option<String> {
        self.0.get(p).cloned()
    }

    /// A workspace by its registered name, else its last path component.
    pub fn workspace(&self, p: &Path) -> String {
        self.0.get(p).cloned().unwrap_or_else(|| {
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
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

/// Why a run started, in words.
pub fn cause_words(c: &Cause) -> String {
    match c {
        Cause::Manual => "started by hand".into(),
        Cause::Schedule(n) => format!("schedule {n}"),
        Cause::Decision(_) => "your answer".into(),
        Cause::Reaction(n) => format!("rule {n}"),
        Cause::Workflow { name, step } => format!("workflow {name} · {step}"),
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
    questions: Vec<&'a Decision>,
    runs: Vec<&'a DigestItem>,
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
            .min()
            .unwrap_or(i16::MAX)
    }
    fn latest(&self) -> Timestamp {
        self.runs
            .iter()
            .map(|i| i.at)
            .chain(self.questions.iter().map(|q| q.created_at))
            .max()
            .unwrap_or(Timestamp::UNIX_EPOCH)
    }
}

/// Everything on the list, in order. `filter` is matched case-insensitively
/// against titles, group names, CLIs and summaries.
pub fn build<'a>(d: &'a Digest, names: &Names, filter: &str, now: Timestamp) -> Vec<Row<'a>> {
    let f = filter.to_lowercase();
    let hit = |parts: &[&str]| f.is_empty() || parts.iter().any(|s| s.to_lowercase().contains(&f));
    let mut groups: Vec<Group<'a>> = Vec::new();
    let group_of = |key: String, groups: &mut Vec<Group<'a>>| -> usize {
        match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    questions: Vec::new(),
                    runs: Vec::new(),
                });
                groups.len() - 1
            }
        }
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
        let g = group_of(key, &mut groups);
        groups[g].questions.push(q);
    }
    for i in &d.items {
        let r = &i.run;
        let live = !r.state.is_terminal() || i.unseen || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT);
        if !live {
            continue;
        }
        let key = r.group_key();
        let name = names.group(&key);
        if !hit(&[&r.title, &name, r.cli.as_deref().unwrap_or(""), &i.summary]) {
            continue;
        }
        let g = group_of(key, &mut groups);
        groups[g].runs.push(i);
    }
    for g in &mut groups {
        g.questions.sort_by_key(|q| q.created_at);
        g.runs.sort_by(|a, b| {
            rank::score(&a.run)
                .cmp(&rank::score(&b.run))
                .then_with(|| b.at.cmp(&a.at))
        });
    }
    groups.sort_by(|a, b| a.score().cmp(&b.score()).then_with(|| b.latest().cmp(&a.latest())));
    let mut rows = Vec::new();
    for g in groups {
        rows.push(Row::Group(names.group(&g.key)));
        rows.extend(g.questions.into_iter().map(Row::Question));
        rows.extend(g.runs.into_iter().map(Row::Run));
    }
    rows
}

/// `(questions, need you, working, finished)` over what the list shows.
pub fn counts(rows: &[Row]) -> (usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0);
    for r in rows {
        match r {
            Row::Group(_) => {}
            Row::Question(_) => c.0 += 1,
            Row::Run(i) => {
                use relay_proto::types::RunState::*;
                match i.run.state {
                    NeedsInput | Blocked | Failed => c.1 += 1,
                    Starting | Working | Stale | Idle => c.2 += 1,
                    Finished | Stopped => c.3 += 1,
                }
            }
        }
    }
    c
}

/// The header line: what needs you first, then what is moving.
pub fn headline(rows: &[Row], news: usize, filter: &str) -> String {
    let (asked, need, working, fin) = counts(rows);
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
    use relay_proto::decision::{DecisionOption, DecisionStatus};
    use relay_proto::types::{Confidence, RunKind, RunState};

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
                Row::Group(g) => format!("[{g}]"),
                other => other.key().unwrap(),
            })
            .collect()
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
        let rows = build(&d, &names, "", now);
        assert_eq!(
            keys(&rows),
            ["[relay]", "q:q1", "r:fail1", "r:done1", "[api]", "r:blok1", "r:work1"],
            "the question puts relay first; api is named by its folder; old01 is gone"
        );
        assert_eq!(counts(&rows), (1, 2, 1, 1));
        assert_eq!(
            headline(&rows, 2, ""),
            "relay · 1 to decide · 2 need you · 1 working · 1 finished · 2 new"
        );

        let rows = build(&d, &names, "api", now);
        assert_eq!(keys(&rows), ["[api]", "r:blok1", "r:work1"]);
        assert_eq!(headline(&rows, 0, "api"), "relay · 1 needs you · 1 working · /api");
        assert_eq!(headline(&[], 0, ""), "relay · nothing running");
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
}
