//! A run the assistant proposes, in the pane: what was asked, while it is asked — so a wait never
//! reads as a panel gone quiet — then what it proposed, until `enter` runs it, more words change
//! it, `^e` opens it as the run form, or `esc` lets it go. Then the panel's side
//! (`App::propose`, `App::proposed`, `App::go_proposal`): words for the assistant open it, each
//! answer lands in it, and words typed while it is up are asked again with the draft they change.

use super::{App, Mode, Pane, theme};
use crate::interpret::Draft;
use ratatui::text::{Line, Span};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// An `enter` sooner than this after the draft came was typed for what was there before it; an
/// `esc` that soon after any answer, for the wait.
pub(super) const SETTLE: Duration = Duration::from_millis(300);

/// What the dots say while the assistant is at it, one a second.
const DOTS: [&str; 3] = ["·  ", "·· ", "···"];

/// The words asked, and the run proposed for them.
pub struct Proposal {
    /// Where it was asked: where the run goes unless the draft says otherwise.
    pub folder: PathBuf,
    /// The first words, then each change asked for.
    pub asked: Vec<String>,
    /// What the assistant proposed last, who did, and when it came.
    pub draft: Option<(Draft, String, Instant)>,
    /// When it was last asked, while it is; none once it answered.
    pub since: Option<Instant>,
    /// When the last answer came, whatever it was.
    pub landed: Option<Instant>,
    /// Why the last ask, or the last `enter`, came to nothing.
    pub error: Option<String>,
    /// Whether it is the subpage now: `esc` leaves it, still asking, and `/ask` brings it back.
    pub shown: bool,
}

impl Proposal {
    pub fn new(folder: PathBuf, words: String) -> Self {
        Self {
            folder,
            asked: vec![words],
            draft: None,
            since: Some(Instant::now()),
            landed: None,
            error: None,
            shown: true,
        }
    }

    /// Whether an `enter` now would be one typed before the draft came.
    pub fn settling(&self, now: Instant) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|(_, _, at)| now.duration_since(*at) < SETTLE)
    }

    /// Whether an `esc` now would be one typed before the answer came.
    pub fn just_landed(&self, now: Instant) -> bool {
        self.landed.is_some_and(|at| now.duration_since(at) < SETTLE)
    }

    /// The page: the words asked, where it stands, what was proposed, the keys.
    pub fn lines(&self, place: &str, now: Instant) -> Vec<Line<'static>> {
        let mut v = Vec::new();
        for (i, w) in self.asked.iter().enumerate() {
            let k = if i == 0 { "asked" } else { "then" };
            v.push(fact(k, w.clone()));
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
                v.push(Line::from(Span::styled(format!("{lead}{l}"), theme::bad())));
            }
            v.push(Line::raw(""));
        }
        if let Some((d, agent, _)) = &self.draft {
            let style = if self.since.is_some() {
                theme::dim()
            } else {
                theme::name()
            };
            v.push(Line::from(vec![
                Span::styled(" proposed", style),
                Span::styled(format!("  by {agent}"), theme::dim()),
            ]));
            let place = match d.get("in") {
                "" => place.to_string(),
                p => p.to_string(),
            };
            v.push(fact("in", place));
            v.push(fact("run", d.get("run").to_string()));
            if !d.get("title").is_empty() {
                v.push(fact("title", d.get("title").to_string()));
            }
            v.push(Line::raw(""));
            if self.since.is_none() {
                v.push(Line::from(Span::styled(
                    " enter runs it · type to change it · ^e edits it · esc drops it",
                    theme::dim(),
                )));
            }
        } else if self.since.is_none() {
            v.push(Line::from(Span::styled(
                " type to ask again · esc drops it",
                theme::dim(),
            )));
        }
        v
    }
}

fn fact(k: &str, v: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!(" {k:<8} "), theme::dim()), Span::raw(v)])
}

impl App {
    /// The proposal, when it is the subpage up.
    pub(super) fn proposal_shown(&self) -> Option<&Proposal> {
        self.proposal
            .as_ref()
            .filter(|p| p.shown && self.show_pane && self.pane == Pane::Facts)
    }

    /// Words for the assistant, outside a flow: the pane opens on them at once. With a proposal
    /// up, they change it — asked again with the draft they change — rather than start another.
    pub(super) fn propose(&mut self, words: String) -> String {
        let refine = self.proposal_shown().is_some_and(|p| p.draft.is_some());
        let asked = match self.proposal.as_mut() {
            Some(p) if refine => {
                p.asked.push(words);
                p.since = Some(Instant::now());
                p.error = None;
                let (d, _, _) = p.draft.as_ref().expect("a draft to change");
                crate::interpret::changing(&p.asked, d)
            }
            _ => {
                self.proposal = Some(Proposal::new(self.folder_here(), words.clone()));
                words
            }
        };
        // One proposal up at a time: a flow's waits for `/flow draft`.
        if let Some(f) = self.flow_proposal.as_mut() {
            f.shown = false;
        }
        self.show_pane = true;
        self.pane = Pane::Facts;
        self.term = None;
        asked
    }

    /// What the assistant proposed, into the pane: said there if it is up, else in a word.
    pub(super) fn proposed(&mut self, d: Draft, agent: String) {
        let p = self
            .proposal
            .get_or_insert_with(|| Proposal::new(PathBuf::new(), String::new()));
        let now = Instant::now();
        p.draft = Some((d, agent.clone(), now));
        p.since = None;
        p.landed = Some(now);
        p.error = None;
        if !(p.shown && self.show_pane) {
            p.shown = false;
            self.say(format!("{agent} proposed a run · /ask shows it"));
        }
    }

