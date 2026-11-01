//! A flow the assistant proposes, in the pane: a new one, or a change to one there. What was
//! asked, while it is asked, then the flow drafted — whole, or for a change every line against the
//! one it changes — until `enter` writes it and says yes, more words change it, `^e` opens it in
//! `$EDITOR`, or `esc` lets it go. Then the panel's side (`App::propose_flow`,
//! `App::save_flow_proposal`): words open it, each answer lands in it, and nothing is written
//! under `~/.relay/flows` but by the writers in `crate::flow`.

use super::new_flow::{Drafted, shown};
use super::{App, Asking, Pane, View, rows, theme};
use crate::context::Want;
use crate::flow::NewFile;
use crate::interpret::Answer;
use anyhow::Result;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::path::PathBuf;
use std::time::Instant;

/// What the dots say while the assistant is at it, one a second.
const DOTS: [&str; 3] = ["·  ", "·· ", "···"];

/// What the words are for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aim {
    /// A flow not written yet: named by the person, else by the assistant; `in` the folder it
    /// was asked from, unless the words say otherwise.
    New {
        named: Option<String>,
        folder: Option<PathBuf>,
    },
    /// A change to the flow `name`, whose `flow.md` was `base` when it was asked.
    // Asked for by nothing yet but the tests: the overview's words, which ask for it, come next.
    #[allow(dead_code)]
    Change { name: String, base: String },
}

/// The words asked, and the flow proposed for them.
pub struct FlowProposal {
    pub aim: Aim,
    /// The first words, then each change asked for.
    pub asked: Vec<String>,
    /// What the assistant proposed last, checked.
    pub draft: Option<Drafted>,
    /// When it was last asked, while it is; none once it answered.
    pub since: Option<Instant>,
    /// Why the last ask, or the last `enter`, came to nothing.
    pub error: Option<String>,
    /// Whether it is the subpage now: `esc` leaves it, still asking, and `/flow draft` brings it
    /// back.
    pub shown: bool,
}

impl FlowProposal {
    fn new(aim: Aim, asked: Vec<String>, shown: bool) -> Self {
        Self {
            aim,
            asked,
            draft: None,
            since: None,
            error: None,
            shown,
        }
    }

    /// The page over the draft: the words asked, where it stands, why the last try came to
    /// nothing. Every line as it is written, a character that does not print as its escape.
    pub fn head(&self, now: Instant) -> Vec<Line<'static>> {
        let mut v = Vec::new();
        for (i, w) in self.asked.iter().enumerate() {
            let k = if i == 0 { "asked" } else { "then" };
            v.push(Line::from(vec![
                Span::styled(format!(" {k:<8} "), theme::dim()),
                Span::raw(shown(w)),
            ]));
        }
        v.push(Line::raw(""));
        if let Some(at) = self.since {
            let secs = now.duration_since(at).as_secs();
            let dots = DOTS[(secs % DOTS.len() as u64) as usize];
            let what = if self.draft.is_some() {
                "changing it"
            } else {
                "asking the assistant"
            };
            v.push(Line::from(Span::styled(
                format!(" {dots} {what} · {secs}s"),
                theme::accent(),
            )));
            v.push(Line::from(Span::styled(
                "     its run is in the list · /stop there · esc leaves it asking",
                theme::dim(),
            )));
            v.push(Line::raw(""));
        }
        if let Some(e) = &self.error {
            for (i, l) in e.lines().enumerate() {
                let lead = if i == 0 { " ✗ " } else { "   " };
                v.push(Line::from(Span::styled(format!("{lead}{}", shown(l)), theme::bad())));
            }
            v.push(Line::raw(""));
        }
        if self.draft.is_none() && self.since.is_none() {
            v.push(Line::from(Span::styled(
                " type to ask again · esc drops it",
                theme::dim(),
            )));
        }
        v
    }

    /// The pane: the head, then the draft under it, scrolled and counted as one.
    pub fn draw(&self, f: &mut Frame, area: Rect, now: Instant) {
        let head = self.head(now);
        match &self.draft {
            Some(d) => d.draw(head, f, area),
            None => f.render_widget(Paragraph::new(head).wrap(Wrap { trim: false }), area),
        }
    }

    /// The footer's keys.
    pub fn hint(&self) -> String {
        match &self.draft {
            _ if self.since.is_some() => "esc leaves it asking".into(),
            Some(d) => d.hint(),
            None => "type to ask again · esc drops it".into(),
        }
    }
}

