//! `n`: a new session, in the footer, one field per line — the harness
//! (cycled with `↑↓`/`←→` through the ones offered, then `shell`), the folder
//! it runs in (cycled with `↑↓` through the projects' folders, or typed), and
//! for an agent a first prompt, which may stay empty. `tab` and `shift-tab`
//! go between the fields, `enter` starts it, `esc` is nothing.
//!
//! Pure: what is picked and typed, and what it would start.

use super::line;
use super::theme;
use crate::rows::short_path;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};

/// The harness that is no agent: a shell, `$SHELL`.
pub const SHELL: &str = "shell";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Harness,
    Folder,
    Prompt,
}

pub struct Form {
    /// What it can start: never empty, `shell` last.
    pub harnesses: Vec<String>,
    pub harness: usize,
    /// The folders `↑↓` walks, the one it opened on first.
    pub folders: Vec<String>,
    pub folder: line::Line,
    pub prompt: line::Line,
    pub at: Field,
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Typing,
    Start,
    Cancel,
}

/// What the form starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The harness's profile name; `None` for a shell.
    pub harness: Option<String>,
    pub cwd: PathBuf,
    /// For an agent, its prompt, if one was typed.
    pub argv: Vec<String>,
}

fn line_of(text: String) -> line::Line {
    let mut l = line::Line::default();
    let at = text.len();
    l.set_with_cursor(text, at);
    l
}

impl Form {
    /// A form offering `harnesses` (then a shell), in `here`, with `known`
    /// folders to walk.
    pub fn new(harnesses: &[String], here: &Path, known: &[PathBuf]) -> Form {
        let mut offered: Vec<String> = harnesses.iter().filter(|h| *h != SHELL).cloned().collect();
        offered.dedup();
        offered.push(SHELL.into());
        let mut folders = vec![short_path(here)];
        for k in known {
            let s = short_path(k);
            if !folders.contains(&s) {
                folders.push(s);
            }
        }
        Form {
            harnesses: offered,
            harness: 0,
            folder: line_of(folders[0].clone()),
            folders,
            prompt: line::Line::default(),
            at: Field::Harness,
        }
    }

    pub fn harness(&self) -> &str {
        &self.harnesses[self.harness]
    }

    /// The fields there are: a shell takes no prompt.
    fn fields(&self) -> Vec<Field> {
        if self.harness() == SHELL {
            vec![Field::Harness, Field::Folder]
        } else {
            vec![Field::Harness, Field::Folder, Field::Prompt]
        }
    }

    fn go(&mut self, by: isize) {
        let fields = self.fields();
        let n = fields.len() as isize;
        let cur = fields.iter().position(|f| *f == self.at).unwrap_or(0) as isize;
        self.at = fields[((cur + by) % n + n) as usize % n as usize];
    }

    fn cycle_harness(&mut self, by: isize) {
        let n = self.harnesses.len() as isize;
        self.harness = ((self.harness as isize + by) % n + n) as usize % n as usize;
    }

    fn cycle_folder(&mut self, by: isize) {
        let n = self.folders.len() as isize;
        let cur = self
            .folders
            .iter()
            .position(|f| f == self.folder.text())
            .map_or(if by > 0 { -1 } else { 0 }, |i| i as isize);
        let next = ((cur + by) % n + n) as usize % n as usize;
        self.folder = line_of(self.folders[next].clone());
    }

    pub fn key(&mut self, k: &KeyEvent) -> Act {
        match k.code {
            KeyCode::Esc => return Act::Cancel,
            KeyCode::Enter => return Act::Start,
            KeyCode::Tab => self.go(1),
            KeyCode::BackTab => self.go(-1),
            _ => match self.at {
                Field::Harness => match k.code {
                    KeyCode::Down | KeyCode::Right | KeyCode::Char(' ') => self.cycle_harness(1),
                    KeyCode::Up | KeyCode::Left => self.cycle_harness(-1),
                    _ => {}
                },
                Field::Folder => match k.code {
                    KeyCode::Down => self.cycle_folder(1),
                    KeyCode::Up => self.cycle_folder(-1),
                    _ => {
                        self.folder.key(k);
                    }
                },
                Field::Prompt => match k.code {
                    KeyCode::Up => self.go(-1),
                    _ => {
                        self.prompt.key(k);
                    }
                },
            },
        }
        Act::Typing
    }

    /// A paste goes into the field being typed; a picked one takes none.
    pub fn paste(&mut self, s: &str) {
        match self.at {
            Field::Folder => self.folder.insert(s),
            Field::Prompt => self.prompt.insert(s),
            Field::Harness => {}
        }
    }

    /// What it would start, or what is wrong with it.
    pub fn draft(&self) -> Result<Draft, String> {
        let typed = self.folder.text().trim();
        let cwd = expand(typed);
        if typed.is_empty() || !cwd.is_dir() {
            return Err(format!("no such folder: {typed}"));
        }
        if self.harness() == SHELL {
            return Ok(Draft {
                harness: None,
                cwd,
                argv: Vec::new(),
            });
        }
        let prompt = self.prompt.text().trim();
        if prompt.starts_with('-') {
            return Err("a prompt can't start with `-`: the agent would read it as a flag".into());
        }
        Ok(Draft {
            harness: Some(self.harness().to_string()),
            cwd,
            argv: (!prompt.is_empty()).then(|| prompt.to_string()).into_iter().collect(),
        })
    }

