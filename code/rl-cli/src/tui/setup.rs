//! The setup guide: a few screens, one question each, with an answer
//! already picked. Enter takes it, esc leaves that part as it is, ← goes
//! back. Moving sessions is the exception: nothing is picked for you and
//! enter asks once more, naming what it would stop, because that step acts
//! on something outside Relay and cannot be undone by going back. The first start shows it before the list, and again when a release
//! adds a step; `rl setup` shows all of it whenever you like. What it
//! settles goes into `config.yaml`, written by rld; how far it got goes into
//! the ledger, so the file stays yours.
//!
//! The last step writes into an agent's own file: the hooks of each agent
//! whose profile has them (`sessions.hooks`: Claude's, Codex's and
//! Gemini's), so their sessions started outside Relay show on the list. Only
//! what is there already starts picked, and what is added or taken out is
//! only what you turned on or off, written by `rl` itself
//! (`rl_detect::user_hooks`) where this environment says the agent looks.

use super::{Client, mark, theme};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rl_detect::Registry;
use rl_detect::sessions::UserHooks;
use rl_detect::user_hooks;
use rl_proto::config::Assistant;
use rl_proto::methods::{
    AgentInfo, ImportMove, ImportMoveParams, ImportReport, Outside, SETUP_STEPS, SetupDone, SetupSave, SetupState,
};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `s` when there is more than one.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Who is asked first for summaries: quick and cheap, in this order.
const SUMMARISERS: &[&str] = &["claude", "codex", "gemini"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Agents,
    Summaries,
    /// Claude's own sessions, moved over to Relay.
    Sessions,
    /// The agents' own sessions, shown by their hooks.
    Hooks,
}

/// Every step, in order: a release adds one at the end.
const STEPS: [Step; SETUP_STEPS as usize] = [Step::Agents, Step::Summaries, Step::Sessions, Step::Hooks];

/// An agent whose own sessions its hooks can show, here, and where its
/// hooks file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hookable {
    pub name: String,
    pub file: PathBuf,
    /// Whether Relay's hooks are in it; why that cannot be told, where the
    /// file does not read (and is then left alone).
    pub installed: Result<bool, String>,
    /// What the agent will say about it, worth knowing first: codex warns
    /// at every start when its `config.toml` has hooks too.
    pub warning: Option<String>,
    /// The agent's own settings its profile sets with the hooks that the
    /// file has no value for yet (`user_hooks::unset`), by dotted path.
    pub unset: Vec<String>,
    /// Its hooks, as its profile has them.
    pub hooks: UserHooks,
}

/// The agents found here whose hooks can show their sessions, by their
/// profiles (`registry`), each with its file as this environment places
/// it. Reads the files, writes nothing.
pub fn hookable(agents: &[AgentInfo]) -> Vec<Hookable> {
    hookable_in(agents, super::rows::registry(), |k| std::env::var_os(k))
}

fn hookable_in(
    agents: &[AgentInfo],
    registry: &Registry,
    env: impl Fn(&str) -> Option<OsString> + Copy,
) -> Vec<Hookable> {
    agents
        .iter()
        .filter(|a| a.available())
        .filter_map(|a| {
            let hooks = registry.user_hooks(&a.name)?;
            let file = hooks.file(env)?;
            let installed = user_hooks::installed(&file, hooks).map_err(|e| format!("{e:#}"));
            let warning = (a.name == user_hooks::CODEX && user_hooks::inline_hooks(&file))
                .then(|| "codex will warn at each start: its config.toml has hooks too".to_string());
            // A file that does not read says so through `installed`.
            let unset = user_hooks::unset(&file, hooks)
                .map(|keys| keys.into_iter().map(String::from).collect())
                .unwrap_or_default();
            Some(Hookable {
                name: a.name.clone(),
                file,
                installed,
                warning,
                unset,
                hooks: hooks.clone(),
            })
        })
        .collect()
}

/// One on the hooks screen.
#[derive(Debug, Clone)]
struct HookPick {
    it: Hookable,
    on: bool,
}

impl HookPick {
    fn can(&self) -> bool {
        self.it.installed.is_ok()
    }

    fn was(&self) -> bool {
        self.it.installed == Ok(true)
    }
}

/// An agent on the first screen.
#[derive(Debug, Clone)]
struct Pick {
    info: AgentInfo,
    on: bool,
}

/// One of Claude's sessions on the third.
#[derive(Debug, Clone)]
struct Session {
    it: Outside,
    on: bool,
}

impl Session {
    /// Whether it can be moved at all: not already here, not in a terminal.
    fn movable(&self) -> bool {
        self.it.run.is_none() && !self.it.in_a_terminal()
    }

    /// Why it cannot be moved, or how it stands.
    fn said(&self) -> String {
        if self.it.run.is_some() {
            "in Relay already".into()
        } else if self.it.in_a_terminal() {
            "open in a terminal · enter on its row carries it on".into()
        } else if self.it.working() {
            "working · moving it stops the turn".into()
        } else {
            self.it.state.clone().unwrap_or_else(|| self.it.status.clone())
        }
    }
}

/// What the guide holds while it is open.
#[derive(Debug)]
pub struct Guide {
    /// The steps this run of the guide asks, in order.
    steps: Vec<Step>,
    step: usize,
    agents: Vec<Pick>,
    sessions: Vec<Session>,
    /// Why there are no sessions to show, when that is the reason.
    sessions_error: Option<String>,
    hooks: Vec<HookPick>,
    /// What each agent's `--version` said, as it comes in.
    versions: HashMap<String, String>,
    /// Who could summarise, given the agents picked.
    cursor: usize,
    summariser: Option<Assistant>,
    /// Whether each step was answered (enter) or left (esc), by step.
    answered: [bool; SETUP_STEPS as usize],
    /// What the config said, to write only what changed.
    was_agents: Vec<String>,
    was_summariser: Option<Assistant>,
    done: bool,
    quit: bool,
    /// On the sessions step: the list has been answered and the guide is
    /// asking whether to really stop them.
    confirm: bool,
}

/// What leaving the guide came to.
pub enum Outcome {
    /// Saved; the words say what changed.
    Saved(String),
    /// Left with ^c: nothing written, asked again next start.
    Later,
}

impl Guide {
    /// The whole guide: what is set, what could be moved over, and whose
    /// hooks could show their sessions (`hookable`). No agent's hooks: no
    /// screen for them.
    pub fn new(state: &SetupState, sessions: &ImportReport, hooks: Vec<Hookable>) -> Self {
        let mut g = Self::of(state, sessions, hooks, STEPS.to_vec());
        g.steps.retain(|s| *s != Step::Hooks || !g.hooks.is_empty());
        g.enter_step();
        g
    }

    /// `/import`, `rl import`: the sessions step on its own.
    pub fn importing(state: &SetupState, sessions: &ImportReport) -> Self {
        let mut g = Self::of(state, sessions, vec![], vec![Step::Sessions]);
        g.enter_step();
        g
    }