impl App {
    /// The flow proposal, when it is the subpage up.
    pub(super) fn flow_proposal_shown(&self) -> Option<&FlowProposal> {
        self.flow_proposal.as_ref().filter(|p| p.shown && self.show_pane)
    }

    /// Words for a flow — a new one, or a change to one there: the pane opens on them at once,
    /// and they are asked.
    pub(super) fn propose_flow(&mut self, aim: Aim, words: String) {
        if self.asking.is_some() {
            return self.say("still asking; /stop on its run");
        }
        let mut p = FlowProposal::new(aim, vec![words], true);
        p.since = Some(Instant::now());
        self.flow_proposal = Some(p);
        self.show_flow_proposal();
        self.ask_flow();
    }

    /// More words with the flow proposal up: asked again, with the text they change.
    pub(super) fn refine_flow(&mut self, words: String) {
        if words.trim().is_empty() {
            return;
        }
        if self.asking.is_some() {
            return self.say("still asking; /stop on its run");
        }
        let Some(p) = self.flow_proposal.as_mut() else { return };
        p.asked.push(words);
        p.since = Some(Instant::now());
        p.error = None;
        self.ask_flow();
    }

    /// The flow proposal, asked of the assistant.
    fn ask_flow(&mut self) {
        if let Some((sent, folder, want, words)) = self.flow_asking() {
            self.spawn_ask(sent, folder, want, words);
        }
    }

    /// What the flow proposal asks of the assistant — the words sent, where, for what, and the
    /// last words, for the footer. For a change, or once there is a draft, the words come with the
    /// text they change; else they are the words alone, every one asked, the first and each since.
    pub(super) fn flow_asking(&self) -> Option<(String, PathBuf, Want, String)> {
        let p = self.flow_proposal.as_ref()?;
        let prior = match (&p.draft, &p.aim) {
            (Some(d), _) => Some(d.text.as_str()),
            (None, Aim::Change { base, .. }) => Some(base.as_str()),
            (None, Aim::New { .. }) => None,
        };
        let sent = match prior {
            Some(text) => crate::interpret::changing_flow(&p.asked, text),
            None => p.asked.join("\n"),
        };
        let words = p.asked.last().cloned().unwrap_or_default();
        let (want, folder) = match &p.aim {
            Aim::New { named: Some(n), folder } => (Want::Flow { name: n.clone() }, folder.clone()),
            Aim::New { named: None, folder } => (Want::NewFlow { folder: folder.clone() }, folder.clone()),
            Aim::Change { name, .. } => (
                Want::Flow { name: name.clone() },
                self.flows
                    .iter()
                    .find(|f| &f.name == name)
                    .and_then(|f| f.folder.clone()),
            ),
        };
        let folder = folder.unwrap_or_else(|| self.folder_here());
        Some((sent, folder, want, words))
    }

    /// `sent` handed to the assistant in `folder`, for `want`; the panel stays live and the
    /// answer comes back through the main loop. `words` are what the footer says is asked.
    pub(super) fn spawn_ask(&mut self, sent: String, folder: PathBuf, want: Want, words: String) {
        let home = self.home.clone();
        let wanted = want.clone();
        let task = tokio::spawn(async move { crate::interpret::ask(&home, &folder, &sent, &wanted).await });
        self.asking = Some(Asking { words, want, task });
    }

