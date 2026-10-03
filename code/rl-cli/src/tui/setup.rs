//! The setup guide: a few screens, one question each, with an answer
//! already picked. Enter takes it, esc leaves that part as it is, ← goes
//! back. Moving sessions is the exception: nothing is picked for you and
//! enter asks once more, naming what it would stop, because that step acts
//! on something outside Relay and cannot be undone by going back. The first start shows it before the list, and again when a release
//! adds a step; `rl setup` shows all of it whenever you like. What it
//! settles goes into `config.yaml`, written by rld; how far it got goes into
//! the ledger, so the file stays yours.

use super::{Client, mark, theme};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rl_proto::config::Assistant;
use rl_proto::methods::{
    AgentInfo, ClaudeSession, ImportMove, ImportMoveParams, ImportReport, SETUP_STEPS, SetupDone, SetupSave, SetupState,
};
use std::collections::HashMap;

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
    it: ClaudeSession,
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
            "open in a terminal · close it there first".into()
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
    /// What each agent's `--version` said, as it comes in.
    versions: HashMap<String, String>,
    /// Who could summarise, given the agents picked.
    cursor: usize,
    summariser: Option<Assistant>,
    /// Whether each step was answered (enter) or left (esc), by step.
    answered: [bool; 3],
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
    /// The whole guide: what is set, and what could be moved over.
    pub fn new(state: &SetupState, sessions: &ImportReport) -> Self {
        let mut g = Self::of(state, sessions, vec![Step::Agents, Step::Summaries, Step::Sessions]);
        g.enter_step();
        g
    }

    /// `/import`, `rl import`: the sessions step on its own.
    pub fn importing(state: &SetupState, sessions: &ImportReport) -> Self {
        let mut g = Self::of(state, sessions, vec![Step::Sessions]);
        g.enter_step();
        g
    }

    /// The guide from a release's new steps on: what the last time through
    /// did not ask. Nothing new: `None`.
    pub fn since(state: &SetupState, sessions: &ImportReport) -> Option<Self> {
        let steps: Vec<Step> = [Step::Agents, Step::Summaries, Step::Sessions]
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i as u32 >= state.seen)
            .map(|(_, s)| s)
            .collect();
        let mut g = Self::of(state, sessions, steps);
        // A step with nothing to show is not worth a screen.
        g.steps.retain(|s| *s != Step::Sessions || !g.sessions.is_empty());
        if g.steps.is_empty() {
            return None;
        }
        g.enter_step();
        Some(g)
    }

    fn of(state: &SetupState, sessions: &ImportReport, steps: Vec<Step>) -> Self {
        let named = &state.config.config.agents;
        let agents = state
            .agents
            .iter()
            .map(|a| Pick {
                on: a.available() && (named.is_empty() || named.contains(&a.name)),
                info: a.clone(),
            })
            .collect();
        let current = state.config.config.assistants.summarise.clone();
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
            versions: HashMap::new(),
            cursor: 0,
            summariser: None,
            answered: [false; 3],
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
    pub fn moving(&self) -> Vec<ClaudeSession> {
        if !self.answered[2] {
            return vec![];
        }
        self.picked().into_iter().map(|s| s.it.clone()).collect()
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
        }
    }

    /// Where the cursor starts on a step: the first agent, or the one picked.
    fn enter_step(&mut self) {
        self.cursor = match self.at() {
            Step::Agents | Step::Sessions => 0,
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
        lines.push(Line::styled("Who writes summaries and drafts rules?", theme::name()));
        lines.push(Line::styled(
            "One short call each time you ask for a recap or type in plain words — never on its own.",
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
async fn move_them(terminal: &mut ratatui::DefaultTerminal, rpc: &mut Client, moving: &[ClaudeSession]) -> String {
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
                session_id: s.session_id.clone(),
                env,
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
                summarise: None,
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

    fn session(name: &str, id: Option<&str>, state: Option<&str>, run: Option<&str>) -> ClaudeSession {
        ClaudeSession {
            session_id: format!("s-{name}"),
            id: id.map(Into::into),
            name: name.into(),
            cwd: format!("/w/{name}").into(),
            kind: if id.is_some() { "background" } else { "interactive" }.into(),
            status: if state == Some("working") { "busy" } else { "idle" }.into(),
            state: state.map(Into::into),
            run: run.map(Into::into),
            new: run.is_none(),
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
        assert_eq!(g.sessions[2].said(), "open in a terminal · close it there first");
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
        let mut g = Guide::new(&state(Config::default()), &sessions);
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
        let g = Guide::since(&st, &sessions).expect("the sessions step is new");
        assert_eq!(g.steps, [Step::Sessions]);
        // Nothing of Claude's: nothing left to ask.
        assert!(Guide::since(&st, &none()).is_none());
        // Never gone through: all of it.
        let g = Guide::since(&state(Config::default()), &sessions).unwrap();
        assert_eq!(g.steps.len(), 3);
    }

    #[test]
    fn enter_twice_takes_the_defaults() {
        let mut g = Guide::new(&state(Config::default()), &none());
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
        let mut g = Guide::new(&state(Config::default()), &none());
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
        let mut g = Guide::new(&state(Config::default()), &none());
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
        c.assistants.summarise = Some(Assistant {
            agent: "codex".into(),
            model: Some("o3".into()),
            args: vec![],
        });
        let mut g = Guide::new(&state(c), &none());
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
        let mut g = Guide::new(&state(Config::default()), &none());
        press(&mut g, KeyCode::Enter);
        assert_eq!(g.step, 1);
        press(&mut g, KeyCode::Left);
        assert_eq!(g.step, 0);
        press(&mut g, KeyCode::Char('q'));
        assert!(g.quit && !g.done);
    }

    #[test]
    fn a_missing_agent_cannot_be_picked() {
        let mut g = Guide::new(&state(Config::default()), &none());
        g.cursor = 2;
        press(&mut g, KeyCode::Char(' '));
        assert!(!g.agents[2].on);
    }

    #[test]
    fn it_draws() {
        let g = Guide::new(&state(Config::default()), &none());
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
        let g = Guide::new(&state(Config::default()), &none());
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
}
