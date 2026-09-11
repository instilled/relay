//! The form at the foot of the panel: a run to start, or a rule to write
//! down, one field per line. `enter` saves, `tab` is the next field, `esc`
//! is nothing. Pure: what is typed, and what it means.

use super::rows::short_path;
pub use crate::rulefile::RuleDraft;
use crate::rulefile::Was;
use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use relay_core::time::parse_duration;
use relay_proto::action::Task;
use relay_proto::reaction::ReactionSpec;
use relay_proto::rule::{Rule, RuleKind};
use relay_proto::schedule::{GLOBAL_SCOPE, ScheduleSpec};
use relay_proto::types::RunKind;
use std::path::{Path, PathBuf};

/// CLIs that are agents: a line starting with one is a prompt, not a shell line.
const AGENTS: &[&str] = &["claude", "codex", "gemini", "aider", "cursor-agent", "copilot"];

/// The word for your own rules in the `in` field.
pub const YOURS: &str = "yours";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Run,
    Rule,
}

pub struct Field {
    pub key: &'static str,
    pub hint: &'static str,
    pub value: String,
    /// The cursor, in chars.
    pub at: usize,
}

impl Field {
    fn new(key: &'static str, hint: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        let at = value.chars().count();
        Self { key, hint, value, at }
    }
}

pub struct Form {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub cur: usize,
    /// The folder the form was opened from: where a rule of yours runs
    /// unless `cwd` says otherwise.
    pub origin: PathBuf,
    /// Folders `in` cycles through with ↑/↓: the known ones, and where
    /// the form was opened. Any other path can still be typed.
    pub folders: Vec<String>,
    /// The rule being edited — kind, scope, name — so a rename or a move
    /// takes the old block out, and what the form does not show survives.
    pub editing: Option<Editing>,
}

pub struct Editing {
    pub rule: Rule,
}

impl Editing {
    pub fn was(&self) -> Was {
        Was {
            kind: self.rule.kind(),
            scope: self.rule.scope().to_string(),
            name: self.rule.name().to_string(),
        }
    }
}

/// What a keystroke did.
#[derive(PartialEq, Eq, Debug)]
pub enum Act {
    Typing,
    Save,
    Cancel,
}

impl Form {
    /// A run in `folder`.
    pub fn run(folder: &Path) -> Self {
        Self {
            kind: Kind::Run,
            fields: vec![
                Field::new("in", "a folder", short_path(folder)),
                Field::new("run", "make test · claude fix the build", ""),
                Field::new("title", "what to call it", ""),
            ],
            cur: 1,
            origin: folder.to_path_buf(),
            folders: vec![],
            editing: None,
        }
    }

    /// The folders `in` cycles through; the one it holds now comes first.
    pub fn with_folders(mut self, known: &[PathBuf]) -> Self {
        let mut v: Vec<String> = vec![self.get("in").to_string()];
        if self.kind == Kind::Rule {
            v.push(YOURS.into());
        }
        v.push(short_path(&self.origin));
        v.extend(known.iter().map(|p| short_path(p)));
        v.dedup();
        let mut seen = std::collections::HashSet::new();
        v.retain(|f| !f.is_empty() && seen.insert(f.clone()));
        self.folders = v;
        self
    }

    /// A rule in `folder`, or yours; `origin` is where a rule of yours runs
    /// unless the form says otherwise.
    pub fn rule(folder: Option<&Path>, origin: &Path) -> Self {
        let place = folder.map(short_path).unwrap_or_else(|| YOURS.into());
        Self {
            kind: Kind::Rule,
            fields: vec![
                Field::new("in", "a folder, or yours", place),
                Field::new("name", "nightly", ""),
                Field::new("when", "every 30m · 0 2 * * * · on run.failed", ""),
                Field::new("run", "make test · claude fix the build", ""),
                Field::new("title", "what to call its runs", ""),
                Field::new("tags", "a, b", ""),
                Field::new("cwd", "where it runs (the folder)", ""),
            ],
            cur: 1,
            origin: origin.to_path_buf(),
            folders: vec![],
            editing: None,
        }
    }