    /// What the assistant answered words for a flow, into the pane: a draft, checked; or why
    /// not. Said in a word when the pane is not up — it never pops up, nor takes a key.
    pub(super) fn flow_answered(
        &mut self,
        want: Want,
        words: String,
        answer: std::result::Result<Result<(Answer, rl_proto::methods::AssistantOutput)>, tokio::task::JoinError>,
    ) {
        let up = self.show_pane;
        let home = self.home.clone();
        let known = self.names.paths();
        let p = self.flow_proposal.get_or_insert_with(|| {
            let aim = match want {
                Want::Flow { name } => Aim::New {
                    named: Some(name),
                    folder: None,
                },
                Want::NewFlow { folder } => Aim::New { named: None, folder },
                Want::Any => Aim::New {
                    named: None,
                    folder: None,
                },
            };
            FlowProposal::new(aim, vec![words], false)
        });
        p.since = None;
        let said = match answer {
            Ok(Ok((Answer::Flow(d), out))) => {
                let name = match &p.aim {
                    Aim::Change { name, .. } => Some(name.clone()),
                    Aim::New { named: Some(n), .. } => Some(n.clone()),
                    Aim::New { named: None, .. } => d.name,
                };
                let drafted = match &p.aim {
                    // A change is to a flow there: its name is not a new one's to check.
                    Aim::Change { base, .. } => {
                        Drafted::new(name, d.text, out.agent.clone(), &known, &|_| None).with_base(base.clone())
                    }
                    Aim::New { .. } => {
                        let taken = |n: &str| crate::flow::check_name(&home, n).err().map(|e| format!("{e:#}"));
                        Drafted::new(name, d.text, out.agent.clone(), &known, &taken)
                    }
                };
                let what = if drafted.name.is_empty() {
                    "a flow".to_string()
                } else {
                    shown(&drafted.name)
                };
                p.draft = Some(drafted);
                p.error = None;
                format!("{} proposed {what} · /flow draft shows it", out.agent)
            }
            Ok(Ok((Answer::Run(_), out))) => {
                let why = format!("asked for a flow; {} answered a run (run {})", out.agent, out.run);
                p.error = Some(why.clone());
                why
            }
            Ok(Err(e)) => {
                p.error = Some(format!("{e:#}"));
                format!("{e:#}")
            }
            Err(e) => {
                p.error = Some(format!("asking: {e}"));
                format!("asking: {e}")
            }
        };
        if !(p.shown && up) {
            p.shown = false;
            self.say(said);
        }
    }

    /// A flow answered where a run was asked: the run proposal becomes a flow proposal, its words
    /// and whether it was up carried over, `in` where it was asked.
    pub(super) fn run_proposal_to_flow(&mut self, words: &str) {
        let (asked, shown, folder) = match self.proposal.take() {
            Some(p) => (p.asked, p.shown, p.folder),
            None => (vec![words.to_string()], false, self.folder_here()),
        };
        let aim = Aim::New {
            named: None,
            folder: Some(folder),
        };
        self.flow_proposal = Some(FlowProposal::new(aim, asked, shown));
    }

    /// `/flow draft`: the flow proposal, back in the pane.
    pub(super) fn show_flow_proposal(&mut self) {
        let Some(p) = self.flow_proposal.as_mut() else {
            return self.say("no flow proposal · type on + new flow in a project, or /flow new <name> <words>");
        };
        p.shown = true;
        // One proposal up at a time: a run's waits for `/ask`.
        if let Some(r) = self.proposal.as_mut() {
            r.shown = false;
        }
        self.show_pane = true;
        self.pane = Pane::Facts;
        self.term = None;
    }

    /// `esc` on the flow proposal: left while it is still asked — it comes back with
    /// `/flow draft` — and let go once it has answered.
    pub(super) fn leave_flow_proposal(&mut self) {
        let Some(p) = self.flow_proposal.as_mut() else { return };
        if p.since.is_some() {
            p.shown = false;
            self.say("still asking · /flow draft shows it");
        } else {
            self.flow_proposal = None;
            self.say("dropped it · nothing written");
        }
    }

