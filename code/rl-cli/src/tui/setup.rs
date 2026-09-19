//! The setup guide: a few screens, one question each, with an answer
//! already picked. Enter takes it, esc leaves that part as it is, ← goes
//! back. The first start shows it before the list, and again when a release
//! adds a step; `rl setup` shows all of it whenever you like. What it
//! settles goes into `config.yaml`, written by rld; how far it got goes into
//! the ledger, so the file stays yours.

use super::{Client, theme};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rl_proto::config::Assistant;
use rl_proto::methods::{AgentInfo, SETUP_STEPS, SetupDone, SetupSave, SetupState};
use std::collections::HashMap;

/// Who is asked first for summaries: quick and cheap, in this order.
const SUMMARISERS: &[&str] = &["claude", "codex", "gemini"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Agents,
    Summaries,
}

const STEPS: [Step; 2] = [Step::Agents, Step::Summaries];

/// An agent on the first screen.
#[derive(Debug, Clone)]
struct Pick {
    info: AgentInfo,
    on: bool,
}

/// What the guide holds while it is open.
#[derive(Debug)]
pub struct Guide {
    step: usize,
    agents: Vec<Pick>,
    /// What each agent's `--version` said, as it comes in.
    versions: HashMap<String, String>,
    /// Who could summarise, given the agents picked.
    cursor: usize,
    summariser: Option<Assistant>,
    /// Whether each step was answered (enter) or left (esc).
    answered: [bool; 2],
    /// What the config said, to write only what changed.
    was_agents: Vec<String>,
    was_summariser: Option<Assistant>,
    done: bool,
    quit: bool,
}

/// What leaving the guide came to.
pub enum Outcome {
    /// Saved; the words say what changed.
    Saved(String),
    /// Left with ^c: nothing written, asked again next start.
    Later,
}

impl Guide {
    pub fn new(state: &SetupState) -> Self {
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
        let mut g = Guide {
            step: 0,
            agents,
            versions: HashMap::new(),
            cursor: 0,
            summariser: None,
            answered: [false; 2],
            was_agents: named.clone(),
            was_summariser: current.clone(),
            done: false,
            quit: false,
        };
        g.summariser = current.or_else(|| g.summarisers().into_iter().next());
        g
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
        match STEPS[self.step] {
            Step::Agents => self.agents.len(),
            Step::Summaries => self.summarisers().len(),
        }
    }

    /// Where the cursor starts on a step: the first agent, or the one picked.
    fn enter_step(&mut self) {
        self.cursor = match STEPS[self.step] {
            Step::Agents => 0,
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
        self.answered[self.step] = answered;
        if self.step + 1 == STEPS.len() {
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
                if self.step > 0 {
                    self.step -= 1;
                    self.enter_step();
                }
            }
            KeyCode::Char(' ') => self.choose(),
            KeyCode::Enter => {
                if STEPS[self.step] == Step::Summaries {
                    self.choose();
                }
                self.next(true);
            }
            KeyCode::Esc => self.next(false),
            _ => {}
        }
    }

    /// Space: an agent in or out; a summariser picked.
    fn choose(&mut self) {
        match STEPS[self.step] {
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
        if self.answered[0] {
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
        if self.answered[1] && self.summariser != self.was_summariser {
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
                Span::styled(format!("   {} of {}", self.step + 1, STEPS.len()), theme::dim()),
            ]),
            Line::raw(""),
        ];
        match STEPS[self.step] {
            Step::Agents => self.draw_agents(&mut lines),
            Step::Summaries => self.draw_summaries(&mut lines),
        }
        lines.push(Line::raw(""));
        let keys = match STEPS[self.step] {
            Step::Agents => "space in or out · enter next · esc leave as it is · ← back · q later",
            Step::Summaries => "↑↓ pick · enter save · esc leave as it is · ← back · q later",
        };
        lines.push(Line::styled(keys, theme::dim()));
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

/// The guide on the screen until it is saved or left.
pub async fn run(terminal: &mut ratatui::DefaultTerminal, rpc: &mut Client, state: SetupState) -> Result<Outcome> {
    let mut guide = Guide::new(&state);
    let mut versions = ask_versions(&state.agents);
    let mut input = EventStream::new();
    loop {
        terminal.draw(|f| guide.draw(f))?;
        if guide.quit {
            return Ok(Outcome::Later);
        }
        if guide.done {
            let save = guide.save();
            let words = said(&save);
            rpc.call::<SetupDone>(save).await?;
            return Ok(Outcome::Saved(words));
        }
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => guide.key(k),
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
        "setup done, nothing changed · rl setup to go through it again".into()
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

    fn press(g: &mut Guide, code: KeyCode) {
        g.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn enter_twice_takes_the_defaults() {
        let mut g = Guide::new(&state(Config::default()));
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
        let mut g = Guide::new(&state(Config::default()));
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
        let mut g = Guide::new(&state(Config::default()));
        press(&mut g, KeyCode::Char(' '));
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
        let mut g = Guide::new(&state(c));
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
        let mut g = Guide::new(&state(Config::default()));
        press(&mut g, KeyCode::Enter);
        assert_eq!(g.step, 1);
        press(&mut g, KeyCode::Left);
        assert_eq!(g.step, 0);
        press(&mut g, KeyCode::Char('q'));
        assert!(g.quit && !g.done);
    }

    #[test]
    fn a_missing_agent_cannot_be_picked() {
        let mut g = Guide::new(&state(Config::default()));
        g.cursor = 2;
        press(&mut g, KeyCode::Char(' '));
        assert!(!g.agents[2].on);
    }

    #[test]
    fn it_draws() {
        let g = Guide::new(&state(Config::default()));
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
        assert!(text.contains("relay setup   1 of 2"), "{text}");
        assert!(text.contains("[x] claude"), "{text}");
        assert!(text.contains("[ ] cursor     not found"), "{text}");
    }
}