    /// The ask came to nothing: said in the pane, where the wait was.
    pub(super) fn not_proposed(&mut self, why: String) {
        match self.proposal.as_mut() {
            Some(p) => {
                p.since = None;
                p.landed = Some(Instant::now());
                p.error = Some(why.clone());
                if !(p.shown && self.show_pane) {
                    self.say(why);
                }
            }
            None => self.say(why),
        }
    }

    /// An esc sooner than SETTLE after the shown proposal's answer came was typed for the wait.
    pub(super) fn proposal_just_landed(&self) -> bool {
        let now = Instant::now();
        self.flow_proposal_shown().is_some_and(|p| p.just_landed(now))
            || self.proposal_shown().is_some_and(|p| p.just_landed(now))
    }

    /// `/ask` with no words: the proposal, back in the pane.
    pub(super) fn show_proposal(&mut self) -> bool {
        let Some(p) = self.proposal.as_mut() else { return false };
        p.shown = true;
        if let Some(f) = self.flow_proposal.as_mut() {
            f.shown = false;
        }
        self.show_pane = true;
        self.pane = Pane::Facts;
        self.term = None;
        true
    }

    /// `esc` on the proposal: left while it is still asked — it comes back with `/ask` — and let
    /// go once it has answered.
    pub(super) fn leave_proposal(&mut self) {
        let Some(p) = self.proposal.as_mut() else { return };
        if p.since.is_some() {
            p.shown = false;
            self.say("still asking · /ask shows it");
        } else {
            self.proposal = None;
            self.say("dropped it");
        }
    }

    /// The form a proposal fills, where it would run.
    fn proposal_form(&self, p: &Proposal) -> Option<super::form::Form> {
        let (d, _, _) = p.draft.as_ref()?;
        Some(
            crate::interpret::form_of(d, &p.folder)
                .with_folders(&self.names.paths())
                .with_agents(&self.agents),
        )
    }

    /// `enter` on an empty line with the proposal up: run it. Why not, said in the pane.
    pub(super) async fn go_proposal(&mut self, area: ratatui::layout::Rect) {
        let Some(p) = self.proposal_shown() else { return };
        if p.since.is_some() {
            return self.say("still asking; a moment");
        }
        if p.settling(Instant::now()) {
            return;
        }
        let Some(form) = self.proposal_form(p) else {
            return self.say("nothing proposed yet · type to ask again");
        };
        let outcome = match super::form::run_draft(&form) {
            Ok(d) => self.start_run(d, area).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => self.proposal = None,
            Err(e) => {
                if let Some(p) = self.proposal.as_mut() {
                    p.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// `^e` with the proposal up: it, as the run form at the foot, to change field by field.
    pub(super) fn edit_proposal(&mut self) {
        let Some(p) = self.proposal_shown() else { return };
        if p.since.is_some() {
            return self.say("still asking; a moment");
        }
        let Some(form) = self.proposal_form(p) else {
            return self.say("nothing proposed yet · type to ask again");
        };
        self.proposal = None;
        self.leave_subpage();
        self.mode = Mode::Form(Box::new(form));
        self.say("enter runs it · esc does not");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpret::Kind;
    use std::collections::BTreeMap;

    fn draft(run: &str) -> Draft {
        Draft {
            kind: Kind::Run,
            fields: BTreeMap::from([("run".to_string(), run.to_string())]),
        }
    }

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn while_it_is_asked_the_page_says_so_and_for_how_long() {
        let p = Proposal::new("/w".into(), "run the tests".into());
        let later = p.since.unwrap() + Duration::from_secs(12);
        let page = text(&p.lines("relay", later));
        assert!(page.contains("asked    run the tests"), "{page}");
        assert!(page.contains("asking the assistant · 12s"), "{page}");
        assert!(!page.contains("enter runs it"), "nothing to run yet: {page}");
    }

    #[test]
    fn the_answer_shows_where_and_what_and_the_keys() {
        let mut p = Proposal::new("/w".into(), "run the tests".into());
        p.since = None;
        p.draft = Some((draft("cargo test"), "claude".into(), Instant::now()));
        let page = text(&p.lines("relay", Instant::now()));
        assert!(page.contains("proposed  by claude"), "{page}");
        assert!(page.contains("in       relay"), "where it was asked: {page}");
        assert!(page.contains("run      cargo test"), "{page}");
        assert!(page.contains("enter runs it · type to change it"), "{page}");
    }

    #[test]
    fn a_change_asked_keeps_the_draft_in_view_and_says_it_is_changing() {
        let mut p = Proposal::new("/w".into(), "run the tests".into());
        p.draft = Some((draft("cargo test"), "claude".into(), Instant::now()));
        p.asked.push("only the failing ones".into());
        let page = text(&p.lines("relay", p.since.unwrap()));
        assert!(page.contains("then     only the failing ones"), "{page}");
        assert!(page.contains("changing it"), "{page}");
        assert!(page.contains("run      cargo test"), "the last draft stays: {page}");
        assert!(!page.contains("enter runs it"), "{page}");
    }

    #[test]
    fn an_enter_right_as_it_lands_is_not_its_yes() {
        let mut p = Proposal::new("/w".into(), "x".into());
        let now = Instant::now();
        p.draft = Some((draft("ls"), "claude".into(), now));
        assert!(p.settling(now + Duration::from_millis(100)));
        assert!(!p.settling(now + SETTLE));
    }
}
