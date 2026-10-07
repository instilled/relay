//! The form at the foot of the panel: a run to start, one field per line.
//! `enter` saves, `tab` is the next field, `esc` is nothing. Pure: what is
//! typed, and what it means.

use super::rows::short_path;
use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use rl_proto::action::Task;
use rl_proto::types::RunKind;
use std::path::{Path, PathBuf};

/// CLIs that are agents, for the moment before the daemon has said which it
/// knows. The registry is the real answer (`agent.list`), and an agent of your
/// own in `detect/*.yaml` only ever shows up there.
const AGENTS: &[&str] = &["claude", "codex", "gemini", "aider", "cursor-agent", "copilot"];

pub struct Field {
    pub key: &'static str,
    pub hint: &'static str,
    pub value: String,
    /// The cursor, in chars.
    pub at: usize,
}

impl Field {
    pub fn new(key: &'static str, hint: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        let at = value.chars().count();
        Self { key, hint, value, at }
    }
}

pub struct Form {
    pub fields: Vec<Field>,
    pub cur: usize,
    /// The folder the form was opened from.
    pub origin: PathBuf,
    /// Folders `in` cycles through with ↑/↓: the known ones, and where
    /// the form was opened. Any other path can still be typed.
    pub folders: Vec<String>,
    /// The agents the registry knows, for reading the run line.
    pub agents: Vec<String>,
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
            fields: vec![
                Field::new("in", "a folder", short_path(folder)),
                Field::new("run", "make test · claude fix the build", ""),
                Field::new("title", "what to call it", ""),
            ],
            cur: 1,
            origin: folder.to_path_buf(),
            folders: vec![],
            agents: vec![],
        }
    }

    /// The folders `in` cycles through; the one it holds now comes first.
    pub fn with_folders(mut self, known: &[PathBuf]) -> Self {
        let mut v: Vec<String> = vec![self.get("in").to_string()];
        v.push(short_path(&self.origin));
        v.extend(known.iter().map(|p| short_path(p)));
        let mut seen = std::collections::HashSet::new();
        v.retain(|f| !f.is_empty() && seen.insert(f.clone()));
        self.folders = v;
        self
    }

    /// Sets a field, cursor at the end; a key the form lacks is ignored.
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(f) = self.fields.iter_mut().find(|f| f.key == key) {
            *f = Field::new(f.key, f.hint, value.to_string());
        }
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

    pub fn title(&self) -> &'static str {
        "new run"
    }

    /// The agents the registry knows, so a line naming one reads as an agent.
    pub fn with_agents(mut self, agents: &[String]) -> Self {
        self.agents = agents.to_vec();
        self
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
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // By word, as on the line: alt is the Mac habit, ctrl the other one.
        let by_word = alt || ctrl;
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
            KeyCode::Left if by_word => f.at = word_left(&f.value, f.at),
            KeyCode::Right if by_word => f.at = word_right(&f.value, f.at),
            KeyCode::Char('b') if alt => f.at = word_left(&f.value, f.at),
            KeyCode::Char('f') if alt => f.at = word_right(&f.value, f.at),
            KeyCode::Left => f.at = f.at.saturating_sub(1),
            KeyCode::Right => f.at = (f.at + 1).min(f.value.chars().count()),
            KeyCode::Home => f.at = 0,
            KeyCode::End => f.at = f.value.chars().count(),
            KeyCode::Backspace if by_word => {
                let to = word_left(&f.value, f.at);
                f.value.drain(byte_at(&f.value, to)..byte_at(&f.value, f.at));
                f.at = to;
            }
            KeyCode::Delete if by_word => {
                let to = word_right(&f.value, f.at);
                f.value.drain(byte_at(&f.value, f.at)..byte_at(&f.value, to));
            }
            KeyCode::Char('d') if alt => {
                let to = word_right(&f.value, f.at);
                f.value.drain(byte_at(&f.value, f.at)..byte_at(&f.value, to));
            }
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
            KeyCode::Char('k') if ctrl => {
                f.value.truncate(byte_at(&f.value, f.at));
            }
            KeyCode::Char('w') if ctrl => {
                let head = f.value[..byte_at(&f.value, f.at)].trim_end_matches(' ');
                let to = head.rfind(' ').map(|i| head[..i + 1].chars().count()).unwrap_or(0);
                f.value.drain(byte_at(&f.value, to)..byte_at(&f.value, f.at));
                f.at = to;
            }
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
                spans.extend(super::theme::with_caret(
                    &f.value,
                    byte_at(&f.value, f.at),
                    Style::default(),
                ));
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

/// The start of the word before `at` (in characters): over anything between
/// words, then the word itself — letters and digits, as on the line.
fn word_left(s: &str, at: usize) -> usize {
    let head: String = s.chars().take(at).collect();
    let head = head.trim_end_matches(|c: char| !c.is_alphanumeric());
    head.trim_end_matches(char::is_alphanumeric).chars().count()
}

/// The end of the word after `at`, the same way round.
fn word_right(s: &str, at: usize) -> usize {
    let tail: String = s.chars().skip(at).collect();
    let tail = tail.trim_start_matches(|c: char| !c.is_alphanumeric());
    let tail = tail.trim_start_matches(char::is_alphanumeric);
    s.chars().count() - tail.chars().count()
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
/// anything else is a shell line. `agents` is what the registry knows; empty,
/// before the daemon has answered, the built-in names stand in.
pub fn task_of(line: &str, agents: &[String]) -> Task {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let known = if agents.is_empty() {
        AGENTS.contains(&head)
    } else {
        agents.iter().any(|a| a == head)
    };
    if known {
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
    let (kind, argv) = match task_of(line, &f.agents) {
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
    fn a_field_is_walked_and_cut_by_word_too() {
        let dir = tempfile::tempdir().unwrap();
        let key = |code, m| KeyEvent::new(code, m);
        let alt = KeyModifiers::ALT;
        let ctrl = KeyModifiers::CONTROL;
        let mut f = Form::run(dir.path());
        press(&mut f, "make ~/Projects/flint");
        let field = |f: &Form| f.fields[f.cur].value.clone();
        let at = |f: &Form| f.fields[f.cur].at;

        f.key(&key(KeyCode::Left, alt));
        assert_eq!(at(&f), 16, "before `flint`");
        f.key(&key(KeyCode::Char('b'), alt));
        assert_eq!(at(&f), 7, "before `Projects`");
        f.key(&key(KeyCode::Right, ctrl));
        assert_eq!(at(&f), 15, "past `Projects`");

        f.key(&key(KeyCode::Char('e'), ctrl));
        f.key(&key(KeyCode::Backspace, alt));
        assert_eq!(field(&f), "make ~/Projects/");
        f.key(&key(KeyCode::Char('a'), ctrl));
        f.key(&key(KeyCode::Delete, alt));
        assert_eq!(field(&f), " ~/Projects/");
        f.key(&key(KeyCode::Char('d'), alt));
        assert_eq!(field(&f), "/");
        // `^k` cuts to the end, `^w` the word behind, as on the line.
        press(&mut f, "one two");
        f.key(&key(KeyCode::Char('w'), ctrl));
        assert_eq!(field(&f), "one /", "the text was typed before the slash left over");
        f.key(&key(KeyCode::Char('a'), ctrl));
        f.key(&key(KeyCode::Char('k'), ctrl));
        assert_eq!(field(&f), "");
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
}