    /// The same form, filled in from a rule as it stands.
    pub fn edit(r: &Rule) -> Self {
        let folder = (!r.is_global()).then(|| PathBuf::from(r.scope()));
        let mut f = Self::rule(folder.as_deref(), Path::new(r.scope()));
        let (when, run, title, tags, cwd) = match r {
            Rule::Schedule(s) => (
                match (&s.spec.every, &s.spec.cron) {
                    (Some(e), _) => format!("every {e}"),
                    (None, Some(c)) => c.clone(),
                    _ => String::new(),
                },
                s.spec.run.as_ref(),
                s.spec.title.clone(),
                s.spec.tags.clone(),
                s.spec.cwd.clone(),
            ),
            Rule::Reaction(x) => (
                format!("on {}", x.spec.on.join(" ")),
                x.spec.run.as_ref(),
                x.spec.title.clone(),
                x.spec.tags.clone(),
                x.spec.cwd.clone(),
            ),
        };
        let run = run.map(task_text).unwrap_or_default();
        for field in &mut f.fields {
            let v = match field.key {
                "name" => r.name().to_string(),
                "when" => when.clone(),
                "run" => run.clone(),
                "title" => title.clone().unwrap_or_default(),
                "tags" => tags.join(", "),
                "cwd" => cwd.as_deref().map(short_path).unwrap_or_default(),
                _ => continue,
            };
            *field = Field::new(field.key, field.hint, v);
        }
        f.editing = Some(Editing { rule: r.clone() });
        f
    }

    pub fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.trim())
            .unwrap_or("")
    }

    /// Lines on screen: the header, then a field per line.
    pub fn height(&self) -> u16 {
        self.fields.len() as u16 + 1
    }

    pub fn title(&self) -> String {
        match (self.kind, &self.editing) {
            (Kind::Run, _) => "new run".into(),
            (Kind::Rule, Some(e)) => format!("edit {}", e.rule.name()),
            (Kind::Rule, None) => "new rule".into(),
        }
    }

    /// Whether the cursor is on `in` with folders to walk through.
    pub fn on_folders(&self) -> bool {
        self.fields[self.cur].key == "in" && self.folders.len() > 1
    }

    /// On `in`: the next (or previous) known folder.
    fn cycle_folder(&mut self, by: isize) {
        let n = self.folders.len() as isize;
        let cur = self.folders.iter().position(|x| x == self.get("in")).unwrap_or(0) as isize;
        let next = ((cur + by) % n + n) % n;
        let value = self.folders[next as usize].clone();
        let f = &mut self.fields[self.cur];
        *f = Field::new(f.key, f.hint, value);
    }

    pub fn key(&mut self, k: &KeyEvent) -> Act {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.fields.len();
        if self.on_folders() && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            self.cycle_folder(if k.code == KeyCode::Down { 1 } else { -1 });
            return Act::Typing;
        }
        let f = &mut self.fields[self.cur];
        match k.code {
            KeyCode::Esc => return Act::Cancel,
            KeyCode::Enter => return Act::Save,
            KeyCode::Tab | KeyCode::Down => self.cur = (self.cur + 1) % n,
            KeyCode::BackTab | KeyCode::Up => self.cur = (self.cur + n - 1) % n,
            KeyCode::Left => f.at = f.at.saturating_sub(1),
            KeyCode::Right => f.at = (f.at + 1).min(f.value.chars().count()),
            KeyCode::Home => f.at = 0,
            KeyCode::End => f.at = f.value.chars().count(),
            KeyCode::Backspace => {
                if f.at > 0 {
                    let i = byte_at(&f.value, f.at - 1);
                    f.value.remove(i);
                    f.at -= 1;
                }
            }
            KeyCode::Delete => {
                if f.at < f.value.chars().count() {
                    let i = byte_at(&f.value, f.at);
                    f.value.remove(i);
                }
            }
            KeyCode::Char('u') if ctrl => {
                f.value.clear();
                f.at = 0;
            }
            KeyCode::Char('a') if ctrl => f.at = 0,
            KeyCode::Char('e') if ctrl => f.at = f.value.chars().count(),
            KeyCode::Char(c) if !ctrl => {
                let i = byte_at(&f.value, f.at);
                f.value.insert(i, c);
                f.at += 1;
            }
            _ => {}
        }
        Act::Typing
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut out = vec![Line::from(Span::raw(format!(" {}", self.title())).bold())];
        for (i, f) in self.fields.iter().enumerate() {
            let label = Span::raw(format!(" {:<6} ", f.key));
            let mut spans = vec![if i == self.cur { label.bold() } else { label.dim() }];
            if i == self.cur {
                let at = byte_at(&f.value, f.at);
                spans.push(Span::raw(f.value[..at].to_string()));
                spans.push(Span::raw("▏"));
                spans.push(Span::raw(f.value[at..].to_string()));
            } else if f.value.is_empty() {
                spans.push(Span::raw(f.hint.to_string()).dim());
            } else {
                spans.push(Span::raw(f.value.clone()));
            }
            if i == self.cur && f.value.is_empty() {
                spans.push(Span::raw(format!("  {}", f.hint)).dim());
            }
            out.push(Line::from(spans));
        }
        out
    }
}

fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

/// `~` and a bare name become a path.
pub fn expand(s: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match s.strip_prefix('~') {
        Some(rest) if !home.is_empty() => PathBuf::from(format!("{home}{rest}")),
        _ => PathBuf::from(s),
    }
}

/// A line as a task: an agent's name first makes the rest its prompt;
/// anything else is a shell line.
pub fn task_of(line: &str) -> Task {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    if AGENTS.contains(&head) {
        let rest = rest.trim();
        let prompt = rest
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .or_else(|| rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
            .unwrap_or(rest);
        Task::Agent {
            agent: head.to_string(),
            prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
            prompt_file: None,
            args: vec![],
        }
    } else {
        Task::Shell(line.to_string())
    }
}

/// A task back as a line for the form.
pub fn task_text(t: &Task) -> String {
    match t {
        Task::Shell(s) => s.clone(),
        Task::Argv(v) => v.join(" "),
        Task::Agent { agent, prompt, .. } => match prompt {
            Some(p) => format!("{agent} {p}"),
            None => agent.clone(),
        },
    }
}

/// The run the form describes.
#[derive(Debug)]
pub struct RunDraft {
    pub cwd: PathBuf,
    pub kind: RunKind,
    pub argv: Vec<String>,
    pub title: Option<String>,
}

pub fn run_draft(f: &Form) -> Result<RunDraft> {
    let line = f.get("run");
    if line.is_empty() {
        bail!("run: what to run");
    }
    let cwd = expand(f.get("in"));
    if f.get("in").is_empty() || !cwd.is_dir() {
        bail!("in: not a folder");
    }
    let (kind, argv) = match task_of(line) {
        Task::Agent { agent, prompt, .. } => {
            let mut argv = vec![agent];
            argv.extend(prompt);
            (RunKind::Agent, argv)
        }
        Task::Shell(s) => (RunKind::Command, vec!["sh".into(), "-c".into(), s]),
        Task::Argv(v) => (RunKind::Command, v),
    };
    let title = f.get("title");
    Ok(RunDraft {
        cwd,
        kind,
        argv,
        title: (!title.is_empty()).then(|| title.to_string()),
    })
}

/// `every 30m`, `30m`, `0 2 * * *`, `cron …`, or `on <event>…`.
enum When {
    Every(String),
    Cron(String),
    On(Vec<String>),
}

fn when_of(s: &str) -> Result<When> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("on ") {
        let on: Vec<String> = rest.split_whitespace().map(String::from).collect();
        if on.is_empty() {
            bail!("when: on <event>, like on run.failed");
        }
        return Ok(When::On(on));
    }
    if let Some(rest) = s.strip_prefix("every ") {
        let d = rest.trim();
        if parse_duration(d).is_none() {
            bail!("when: every <duration>, like every 30m");
        }
        return Ok(When::Every(d.to_string()));
    }
    if let Some(rest) = s.strip_prefix("cron ") {
        return Ok(When::Cron(rest.trim().to_string()));
    }
    if parse_duration(s).is_some() {
        return Ok(When::Every(s.to_string()));
    }
    if s.split_whitespace().count() == 5 {
        return Ok(When::Cron(s.to_string()));
    }
    bail!("when: every 30m, a cron line, or on run.failed")
}