    /// The guide from a release's new steps on: what the last time through
    /// did not ask. Nothing new: `None`.
    pub fn since(state: &SetupState, sessions: &ImportReport, hooks: Vec<Hookable>) -> Option<Self> {
        let steps: Vec<Step> = STEPS
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i as u32 >= state.seen)
            .map(|(_, s)| s)
            .collect();
        let mut g = Self::of(state, sessions, hooks, steps);
        // A step with nothing to show is not worth a screen.
        g.steps.retain(|s| match s {
            Step::Sessions => !g.sessions.is_empty(),
            Step::Hooks => !g.hooks.is_empty(),
            _ => true,
        });
        if g.steps.is_empty() {
            return None;
        }
        g.enter_step();
        Some(g)
    }

    fn of(state: &SetupState, sessions: &ImportReport, hooks: Vec<Hookable>, steps: Vec<Step>) -> Self {
        let named = &state.config.config.agents;
        let agents = state
            .agents
            .iter()
            .map(|a| Pick {
                on: a.available() && (named.is_empty() || named.contains(&a.name)),
                info: a.clone(),
            })
            .collect();
        let current = state.config.config.assistants.summary.clone();
        let sessions_error = sessions.error.clone();
        let sessions: Vec<Session> = sessions
            .sessions
            .iter()
            .map(|s| Session {
                // Never picked for you: enter on an untouched list must move
                // nothing, so tapping through the guide cannot stop an agent.
                on: false,
                it: s.clone(),
            })
            .collect();
        let mut g = Guide {
            steps,
            step: 0,
            agents,
            sessions,
            sessions_error,
            // Only what is there already starts picked: a hook is only ever
            // added on a yes.
            hooks: hooks
                .into_iter()
                .map(|it| HookPick {
                    on: it.installed == Ok(true),
                    it,
                })
                .collect(),
            versions: HashMap::new(),
            cursor: 0,
            summariser: None,
            answered: [false; SETUP_STEPS as usize],
            was_agents: named.clone(),
            was_summariser: current.clone(),
            done: false,
            quit: false,
            confirm: false,
        };
        g.summariser = current.or_else(|| g.summarisers().into_iter().next());
        g
    }

    fn at(&self) -> Step {
        self.steps[self.step]
    }

    /// Every session picked to move, confirmed or not.
    fn picked(&self) -> Vec<&Session> {
        self.sessions.iter().filter(|s| s.on && s.movable()).collect()
    }

    /// `a` on the sessions step: all of them, or none if all are already on.
    fn all(&mut self) {
        let want = self.picked().len() < self.sessions.iter().filter(|s| s.movable()).count();
        for s in &mut self.sessions {
            if s.movable() {
                s.on = want;
            }
        }
    }

    /// The sessions to move, once the guide is through. Only ever what was
    /// picked by hand and then confirmed.
    pub fn moving(&self) -> Vec<Outside> {
        if !self.answered[Step::Sessions as usize] {
            return vec![];
        }
        self.picked().into_iter().map(|s| s.it.clone()).collect()
    }

    /// The hooks to add or bring up to date (true) or take out (false),
    /// once the guide is through and the screen answered: every one left
    /// on — so one with a stale `rl` or a missing event is put right, and
    /// one already right is left as it is — and every one turned off. One
    /// never on is never touched.
    pub fn hooking(&self) -> Vec<(Hookable, bool)> {
        if !self.answered[Step::Hooks as usize] {
            return vec![];
        }
        self.hooks
            .iter()
            .filter(|h| h.can() && (h.on || h.was()))
            .map(|h| (h.it.clone(), h.on))
            .collect()
    }

    /// Everyone who could do summaries: the picked agents that are here,
    /// the quick ones first, each on its small model; and whatever the
    /// config already names, if it is none of those.
    fn summarisers(&self) -> Vec<Assistant> {
        let mut here: Vec<&AgentInfo> = self
            .agents
            .iter()
            .filter(|p| p.on && p.info.available())
            .map(|p| &p.info)
            .collect();
        here.sort_by_key(|a| SUMMARISERS.iter().position(|n| *n == a.name).unwrap_or(usize::MAX));
        let mut out: Vec<Assistant> = here
            .into_iter()
            .map(|a| Assistant {
                agent: a.name.clone(),
                model: a.small_model.clone().filter(|_| !a.model_flag.is_empty()),
                args: vec![],
            })
            .collect();
        if let Some(c) = &self.was_summariser
            && !out.contains(c)
        {
            out.insert(0, c.clone());
        }
        out
    }

    fn rows(&self) -> usize {
        match self.at() {
            Step::Agents => self.agents.len(),
            Step::Summaries => self.summarisers().len(),
            Step::Sessions => self.sessions.len(),
            Step::Hooks => self.hooks.len(),
        }
    }

    /// Where the cursor starts on a step: the first agent, or the one picked.
    fn enter_step(&mut self) {
        self.cursor = match self.at() {
            Step::Agents | Step::Sessions | Step::Hooks => 0,
            Step::Summaries => {
                let all = self.summarisers();
                if self.summariser.as_ref().is_none_or(|s| !all.contains(s)) {
                    self.summariser = all.first().cloned();
                }
                self.summariser
                    .as_ref()
                    .and_then(|s| all.iter().position(|x| x == s))
                    .unwrap_or(0)
            }
        };
    }

    fn next(&mut self, answered: bool) {
        self.answered[self.at() as usize] = answered;
        self.confirm = false;
        if self.step + 1 == self.steps.len() {
            self.done = true;
        } else {
            self.step += 1;
            self.enter_step();
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.rows().saturating_sub(1));
            }
            KeyCode::Left | KeyCode::BackTab => {
                if self.confirm {
                    self.confirm = false;
                } else if self.step > 0 {
                    self.step -= 1;
                    self.enter_step();
                }
            }
            KeyCode::Char(' ') if !self.confirm => self.choose(),
            KeyCode::Char('a') if !self.confirm && self.at() == Step::Sessions => self.all(),
            KeyCode::Enter => match self.at() {
                // Nothing picked is an answer: it leaves them with Claude.
                Step::Sessions if self.picked().is_empty() => self.next(false),
                // Picked, but not yet confirmed: say what would stop, first.
                Step::Sessions if !self.confirm => self.confirm = true,
                Step::Summaries => {
                    self.choose();
                    self.next(true);
                }
                _ => self.next(true),
            },
            KeyCode::Esc if self.confirm => self.confirm = false,
            KeyCode::Esc => self.next(false),
            _ => {}
        }
    }

    /// Space: an agent in or out; a summariser picked.
    fn choose(&mut self) {
        match self.at() {
            Step::Sessions => {
                if let Some(s) = self.sessions.get_mut(self.cursor)
                    && s.movable()
                {
                    s.on = !s.on;
                }
            }
            Step::Agents => {
                if let Some(p) = self.agents.get_mut(self.cursor)
                    && p.info.available()
                {
                    p.on = !p.on;
                }
            }
            Step::Hooks => {
                if let Some(h) = self.hooks.get_mut(self.cursor)
                    && h.can()
                {
                    h.on = !h.on;
                }
            }
            Step::Summaries => {
                if let Some(s) = self.summarisers().get(self.cursor) {
                    self.summariser = Some(s.clone());
                }
            }
        }
    }

    /// What to write: only what was answered and differs from the file.
    pub fn save(&self) -> SetupSave {
        let mut out = SetupSave {
            seen: SETUP_STEPS,
            ..Default::default()
        };
        if self.answered[Step::Agents as usize] {
            let on: Vec<String> = self
                .agents
                .iter()
                .filter(|p| p.on)
                .map(|p| p.info.name.clone())
                .collect();
            let all_here: Vec<String> = self
                .agents
                .iter()
                .filter(|p| p.info.available())
                .map(|p| p.info.name.clone())
                .collect();
            // Everything here and nothing named: that is what unset means already.
            let same = if self.was_agents.is_empty() {
                on == all_here
            } else {
                on == self.was_agents
            };
            if !same {
                out.agents = Some(if on == all_here { vec![] } else { on });
            }
        }
        if self.answered[Step::Summaries as usize] && self.summariser != self.was_summariser {
            out.assistant = self.summariser.clone();
        }
        out
    }

    pub fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(4),
            height: area.height.saturating_sub(2),
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled("relay", theme::accent().add_modifier(Modifier::BOLD)),
                Span::raw(" setup"),
                Span::styled(format!("   {} of {}", self.step + 1, self.steps.len()), theme::dim()),
            ]),
            Line::raw(""),
        ];
        match self.at() {
            Step::Agents => self.draw_agents(&mut lines),
            Step::Summaries => self.draw_summaries(&mut lines),
            Step::Sessions if self.confirm => self.draw_confirm(&mut lines),
            Step::Sessions => self.draw_sessions(&mut lines),
            Step::Hooks => self.draw_hooks(&mut lines),
        }
        lines.push(Line::raw(""));
        let keys = match self.at() {
            Step::Agents => "space in or out · enter next · esc leave as it is · ← back · q later",
            Step::Summaries => "↑↓ pick · enter save · esc leave as it is · ← back · q later",
            Step::Sessions if self.confirm => "enter stops them and moves them · esc back to the list",
            Step::Sessions if self.picked().is_empty() => {
                "space picks one · a all · enter leaves them with Claude · ← back · q later"
            }
            Step::Sessions => "space in or out · a all · enter asks once more · esc leaves them · ← back",
            Step::Hooks => "space yes or no · enter writes what changed · esc leave as it is · ← back · q later",
        };
        lines.push(Line::styled(keys, theme::dim()));
        // Where the screen has room for it and the guide both, the mark
        // stands above the title. Its first column is air, so it starts a
        // column early and its ink stands level with the title.
        let w = inner.width.max(1) as usize;
        let rows: usize = lines.iter().map(|l| l.width().div_ceil(w).max(1)).sum();
        let tall = mark::FULL.len() as u16 + 1;
        let inner = if rows + tall as usize <= inner.height as usize && inner.width >= mark::W {
            let at = Rect {
                x: inner.x - 1,
                width: mark::W,
                height: mark::FULL.len() as u16,
                ..inner
            };
            f.render_widget(Paragraph::new(mark::lines(&mark::FULL)), at);
            Rect {
                y: inner.y + tall,
                height: inner.height - tall,
                ..inner
            }
        } else {
            inner
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn draw_agents(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::styled("Which agents should Relay offer?", theme::name()));
        lines.push(Line::styled(
            "The panel suggests these when you start a run. Found on this machine: picked.",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        let width = self.agents.iter().map(|p| p.info.name.len()).max().unwrap_or(0);
        for (i, p) in self.agents.iter().enumerate() {
            let here = p.info.available();
            let mark = match (here, p.on) {
                (false, _) => "[ ]",
                (true, true) => "[x]",
                (true, false) => "[ ]",
            };
            let about = if here {
                self.versions.get(&p.info.name).cloned().unwrap_or_default()
            } else {
                "not found".into()
            };
            let mut spans = vec![
                Span::raw(if i == self.cursor { "▸ " } else { "  " }),
                Span::styled(format!("{mark} "), if p.on { theme::accent() } else { theme::dim() }),
                Span::styled(
                    format!("{:<width$}   ", p.info.name),
                    if here { theme::name() } else { theme::dim() },
                ),
                Span::styled(about, theme::dim()),
            ];
            if i == self.cursor {
                for s in &mut spans {
                    s.style = s.style.patch(theme::selected());
                }
            }
            lines.push(Line::from(spans));
        }
    }

    fn draw_sessions(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::styled("Move your Claude sessions into Relay?", theme::name()));
        lines.push(Line::styled(
            "Claude's background process stops — the conversation is kept — and Relay resumes it here.",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        if let Some(e) = &self.sessions_error {
            lines.push(Line::styled(format!("  Claude did not say: {e}"), theme::dim()));
            return;
        }
        if self.sessions.is_empty() {
            lines.push(Line::styled("  Claude has none running.", theme::dim()));
            return;
        }
        lines.push(Line::styled(
            "  Nothing is picked: space takes one, a takes all. Enter on an empty list leaves them be.",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        let width = self
            .sessions
            .iter()
            .map(|s| s.it.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(40);
        for (i, s) in self.sessions.iter().enumerate() {
            let mark = if !s.movable() {
                "   "
            } else if s.on {
                "[x]"
            } else {
                "[ ]"
            };
            let mut spans = vec![
                Span::raw(if i == self.cursor { "▸ " } else { "  " }),
                Span::styled(format!("{mark} "), if s.on { theme::accent() } else { theme::dim() }),
                Span::styled(
                    format!("{:<width$}  ", super::rows::fit(&s.it.name, width)),
                    if s.movable() { theme::name() } else { theme::dim() },
                ),
                Span::styled(format!("{:<38}  ", s.said()), theme::dim()),
                Span::styled(rl_core::display_path(&s.it.cwd), theme::dim()),
            ];
            if i == self.cursor {
                for x in &mut spans {
                    x.style = x.style.patch(theme::selected());
                }
            }
            lines.push(Line::from(spans));
        }
    }

    /// The last word before anything outside Relay is touched: what would
    /// stop, by name, and nothing else on the screen.
    fn draw_confirm(&self, lines: &mut Vec<Line<'static>>) {
        let picked = self.picked();
        let n = picked.len();
        lines.push(Line::styled(
            format!(
                "Stop {n} Claude session{} and move {} here?",
                plural(n),
                if n == 1 { "it" } else { "them" }
            ),
            theme::name(),
        ));
        lines.push(Line::styled(
            "Claude stops holding each one — the conversation is kept — and Relay resumes it as a run.",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        let width = picked
            .iter()
            .map(|s| s.it.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(40);
        for s in &picked {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!("{:<width$}  ", super::rows::fit(&s.it.name, width)),
                    theme::name(),
                ),
                Span::styled(rl_core::display_path(&s.it.cwd), theme::dim()),
            ]));
        }
    }

    fn draw_summaries(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::styled("Who writes summaries and drafts runs?", theme::name()));
        lines.push(Line::styled(
            "One short call each time you ask for a recap or type in plain words, and one for the top of \
             what happened when you come back after a while (catch_up.summary: ask turns that off).",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        let all = self.summarisers();
        if all.is_empty() {
            lines.push(Line::styled(
                "  None of the agents picked is on this machine; enter goes on without one.",
                theme::dim(),
            ));
            return;
        }
        for (i, a) in all.iter().enumerate() {
            let on = self.summariser.as_ref() == Some(a);
            let mut spans = vec![
                Span::raw(if i == self.cursor { "▸ " } else { "  " }),
                Span::styled(
                    if on { "(•) " } else { "( ) " },
                    if on { theme::accent() } else { theme::dim() },
                ),
                Span::styled(a.agent.clone(), theme::name()),
            ];
            if let Some(m) = &a.model {
                spans.push(Span::styled(format!(" · {m}"), theme::dim()));
            }
            if self.was_summariser.as_ref() == Some(a) {
                spans.push(Span::styled("   as config.yaml has it", theme::dim()));
            }
            if i == self.cursor {
                for s in &mut spans {
                    s.style = s.style.patch(theme::selected());
                }
            }
            lines.push(Line::from(spans));
        }
    }
}

impl Guide {
    fn draw_hooks(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::styled("Show sessions started outside Relay?", theme::name()));
        lines.push(Line::styled(
            "Relay hears of them from the agent's own hooks: a yes adds Relay's to the file named \
             (or brings them up to date), a no takes them out again. Nothing else in it changes, \
             though the file is written back in its own layout.",
            theme::dim(),
        ));
        lines.push(Line::raw(""));
        let width = self.hooks.iter().map(|h| h.it.name.len()).max().unwrap_or(0);
        for (i, h) in self.hooks.iter().enumerate() {
            let mark = match (h.can(), h.on) {
                (false, _) => "   ",
                (true, true) => "[x]",
                (true, false) => "[ ]",
            };
            let file = rl_core::display_path(&h.it.file);
            let mut about = match (&h.it.installed, h.on) {
                (Err(e), _) => e.clone(),
                (Ok(true), true) => format!("its hooks are in {file}"),
                (Ok(true), false) => format!("takes Relay's hooks out of {file}"),
                (Ok(false), _) => format!(
                    "Show {} sessions started outside Relay? This adds a hook to {file}",
                    display(&h.it.name)
                ),
            };
            if h.can() && h.on && !h.it.unset.is_empty() {
                about = format!("{about} · and sets {}", h.it.unset.join(", "));
            }
            if let Some(w) = &h.it.warning
                && h.can()
            {
                about = format!("{about} · {w}");
            }
            let mut spans = vec![
                Span::raw(if i == self.cursor { "▸ " } else { "  " }),
                Span::styled(format!("{mark} "), if h.on { theme::accent() } else { theme::dim() }),
                Span::styled(
                    format!("{:<width$}   ", h.it.name),
                    if h.can() { theme::name() } else { theme::dim() },
                ),
                Span::styled(about, theme::dim()),
            ];
            if i == self.cursor {
                for s in &mut spans {
                    s.style = s.style.patch(theme::selected());
                }
            }
            lines.push(Line::from(spans));
        }
    }
}

/// An agent's name as a word: its own capitalised (`Codex`).
fn display(agent: &str) -> String {
    let mut c = agent.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Adds or takes out each agent's hooks as the guide settled, in words for
/// the flash line: what changed, and what the agent asks of you after
/// (its profile's `after_install`).
pub fn hook_them(hooking: &[(Hookable, bool)], rl_bin: &Path) -> String {
    let mut said = Vec::new();
    for (h, on) in hooking {
        let file = rl_core::display_path(&h.file);
        let was = h.installed == Ok(true);
        let done = if *on {
            user_hooks::install(&h.file, &h.hooks, rl_bin)
        } else {
            user_hooks::uninstall(&h.file, &h.hooks)
        };
        let set = if *on && !h.unset.is_empty() {
            format!(" · {} set", h.unset.join(", "))
        } else {
            String::new()
        };
        let trust = (h.hooks.after_install.as_deref())
            .map(|a| format!("{set} · {a}"))
            .unwrap_or(set);
        said.push(match (done, on) {
            (Err(e), _) => format!("{}: {e:#}", h.name),
            // There and right already: nothing to say.
            (Ok(false), _) => continue,
            (Ok(true), true) if was => format!("{} hooks in {file} brought up to date{trust}", h.name),
            (Ok(true), true) => format!("{} hooks added to {file}{trust}", h.name),
            (Ok(true), false) => format!("{} hooks taken out of {file}", h.name),
        });
    }
    said.join(" · ")
}

/// The `rl` the hooks call: this one, by its name on `PATH` where that is
/// this very file (a link that outlives a reinstall), else by its own path.
/// Never another `rl` on `PATH`: an older one would not know the words. A
/// path that is not whole is none.
pub fn rl_for_hooks() -> Option<PathBuf> {
    pick_rl(
        rl_detect::registry::find_bin(&["rl".to_string()]),
        std::env::current_exe().ok(),
    )
}

fn pick_rl(on_path: Option<PathBuf>, this: Option<PathBuf>) -> Option<PathBuf> {
    let this = this.filter(|p| p.is_absolute())?;
    let same = |p: &PathBuf| match (std::fs::canonicalize(p), std::fs::canonicalize(&this)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    Some(on_path.filter(|p| p.is_absolute() && same(p)).unwrap_or(this))
}

/// What `<bin> --version` says, first line, for the agents found. Asked
/// all at once and shown as each comes in; one that hangs is left out.
fn ask_versions(agents: &[AgentInfo]) -> tokio::sync::mpsc::UnboundedReceiver<(String, String)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for a in agents {
        let Some(bin) = a.bin.clone() else { continue };
        let (tx, name) = (tx.clone(), a.name.clone());
        tokio::spawn(async move {
            let out = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::process::Command::new(bin)
                    .arg("--version")
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            if let Ok(Ok(o)) = out
                && o.status.success()
                && let Some(line) = String::from_utf8_lossy(&o.stdout).lines().next()
            {
                let line: String = line.trim().chars().take(40).collect();
                if !line.is_empty() {
                    let _ = tx.send((name, line));
                }
            }
        });
    }
    rx
}

/// The guide on the screen until it is saved or left. Moving the sessions
/// it settled on happens after: one call each, said as they go.
pub async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rpc: &mut Client,
    mut guide: Guide,
    agents: &[AgentInfo],
) -> Result<Outcome> {
    let mut versions = ask_versions(agents);
    let mut input = EventStream::new();
    loop {
        terminal.draw(|f| guide.draw(f))?;
        if guide.quit {
            return Ok(Outcome::Later);
        }
        if guide.done {
            let save = guide.save();
            let mut words = said(&save);
            rpc.call::<SetupDone>(save).await?;
            let hooking = guide.hooking();
            if !hooking.is_empty() {
                let hooked = match rl_for_hooks() {
                    Some(rl) => hook_them(&hooking, &rl),
                    None => "hooks left as they were: where this rl lives is not known".into(),
                };
                words = match words.is_empty() {
                    true => hooked,
                    false => format!("{words} · {hooked}"),
                };
            }
            let moving = guide.moving();
            if !moving.is_empty() {
                drop(input);
                let moved = move_them(terminal, rpc, &moving).await;
                words = match words.is_empty() {
                    true => moved,
                    false => format!("{words} · {moved}"),
                };
            }
            return Ok(Outcome::Saved(words));
        }
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => guide.key(super::input::typed(k)),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(Outcome::Later),
            },
            Some((name, v)) = versions.recv() => {
                guide.versions.insert(name, v);
            }
        }
    }
}

/// Each session in turn, with the screen saying where it is up to. One
/// that will not move says why and the rest still go.
async fn move_them(terminal: &mut ratatui::DefaultTerminal, rpc: &mut Client, moving: &[Outside]) -> String {
    let mut done: Vec<Line<'static>> = Vec::new();
    let mut ok = 0;
    let mut failed = Vec::new();
    for s in moving {
        let mut lines = vec![
            Line::styled("Moving your Claude sessions into Relay", theme::name()),
            Line::raw(""),
        ];
        lines.extend(done.clone());
        lines.push(Line::styled(format!("  … {}", s.name), theme::dim()));
        let _ = terminal.draw(|f| {
            let area = f.area();
            let inner = Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width.saturating_sub(4),
                height: area.height.saturating_sub(2),
            };
            f.render_widget(Paragraph::new(lines.clone()).wrap(Wrap { trim: false }), inner);
        });
        let env = crate::env::child_env(&[]).unwrap_or_default();
        match rpc
            .call::<ImportMove>(ImportMoveParams {
                agent: s.agent.clone(),
                session_id: s.session_id.clone(),
                env,
                cols: None,
                rows: None,
            })
            .await
        {
            Ok(run) => {
                ok += 1;
                done.push(Line::from(vec![
                    Span::styled("  ✓ ", theme::accent()),
                    Span::styled(s.name.clone(), theme::name()),
                    Span::styled(format!("  now run {}", run.id), theme::dim()),
                ]));
            }
            Err(e) => {
                let why = format!("{e:#}");
                failed.push(s.name.clone());
                done.push(Line::from(vec![
                    Span::styled("  ✗ ", theme::dim()),
                    Span::styled(s.name.clone(), theme::dim()),
                    Span::styled(format!("  {why}"), theme::dim()),
                ]));
            }
        }
    }
    let mut words = format!("moved {ok} Claude session{}", if ok == 1 { "" } else { "s" });
    if !failed.is_empty() {
        words.push_str(&format!(" · {} would not move: {}", failed.len(), failed.join(", ")));
    }
    words
}

/// What saving changed, in words for the flash line.
fn said(s: &SetupSave) -> String {
    let mut parts = Vec::new();
    match &s.agents {
        Some(a) if a.is_empty() => parts.push("every agent offered".to_string()),
        Some(a) => parts.push(format!("offering {}", a.join(", "))),
        None => {}
    }
    if let Some(a) = &s.assistant {
        parts.push(format!(
            "summaries by {}{}",
            a.agent,
            a.model.as_deref().map(|m| format!(" · {m}")).unwrap_or_default()
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("setup saved: {} · rl setup to change it", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::config::Config;
    use rl_proto::methods::ConfigReport;

    fn agent(name: &str, here: bool, small: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: name.into(),
            bins: vec![name.into()],
            bin: here.then(|| format!("/bin/{name}").into()),
            headless: vec!["-p".into()],
            model_flag: vec!["-m".into()],
            small_model: small.map(Into::into),
            offered: true,
        }
    }

    fn state(config: Config) -> SetupState {
        SetupState {
            seen: 0,
            config: ConfigReport {
                file: "/h/config.yaml".into(),
                present: true,
                config,
                interpret: None,
                summary: None,
                catch_up_after: "8h".into(),
                catch_up_summary: "auto".into(),
                errors: vec![],
            },
            agents: vec![
                agent("claude", true, Some("haiku")),
                agent("codex", true, Some("luna")),
                agent("cursor", false, None),
                agent("gemini", true, Some("flash-lite")),
                agent("opencode", true, None),
            ],
        }
    }

    /// No sessions of Claude's, and nothing wrong: the usual case.
    fn none() -> ImportReport {
        ImportReport::default()
    }

    fn session(name: &str, id: Option<&str>, state: Option<&str>, run: Option<&str>) -> Outside {
        Outside {
            agent: "claude".into(),
            session_id: format!("s-{name}"),
            id: id.map(Into::into),
            name: name.into(),
            cwd: format!("/w/{name}").into(),
            kind: if id.is_some() { "background" } else { "interactive" }.into(),
            status: if state == Some("working") { "busy" } else { "idle" }.into(),
            state: state.map(Into::into),
            run: run.map(Into::into),
            new: run.is_none(),
            pid: None,
            started_at: None,
            waiting_for: None,
            workspace: None,
            state_at: None,
            was: None,
        }
    }

    fn press(g: &mut Guide, code: KeyCode) {
        g.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn the_sessions_step_moves_what_is_picked() {
        let sessions = ImportReport {
            sessions: vec![
                session("done-one", Some("a1"), Some("done"), None),
                session("busy-one", Some("b2"), Some("working"), None),
                session("in-a-terminal", None, None, None),
                session("here-already", Some("c3"), Some("done"), Some("r7")),
            ],
            error: None,
        };
        let mut g = Guide::importing(&state(Config::default()), &sessions);
        assert_eq!(g.steps.len(), 1);
        // Nothing is picked for you, whatever state it is in.
        assert!(g.sessions.iter().all(|s| !s.on), "nothing picked to start");
        // Picked by hand: a ready one and a working one. The others cannot be.
        g.cursor = 0;
        press(&mut g, KeyCode::Char(' '));
        g.cursor = 1;
        press(&mut g, KeyCode::Char(' '));
        g.cursor = 2;
        press(&mut g, KeyCode::Char(' '));
        g.cursor = 3;
        press(&mut g, KeyCode::Char(' '));
        // The first enter only asks; nothing has moved yet.
        press(&mut g, KeyCode::Enter);
        assert!(g.confirm && !g.done, "it asks before it acts");
        assert!(g.moving().is_empty(), "nothing moves while it is still asking");
        press(&mut g, KeyCode::Enter);
        assert!(g.done);
        let moving: Vec<String> = g.moving().into_iter().map(|s| s.name).collect();
        assert_eq!(moving, ["done-one", "busy-one"]);
        assert_eq!(
            g.sessions[2].said(),
            "open in a terminal · enter on its row carries it on"
        );
        assert_eq!(g.sessions[3].said(), "in Relay already");
    }

    /// The whole guide answered by holding enter must not stop an agent.
    #[test]
    fn enter_all_the_way_through_moves_nothing() {
        let sessions = ImportReport {
            sessions: vec![
                session("one", Some("a1"), Some("done"), None),
                session("two", Some("b2"), Some("done"), None),
            ],
            error: None,
        };
        let mut g = Guide::new(&state(Config::default()), &sessions, vec![]);
        for _ in 0..8 {
            press(&mut g, KeyCode::Enter);
        }
        assert!(g.done);
        assert!(g.moving().is_empty(), "holding enter moved {:?}", g.moving());
    }

    #[test]
    fn a_picks_all_of_them_and_esc_backs_out_of_the_asking() {
        let sessions = ImportReport {
            sessions: vec![
                session("one", Some("a1"), Some("done"), None),
                session("two", Some("b2"), Some("done"), None),
                session("here-already", Some("c3"), Some("done"), Some("r7")),
            ],
            error: None,
        };
        let mut g = Guide::importing(&state(Config::default()), &sessions);
        press(&mut g, KeyCode::Char('a'));
        assert_eq!(g.picked().len(), 2, "all that can move, and no more");
        press(&mut g, KeyCode::Enter);
        assert!(g.confirm);
        // Second thoughts: back to the list, still picked, nothing moved.
        press(&mut g, KeyCode::Esc);
        assert!(!g.confirm && !g.done);
        assert_eq!(g.picked().len(), 2);
        // And `a` again takes them all back off.
        press(&mut g, KeyCode::Char('a'));
        assert!(g.picked().is_empty());
        press(&mut g, KeyCode::Enter);
        assert!(g.done && g.moving().is_empty(), "an empty list is an answer");
    }

    #[test]
    fn esc_on_the_sessions_step_moves_nothing() {
        let sessions = ImportReport {
            sessions: vec![session("done-one", Some("a1"), Some("done"), None)],
            error: None,
        };
        let mut g = Guide::importing(&state(Config::default()), &sessions);
        press(&mut g, KeyCode::Esc);
        assert!(g.done && g.moving().is_empty());
    }

    #[test]
    fn only_the_steps_a_release_added_are_asked_again() {
        let mut st = state(Config::default());
        st.seen = 2;
        let sessions = ImportReport {
            sessions: vec![session("done-one", Some("a1"), Some("done"), None)],
            error: None,
        };
        let g = Guide::since(&st, &sessions, vec![]).expect("the sessions step is new");
        assert_eq!(g.steps, [Step::Sessions]);
        // Nothing of Claude's: nothing left to ask.
        assert!(Guide::since(&st, &none(), vec![]).is_none());
        // Never gone through: all of it.
        let g = Guide::since(&state(Config::default()), &sessions, vec![]).unwrap();
        assert_eq!(g.steps.len(), 3);
    }

    #[test]
    fn the_summaries_step_says_the_catch_up_asks_too_and_how_to_stop_it() {
        let g = Guide::new(&state(Config::default()), &none(), vec![]);
        let mut lines = Vec::new();
        g.draw_summaries(&mut lines);
        let said: String = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(said.contains("when you come back after a while"), "{said}");
        assert!(said.contains("catch_up.summary: ask"), "{said}");
        assert!(!said.contains("never on its own"), "{said}");
    }

    #[test]
    fn enter_twice_takes_the_defaults() {
        let mut g = Guide::new(&state(Config::default()), &none(), vec![]);
        press(&mut g, KeyCode::Enter);
        press(&mut g, KeyCode::Enter);
        press(&mut g, KeyCode::Enter);
        assert!(g.done);
        let s = g.save();
        assert_eq!(s.seen, SETUP_STEPS);
        assert_eq!(s.agents, None, "everything found is what unset means");
        assert_eq!(
            s.assistant,
            Some(Assistant {
                agent: "claude".into(),
                model: Some("haiku".into()),
                args: vec![]
            })
        );
        assert_eq!(
            said(&s),
            "setup saved: summaries by claude · haiku · rl setup to change it"
        );
    }

    #[test]
    fn leaving_claude_out_makes_codex_the_summariser() {
        let mut g = Guide::new(&state(Config::default()), &none(), vec![]);
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Down);
        press(&mut g, KeyCode::Down);
        // cursor, not on this machine: space does nothing
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Enter);
        press(&mut g, KeyCode::Enter);
        let s = g.save();
        assert_eq!(
            s.agents.as_deref(),
            Some(&["codex".to_string(), "gemini".into(), "opencode".into()][..])
        );
        assert_eq!(s.assistant.unwrap().agent, "codex");
    }

    #[test]
    fn esc_leaves_the_file_as_it_is() {
        let mut g = Guide::new(&state(Config::default()), &none(), vec![]);
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Esc);
        press(&mut g, KeyCode::Esc);
        press(&mut g, KeyCode::Esc);
        assert!(g.done);
        let s = g.save();
        assert_eq!((s.agents, s.assistant, s.seen), (None, None, SETUP_STEPS));
    }

    #[test]
    fn what_the_config_says_is_picked_and_kept() {
        let mut c = Config {
            agents: vec!["codex".into()],
            ..Default::default()
        };
        c.assistants.summary = Some(Assistant {
            agent: "codex".into(),
            model: Some("o3".into()),
            args: vec![],
        });
        let mut g = Guide::new(&state(c), &none(), vec![]);
        assert_eq!(g.agents.iter().filter(|p| p.on).count(), 1);
        press(&mut g, KeyCode::Enter);
        assert_eq!(
            g.summarisers()[0].model.as_deref(),
            Some("o3"),
            "the config's own first"
        );
        press(&mut g, KeyCode::Enter);
        let s = g.save();
        assert_eq!(
            (s.agents, s.assistant),
            (None, None),
            "nothing changed, nothing written"
        );
    }

    #[test]
    fn back_and_q() {
        let mut g = Guide::new(&state(Config::default()), &none(), vec![]);
        press(&mut g, KeyCode::Enter);
        assert_eq!(g.step, 1);
        press(&mut g, KeyCode::Left);
        assert_eq!(g.step, 0);
        press(&mut g, KeyCode::Char('q'));
        assert!(g.quit && !g.done);
    }

    #[test]
    fn a_missing_agent_cannot_be_picked() {
        let mut g = Guide::new(&state(Config::default()), &none(), vec![]);
        g.cursor = 2;
        press(&mut g, KeyCode::Char(' '));
        assert!(!g.agents[2].on);
    }

    #[test]
    fn it_draws() {
        let g = Guide::new(&state(Config::default()), &none(), vec![]);
        let backend = ratatui::backend::TestBackend::new(90, 16);
        let mut t = ratatui::Terminal::new(backend).unwrap();
        t.draw(|f| g.draw(f)).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content()
            .chunks(90)
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect();
        assert!(text.contains("relay setup   1 of 3"), "{text}");
        assert!(text.contains("[x] claude"), "{text}");
        assert!(text.contains("[ ] cursor     not found"), "{text}");
        assert!(!text.contains('█'), "no room for the mark: {text}");
    }

    #[test]
    fn the_mark_stands_above_the_title_where_there_is_room() {
        let g = Guide::new(&state(Config::default()), &none(), vec![]);
        let backend = ratatui::backend::TestBackend::new(90, 30);
        let mut t = ratatui::Terminal::new(backend).unwrap();
        t.draw(|f| g.draw(f)).unwrap();
        let rows: Vec<String> = t
            .backend()
            .buffer()
            .content()
            .chunks(90)
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect();
        let foot = rows.iter().position(|r| r.contains("██▀▀████▀▀██")).unwrap();
        let title = rows.iter().position(|r| r.contains("relay setup")).unwrap();
        assert_eq!(title, foot + 2, "{rows:#?}");
        // Its ink stands level with the title.
        assert_eq!(rows[title].find('r'), Some(2));
        assert_eq!(rows[foot - 4].trim_end(), "  █  █    ██    █  █");
        assert!(
            rows.iter().any(|r| r.contains("q later")),
            "the keys still fit: {rows:#?}"
        );
    }

    #[test]
    fn the_asking_names_what_would_stop() {
        let sessions = ImportReport {
            sessions: vec![
                session("infra-auth", Some("a1"), Some("done"), None),
                session("left-alone", Some("b2"), Some("done"), None),
            ],
            error: None,
        };
        let mut g = Guide::importing(&state(Config::default()), &sessions);
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Enter);
        let backend = ratatui::backend::TestBackend::new(90, 16);
        let mut t = ratatui::Terminal::new(backend).unwrap();
        t.draw(|f| g.draw(f)).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content()
            .chunks(90)
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect();
        assert!(text.contains("Stop 1 Claude session and move it here?"), "{text}");
        assert!(text.contains("infra-auth"), "{text}");
        assert!(!text.contains("left-alone"), "only what would stop: {text}");
        assert!(text.contains("esc back to the list"), "{text}");
    }

    /// An agent's hooks, as its built-in profile has them.
    fn h(agent: &str) -> UserHooks {
        Registry::builtin().user_hooks(agent).unwrap().clone()
    }

    /// Claude, Codex and Gemini found, their files under a fake home:
    /// claude's and codex's not there yet, gemini's with Relay's hooks in
    /// already.
    fn fake_home() -> (tempfile::TempDir, Vec<Hookable>) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let gemini = home.join(".gemini/settings.json");
        user_hooks::install(&gemini, &h("gemini"), Path::new("/x/rl")).unwrap();
        let env = |k: &str| (k == "HOME").then(|| home.clone().into_os_string());
        let found = hookable_in(&state(Config::default()).agents, &Registry::builtin(), env);
        (tmp, found)
    }

    #[test]
    fn the_hookable_agents_are_those_here_with_their_files() {
        let (tmp, found) = fake_home();
        assert_eq!(
            found,
            [
                Hookable {
                    name: "claude".into(),
                    file: tmp.path().join(".claude/settings.json"),
                    installed: Ok(false),
                    warning: None,
                    unset: vec![],
                    hooks: h("claude"),
                },
                Hookable {
                    name: "codex".into(),
                    file: tmp.path().join(".codex/hooks.json"),
                    installed: Ok(false),
                    warning: None,
                    unset: vec![],
                    hooks: h("codex"),
                },
                Hookable {
                    name: "gemini".into(),
                    file: tmp.path().join(".gemini/settings.json"),
                    installed: Ok(true),
                    warning: None,
                    unset: vec![],
                    hooks: h("gemini"),
                },
            ]
        );
        assert!(!tmp.path().join(".codex").exists(), "looking writes nothing");
        // One whose file does not read says so, and cannot be picked.
        std::fs::write(tmp.path().join(".gemini/settings.json"), "{ // mine\n}").unwrap();
        let home = tmp.path().to_path_buf();
        let env = |k: &str| (k == "HOME").then(|| home.clone().into_os_string());
        let again = hookable_in(&state(Config::default()).agents, &Registry::builtin(), env);
        assert!(again[2].installed.as_ref().unwrap_err().contains("left as it was"));
        // Codex's own config with hooks inline: said on the row.
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        std::fs::write(tmp.path().join(".codex/config.toml"), "[[hooks.Stop]]\n").unwrap();
        let again = hookable_in(&state(Config::default()).agents, &Registry::builtin(), env);
        assert!(
            again[1]
                .warning
                .as_deref()
                .unwrap()
                .contains("config.toml has hooks too")
        );
        let g = Guide::new(&state(Config::default()), &none(), again);
        let mut lines = Vec::new();
        g.draw_hooks(&mut lines);
        let said: String = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(said.contains("codex will warn at each start"), "{said}");
        // Gemini's own setting, not there yet: said on its row while it is on.
        std::fs::write(tmp.path().join(".gemini/settings.json"), "{}").unwrap();
        let again = hookable_in(&state(Config::default()).agents, &Registry::builtin(), env);
        assert_eq!(again[2].unset, ["ui.useAlternateBuffer"]);
        let mut g = Guide::new(&state(Config::default()), &none(), again);
        for p in &mut g.hooks {
            p.on = true;
        }
        let mut lines = Vec::new();
        g.draw_hooks(&mut lines);
        let said: String = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(said.contains("and sets ui.useAlternateBuffer"), "{said}");
    }

    #[test]
    fn the_hooks_call_this_rl_by_its_name_on_the_path_and_never_another() {
        let tmp = tempfile::tempdir().unwrap();
        let this = tmp.path().join("cellar/rl-1.2/rl");
        std::fs::create_dir_all(this.parent().unwrap()).unwrap();
        std::fs::write(&this, "").unwrap();
        // On PATH as a link to this very file: the link, which outlives an upgrade.
        let link = tmp.path().join("bin/rl");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&this, &link).unwrap();
        assert_eq!(pick_rl(Some(link.clone()), Some(this.clone())), Some(link));
        // Another `rl` on PATH (an older install): never it.
        let other = tmp.path().join("old/rl");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "").unwrap();
        assert_eq!(pick_rl(Some(other), Some(this.clone())), Some(this.clone()));
        assert_eq!(pick_rl(None, Some(this.clone())), Some(this));
        assert_eq!(pick_rl(None, Some("target/debug/rl".into())), None, "not a whole path");
        assert_eq!(pick_rl(None, None), None);
    }

    #[test]
    fn hooks_left_on_are_brought_up_to_date_and_said_only_when_they_change() {
        let (tmp, found) = fake_home();
        let mut g = Guide::new(&state(Config::default()), &none(), found);
        for _ in 0..8 {
            press(&mut g, KeyCode::Enter);
        }
        // Gemini's were in, pointing at an `rl` since moved.
        let hooking = g.hooking();
        let words = hook_them(&hooking, Path::new("/usr/local/bin/rl"));
        assert_eq!(
            words,
            format!(
                "gemini hooks in {} brought up to date",
                rl_core::display_path(&tmp.path().join(".gemini/settings.json"))
            )
        );
        let text = std::fs::read_to_string(tmp.path().join(".gemini/settings.json")).unwrap();
        assert!(text.contains("'/usr/local/bin/rl' hook gemini AfterTool || true") && !text.contains("/x/rl"));
        // Again: right already, nothing written, nothing said.
        assert_eq!(hook_them(&hooking, Path::new("/usr/local/bin/rl")), "");
    }

    #[test]
    fn a_hook_is_added_only_on_a_yes_and_taken_out_only_on_a_no() {
        let (_tmp, found) = fake_home();
        // Enter all the way through: what is there stays, nothing is added.
        let mut g = Guide::new(&state(Config::default()), &none(), found.clone());
        assert_eq!(g.steps.len(), 4);
        for _ in 0..8 {
            press(&mut g, KeyCode::Enter);
        }
        assert!(g.done);
        let h: Vec<(String, bool)> = g.hooking().into_iter().map(|(h, on)| (h.name, on)).collect();
        assert_eq!(
            h,
            [("gemini".to_string(), true)],
            "only what was there, to be kept up to date; codex is never added"
        );

        // Codex on, gemini off.
        let mut g = Guide::since(
            &SetupState {
                seen: 3,
                ..state(Config::default())
            },
            &none(),
            found.clone(),
        )
        .unwrap();
        assert_eq!(g.steps, [Step::Hooks]);
        press(&mut g, KeyCode::Down);
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Down);
        press(&mut g, KeyCode::Char(' '));
        assert!(g.hooking().is_empty(), "nothing until it is answered");
        press(&mut g, KeyCode::Enter);
        let h: Vec<(String, bool)> = g.hooking().into_iter().map(|(h, on)| (h.name, on)).collect();
        assert_eq!(h, [("codex".to_string(), true), ("gemini".to_string(), false)]);

        // Esc leaves both as they are, whatever was toggled.
        let mut g = Guide::since(
            &SetupState {
                seen: 3,
                ..state(Config::default())
            },
            &none(),
            found,
        )
        .unwrap();
        press(&mut g, KeyCode::Char(' '));
        press(&mut g, KeyCode::Esc);
        assert!(g.done && g.hooking().is_empty());
        // Seen it all, and nothing of anyone's hooks: nothing to ask.
        assert!(
            Guide::since(
                &SetupState {
                    seen: 3,
                    ..state(Config::default())
                },
                &none(),
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn hooking_writes_the_files_and_says_what_codex_asks() {
        let (tmp, found) = fake_home();
        let words = hook_them(
            &[
                (found[0].clone(), true),
                (found[1].clone(), true),
                (found[2].clone(), false),
            ],
            Path::new("/opt/rl"),
        );
        assert!(
            words.contains(
                "claude hooks added to {} · Claude picks it up in sessions started from now on; \
                 ones already open may ask you to review it in /hooks"
                    .replace("{}", &rl_core::display_path(&tmp.path().join(".claude/settings.json")))
                    .as_str()
            ),
            "{words}"
        );
        let claude = tmp.path().join(".claude/settings.json");
        assert!(
            std::fs::read_to_string(&claude)
                .unwrap()
                .contains("'/opt/rl' hook claude Notification --user || true")
        );
        assert!(
            words.contains("codex asks you to trust it once: open codex, /hooks"),
            "{words}"
        );
        assert!(words.contains("gemini hooks taken out of"), "{words}");
        let codex = tmp.path().join(".codex/hooks.json");
        assert!(user_hooks::installed(&codex, &h("codex")).unwrap());
        assert!(
            std::fs::read_to_string(&codex)
                .unwrap()
                .contains("'/opt/rl' hook codex PermissionRequest || true")
        );
        assert!(!user_hooks::installed(&tmp.path().join(".gemini/settings.json"), &h("gemini")).unwrap());
    }

    /// An agent added by its profile alone, `fake`: offered, and its hooks
    /// installed into the file its profile names, as its profile writes
    /// them. Its hooks telling `rld` of its sessions, and enter carrying one
    /// on, are `rl-daemon`'s to test (`tests/profile_agent.rs`).
    #[test]
    fn an_agent_added_by_its_profile_alone_is_offered_and_hooked() {
        let tmp = tempfile::tempdir().unwrap();
        let detect = tmp.path().join("detect");
        std::fs::create_dir_all(&detect).unwrap();
        std::fs::write(
            detect.join("fake.yaml"),
            "kind: agent\nbins: [fake]\nsessions:\n  hooks:\n    file: [\"$FAKE_HOME/hooks.json\", \"~/.fake/hooks.json\"]\n    \
             timeout: 750ms\n    unit: ms\n    after_install: \"fake asks nothing\"\n    events:\n      Hello: started\n      Done: idle\n",
        )
        .unwrap();
        let registry = Registry::load_from(&detect).unwrap();
        let mut agents = state(Config::default()).agents;
        agents.push(agent("fake", true, None));
        let home = tmp.path().to_path_buf();
        let env = |k: &str| (k == "HOME").then(|| home.clone().into_os_string());
        let found = hookable_in(&agents, &registry, env);
        let fake = found.iter().find(|h| h.name == "fake").expect("offered");
        assert_eq!(fake.file, tmp.path().join(".fake/hooks.json"));
        assert_eq!(fake.installed, Ok(false));
        let words = hook_them(&[(fake.clone(), true)], Path::new("/opt/rl"));
        assert!(
            words.contains("fake hooks added to") && words.contains("fake asks nothing"),
            "{words}"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".fake/hooks.json")).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"hooks": {
                "Hello": [{"hooks": [{"type": "command", "command": "'/opt/rl' hook fake Hello || true", "timeout": 750}]}],
                "Done": [{"hooks": [{"type": "command", "command": "'/opt/rl' hook fake Done || true", "timeout": 750}]}]
            }})
        );
        assert!(user_hooks::installed(&tmp.path().join(".fake/hooks.json"), &fake.hooks).unwrap());
        // Not on this machine: not offered.
        agents.last_mut().unwrap().bin = None;
        assert!(hookable_in(&agents, &registry, env).iter().all(|h| h.name != "fake"));
    }

    #[test]
    fn the_hooks_screen_names_each_file() {
        let (_tmp, found) = fake_home();
        let g = Guide::since(
            &SetupState {
                seen: 3,
                ..state(Config::default())
            },
            &none(),
            found,
        )
        .unwrap();
        let mut lines = Vec::new();
        g.draw_hooks(&mut lines);
        let said: String = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(said.contains("[ ] claude"), "{said}");
        assert!(
            said.contains("Show Claude sessions started outside Relay? This adds a hook to"),
            "{said}"
        );
        assert!(said.contains(".claude/settings.json"), "{said}");
        assert!(said.contains("[ ] codex"), "{said}");
        assert!(
            said.contains("Show Codex sessions started outside Relay? This adds a hook to"),
            "{said}"
        );
        assert!(said.contains(".codex/hooks.json"), "{said}");
        assert!(said.contains("[x] gemini"), "{said}");
    }
}