    /// The flow proposal let go, written or opened: the pane goes with it.
    fn close_flow_proposal(&mut self) {
        self.flow_proposal = None;
        self.show_pane = false;
        self.pane = Pane::Facts;
    }

    /// `enter` on an empty line with the flow proposal up: written and said yes to — a new flow
    /// by `flow::write_new`, a change by `flow::write_changed` — once it has been on screen a
    /// moment, would load, and its end has been. Why not, said; a write that fails stays in the
    /// pane.
    pub(super) async fn save_flow_proposal(&mut self) {
        let Some(p) = self.flow_proposal_shown() else { return };
        if p.since.is_some() {
            return self.say("still asking; a moment");
        }
        let Some(d) = &p.draft else {
            return self.say("nothing proposed yet · type to ask again");
        };
        // Typed for what was there before it came: not its yes, and nothing said.
        if d.settling(Instant::now()) {
            return;
        }
        if !d.ready() {
            return self.say(format!("it would not load: {} · ^e opens it in $EDITOR", d.problems[0]));
        }
        // The yes is to what was shown: none until its end has been.
        if !d.seen() {
            return self.say("the end is below · ↓ to read it");
        }
        let (name, text) = (d.name.clone(), d.text.clone());
        let known = self.names.paths();
        let written = match p.aim.clone() {
            Aim::New { .. } => match crate::flow::check_name(&self.home, &name) {
                Ok(new) => {
                    let files = vec![NewFile::flow_md(text)];
                    crate::flow::write_new(&mut self.rpc, &self.home, &new, files, &known)
                        .await
                        .map(|_| format!("{name} approved"))
                }
                Err(e) => Err(e),
            },
            Aim::Change { name, base } => {
                match crate::flow::write_changed(&mut self.rpc, &self.home, &name, &base, &text, &known).await {
                    // What did not load was put back as it was: the proposal stays to change.
                    Ok(e @ crate::flow::Edited::NotLoaded(_)) => Err(anyhow::anyhow!(e.words(&name))),
                    Ok(e) => Ok(e.words(&name)),
                    Err(e) => Err(e),
                }
            }
        };
        match written {
            Ok(said) => {
                self.close_flow_proposal();
                self.refresh().await;
                if self.view == View::Overview {
                    self.select(Some(rows::flow_key(&name)));
                }
                // Said last: a refresh that fails says so, and this is what matters.
                self.say(said);
            }
            Err(e) => {
                let why = format!("{e:#}");
                if let Some(p) = self.flow_proposal.as_mut() {
                    p.error = Some(why.clone());
                }
                self.say(why);
            }
        }
    }

    /// `^e` on an empty line with the flow proposal up: it, written and opened in `$EDITOR`, and
    /// said yes to on return as an edit is — a new flow's folder taken away again if the editor
    /// never opens, a change's `flow.md` put back as it was asked of.
    pub(super) fn edit_flow_proposal(&mut self) {
        let Some(p) = self.flow_proposal_shown() else { return };
        if p.since.is_some() {
            return self.say("still asking; a moment");
        }
        let Some(d) = &p.draft else {
            return self.say("nothing proposed yet · type to ask again");
        };
        let opened = match &p.aim {
            Aim::New { .. } => crate::flow::check_name(&self.home, &d.name).and_then(|name| {
                crate::flow::start_new(&self.home, &name, vec![NewFile::flow_md(d.text.clone())], true)
            }),
            Aim::Change { name, base } => crate::flow::start_changed(&self.home, name, base, &d.text),
        };
        let base = match &p.aim {
            Aim::Change { base, .. } => Some(base.clone()),
            Aim::New { .. } => None,
        };
        match opened {
            Ok(editing) => {
                self.editor = Some((editing, 1));
                self.editing_base = base;
                self.close_flow_proposal();
            }
            Err(e) => self.say(format!("{e:#}")),
        }
    }
}