pub fn rule_draft(f: &Form) -> Result<RuleDraft> {
    let name = f.get("name").to_string();
    crate::rulefile::check_name(&name)?;
    let when = when_of(f.get("when"))?;
    // Editing keeps whatever the form does not show — env, where, ask…
    let was = f.editing.as_ref().map(|e| &e.rule);
    let run = f.get("run");
    let asks = matches!(was, Some(Rule::Reaction(r)) if r.spec.ask.is_some());
    if run.is_empty() && !asks {
        bail!("run: what it runs");
    }
    let place = f.get("in");
    let folder = if place.is_empty() || place == YOURS || place == GLOBAL_SCOPE {
        None
    } else {
        let p = expand(place);
        if !p.is_dir() {
            bail!("in: not a folder");
        }
        Some(std::fs::canonicalize(&p).unwrap_or(p))
    };
    let title = f.get("title");
    let title = (!title.is_empty()).then(|| title.to_string());
    let tags: Vec<String> = f
        .get("tags")
        .split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    // A rule of yours runs where the form was opened unless told otherwise.
    let cwd = f.get("cwd");
    let cwd = match (cwd.is_empty(), &folder) {
        (false, _) => Some(expand(cwd)),
        (true, None) => Some(f.origin.clone()),
        (true, Some(_)) => None,
    };
    let task = (!run.is_empty()).then(|| task_of(run));
    let (kind, body) = match when {
        When::On(on) => {
            let mut spec = match was {
                Some(Rule::Reaction(r)) => r.spec.clone(),
                _ => ReactionSpec::default(),
            };
            spec.on = on;
            spec.run = task;
            spec.title = title;
            spec.tags = tags;
            spec.cwd = cwd;
            (
                RuleKind::Reaction,
                crate::rulefile::body(&spec, &ReactionSpec::default())?,
            )
        }
        every_or_cron => {
            let mut spec = match was {
                Some(Rule::Schedule(s)) => s.spec.clone(),
                _ => ScheduleSpec::default(),
            };
            match every_or_cron {
                When::Every(e) => (spec.every, spec.cron) = (Some(e), None),
                When::Cron(c) => (spec.every, spec.cron) = (None, Some(c)),
                When::On(_) => unreachable!(),
            }
            spec.run = task;
            spec.title = title;
            spec.tags = tags;
            spec.cwd = cwd;
            (
                RuleKind::Schedule,
                crate::rulefile::body(&spec, &ScheduleSpec::default())?,
            )
        }
    };
    Ok(RuleDraft {
        kind,
        folder,
        name,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(f: &mut Form, keys: &str) -> Act {
        let mut act = Act::Typing;
        for c in keys.chars() {
            act = f.key(&KeyEvent::from(KeyCode::Char(c)));
        }
        act
    }

    #[test]
    fn a_run_from_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = Form::run(dir.path());
        assert_eq!(f.cur, 1, "the cursor starts on `run`");
        press(&mut f, "make test");
        assert_eq!(f.key(&KeyEvent::from(KeyCode::Tab)), Act::Typing);
        press(&mut f, "tests");
        assert_eq!(f.key(&KeyEvent::from(KeyCode::Enter)), Act::Save);
        let d = run_draft(&f).unwrap();
        assert_eq!(d.kind, RunKind::Command);
        assert_eq!(d.argv, ["sh", "-c", "make test"]);
        assert_eq!(d.title.as_deref(), Some("tests"));
        assert_eq!(d.cwd, dir.path());

        let mut f = Form::run(dir.path());
        press(&mut f, "claude \"fix the build\"");
        let d = run_draft(&f).unwrap();
        assert_eq!(d.kind, RunKind::Agent);
        assert_eq!(d.argv, ["claude", "fix the build"]);

        let f = Form::run(dir.path());
        assert!(run_draft(&f).unwrap_err().to_string().starts_with("run:"));
    }

    #[test]
    fn the_in_field_walks_the_known_folders() {
        let known = vec![PathBuf::from("/w/api"), PathBuf::from("/w/relay")];
        let mut f = Form::run(Path::new("/w/relay")).with_folders(&known);
        assert_eq!(
            f.folders,
            ["/w/relay", "/w/api"],
            "where it opened first, then the others, once"
        );
        f.key(&KeyEvent::from(KeyCode::BackTab));
        assert!(f.on_folders());
        f.key(&KeyEvent::from(KeyCode::Down));
        assert_eq!(f.get("in"), "/w/api");
        f.key(&KeyEvent::from(KeyCode::Down));
        assert_eq!(f.get("in"), "/w/relay", "round the end");
        f.key(&KeyEvent::from(KeyCode::Up));
        assert_eq!(f.get("in"), "/w/api");
        // A path of its own is typed like anything else.
        f.key(&KeyEvent::from(KeyCode::Char('/')));
        assert_eq!(f.get("in"), "/w/api/");
        f.key(&KeyEvent::from(KeyCode::Tab));
        assert!(!f.on_folders());
        // A rule's `in` offers yours too.
        let f = Form::rule(Some(Path::new("/w/relay")), Path::new("/w/relay")).with_folders(&known);
        assert_eq!(f.folders, ["/w/relay", "yours", "/w/api"]);
    }

    #[test]
    fn editing_a_field_keeps_the_cursor_honest() {
        let mut f = Form::run(Path::new("/w"));
        press(&mut f, "héllo");
        f.key(&KeyEvent::from(KeyCode::Left));
        f.key(&KeyEvent::from(KeyCode::Left));
        press(&mut f, "X");
        assert_eq!(f.get("run"), "hélXlo");
        f.key(&KeyEvent::from(KeyCode::Backspace));
        f.key(&KeyEvent::from(KeyCode::Backspace));
        assert_eq!(f.get("run"), "hélo");
        f.key(&KeyEvent::from(KeyCode::Home));
        f.key(&KeyEvent::from(KeyCode::Delete));
        assert_eq!(f.get("run"), "élo");
        assert_eq!(f.key(&KeyEvent::from(KeyCode::Esc)), Act::Cancel);
    }

    #[test]
    fn a_rule_from_its_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = Form::rule(Some(dir.path()), dir.path());
        press(&mut f, "nightly");
        f.key(&KeyEvent::from(KeyCode::Tab));
        press(&mut f, "every 4h");
        f.key(&KeyEvent::from(KeyCode::Tab));
        press(&mut f, "make test");
        f.key(&KeyEvent::from(KeyCode::Tab));
        f.key(&KeyEvent::from(KeyCode::Tab));
        press(&mut f, "ci, slow");
        let d = rule_draft(&f).unwrap();
        assert_eq!(d.kind, RuleKind::Schedule);
        assert_eq!(
            d.folder.as_deref(),
            Some(std::fs::canonicalize(dir.path()).unwrap().as_path())
        );
        assert_eq!(d.name, "nightly");
        assert_eq!(d.body, ["every: 4h", "run: make test", "tags:", "- ci", "- slow"]);

        // `on …` is a reaction; a cron line is a schedule.
        let mut f = Form::rule(None, Path::new("/w"));
        press(&mut f, "notify");
        f.key(&KeyEvent::from(KeyCode::Tab));
        press(&mut f, "on run.failed decision.raised");
        f.key(&KeyEvent::from(KeyCode::Tab));
        press(&mut f, "say no");
        let d = rule_draft(&f).unwrap();
        assert_eq!(
            (d.kind, d.folder.clone(), d.scope()),
            (RuleKind::Reaction, None, "global".to_string())
        );
        assert_eq!(
            d.body,
            ["on:", "- run.failed", "- decision.raised", "run: say no", "cwd: /w"],
            "a rule of yours runs where the form was opened"
        );
        for field in &mut f.fields {
            if field.key == "when" {
                field.value = "0 2 * * *".into();
            }
            if field.key == "cwd" {
                field.value = "/elsewhere".into();
            }
        }
        let d = rule_draft(&f).unwrap();
        assert_eq!(d.kind, RuleKind::Schedule);
        assert_eq!(d.body[0], "cron: 0 2 * * *");
        assert!(d.body.contains(&"cwd: /elsewhere".to_string()));
        for field in &mut f.fields {
            if field.key == "when" {
                field.value = "soon".into();
            }
        }
        assert!(rule_draft(&f).unwrap_err().to_string().starts_with("when:"));
    }

    #[test]
    fn editing_shows_the_rule_and_keeps_what_it_does_not_show() {
        use relay_proto::schedule::Schedule;
        let mut spec = ScheduleSpec {
            every: Some("4h".into()),
            run: Some(Task::Agent {
                agent: "claude".into(),
                prompt: Some("fix it".into()),
                prompt_file: None,
                args: vec![],
            }),
            title: Some("night".into()),
            ..Default::default()
        };
        spec.env.insert("CI".into(), "1".into());
        let rule = Rule::Schedule(Schedule {
            scope: "/w".into(),
            name: "nightly".into(),
            hash: "h".into(),
            source: "/w/.relay/schedules.yaml".into(),
            spec,
            paused: false,
            last_fired_at: None,
            last_run: None,
            next_at: None,
            removed_at: None,
            approved_hash: None,
        });
        let mut f = Form::edit(&rule);
        assert_eq!(f.title(), "edit nightly");
        assert_eq!(
            f.fields.iter().map(|x| x.value.as_str()).collect::<Vec<_>>(),
            ["/w", "nightly", "every 4h", "claude fix it", "night", "", ""]
        );
        // Only the cadence changes; the env the form never showed stays.
        for field in &mut f.fields {
            if field.key == "when" {
                field.value = "every 6h".into();
            }
            if field.key == "in" {
                field.value = YOURS.into();
            }
        }
        let d = rule_draft(&f).unwrap();
        assert_eq!(
            d.body,
            [
                "every: 6h",
                "run:",
                "  agent: claude",
                "  prompt: fix it",
                "cwd: /w",
                "env:",
                "  CI: '1'",
                "title: night"
            ]
        );
    }
}