    /// Its lines, for the footer: the title, then a field each.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let label =
            |f: Field, name: &str| Span::styled(format!("{name:<8}"), if self.at == f { bold } else { theme::dim() });
        let mut out = vec![Line::from(vec![
            Span::styled("new session", bold),
            Span::styled("  ↑↓ picks · tab next · enter starts · esc cancels", theme::dim()),
        ])];
        let mut harness = vec![label(Field::Harness, "harness")];
        for (i, h) in self.harnesses.iter().enumerate() {
            if i > 0 {
                harness.push(Span::styled(" · ", theme::dim()));
            }
            harness.push(if i == self.harness {
                Span::styled(h.clone(), theme::accent().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(h.clone(), theme::dim())
            });
        }
        out.push(Line::from(harness));
        out.push(self.text_line(Field::Folder, label(Field::Folder, "folder"), &self.folder, ""));
        if self.harness() == SHELL {
            out.push(Line::from(vec![
                label(Field::Prompt, "prompt"),
                Span::styled("a shell takes none", theme::dim()),
            ]));
        } else {
            out.push(self.text_line(
                Field::Prompt,
                label(Field::Prompt, "prompt"),
                &self.prompt,
                "optional · what to start on",
            ));
        }
        out
    }

    fn text_line(&self, f: Field, label: Span<'static>, l: &line::Line, hint: &str) -> Line<'static> {
        let mut spans = vec![label];
        if self.at == f {
            spans.extend(theme::with_caret(l.text(), l.cursor(), Style::default()));
            if l.is_empty() && !hint.is_empty() {
                spans.push(Span::styled(format!(" {hint}"), theme::dim()));
            }
        } else if l.is_empty() {
            spans.push(Span::styled(hint.to_string(), theme::dim()));
        } else {
            spans.push(Span::raw(l.text().to_string()));
        }
        Line::from(spans)
    }

    /// How many lines it takes.
    pub fn height(&self) -> u16 {
        4
    }
}

/// `~` for home, and the rest as typed.
pub fn expand(s: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match s.strip_prefix('~') {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            PathBuf::from(format!("{home}{rest}"))
        }
        _ => PathBuf::from(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(f: &mut Form, code: KeyCode) -> Act {
        f.key(&KeyEvent::from(code))
    }

    fn typed(f: &mut Form, s: &str) {
        for c in s.chars() {
            press(f, KeyCode::Char(c));
        }
    }

    #[test]
    fn the_harnesses_cycle_round_with_a_shell_last() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = Form::new(&["claude".into(), "codex".into()], tmp.path(), &[]);
        assert_eq!(f.harnesses, ["claude", "codex", "shell"]);
        assert_eq!(f.harness(), "claude");
        press(&mut f, KeyCode::Down);
        assert_eq!(f.harness(), "codex");
        press(&mut f, KeyCode::Right);
        assert_eq!(f.harness(), "shell");
        press(&mut f, KeyCode::Down);
        assert_eq!(f.harness(), "claude", "round the end");
        press(&mut f, KeyCode::Up);
        assert_eq!(f.harness(), "shell");
        // A letter is nothing on a field that is picked.
        typed(&mut f, "x");
        assert_eq!(f.harness(), "shell");
    }

    #[test]
    fn the_folder_walks_the_known_ones_and_takes_a_path_typed() {
        let here = PathBuf::from("/w/relay");
        let known = vec![PathBuf::from("/w/api"), PathBuf::from("/w/relay")];
        let mut f = Form::new(&[], &here, &known);
        assert_eq!(
            f.folders,
            ["/w/relay", "/w/api"],
            "where it opened, then the rest, once"
        );
        press(&mut f, KeyCode::Tab);
        assert_eq!(f.at, Field::Folder);
        press(&mut f, KeyCode::Down);
        assert_eq!(f.folder.text(), "/w/api");
        press(&mut f, KeyCode::Down);
        assert_eq!(f.folder.text(), "/w/relay");
        press(&mut f, KeyCode::Up);
        assert_eq!(f.folder.text(), "/w/api");
        typed(&mut f, "/src");
        assert_eq!(f.folder.text(), "/w/api/src");
    }

    #[test]
    fn a_shell_takes_no_prompt_and_an_agent_takes_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let mut f = Form::new(&["claude".into()], &dir, &[]);
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        assert_eq!(f.at, Field::Prompt);
        typed(&mut f, "fix the build");
        assert_eq!(press(&mut f, KeyCode::Enter), Act::Start);
        assert_eq!(
            f.draft().unwrap(),
            Draft {
                harness: Some("claude".into()),
                cwd: dir.clone(),
                argv: vec!["fix the build".into()],
            }
        );
        press(&mut f, KeyCode::Tab);
        assert_eq!(f.at, Field::Harness, "round the fields");
        press(&mut f, KeyCode::Down);
        assert_eq!(f.harness(), "shell");
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        assert_eq!(f.at, Field::Harness, "a shell has no prompt to go to");
        assert_eq!(
            f.draft().unwrap(),
            Draft {
                harness: None,
                cwd: dir,
                argv: vec![],
            }
        );
        assert_eq!(press(&mut f, KeyCode::Esc), Act::Cancel);
    }

    #[test]
    fn a_folder_that_is_not_one_or_a_prompt_like_a_flag_is_said() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = Form::new(&["claude".into()], &tmp.path().join("nope"), &[]);
        assert!(f.draft().unwrap_err().starts_with("no such folder"));
        f.folder = line_of(tmp.path().display().to_string());
        f.at = Field::Prompt;
        typed(&mut f, "--yolo");
        assert!(f.draft().unwrap_err().contains("flag"));
    }
}
