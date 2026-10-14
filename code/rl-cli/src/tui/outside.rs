//! One of an agent's own sessions in words, for its row and the pane: whose
//! it is, where it lives, how it stands, and what enter does with it. Pure.
//!
//! These are the sessions an agent CLI runs that are not Relay's runs (the
//! daemon leaves those out): Claude's, as `claude agents` lists them, and
//! others' as their hooks tell. The agent's own words for how one stands are
//! kept as it writes them; one this does not know is said as it is.

use super::rows::short_path;
use super::theme;
use crate::rank;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::fmt_age;
use rl_proto::methods::Outside;
use rl_proto::types::RunState;

/// Whose it is, in words: its agent's name, capitalised — `Claude`,
/// `Codex`, `Gemini`.
pub fn agent_name(s: &Outside) -> String {
    let mut chars = s.agent.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Agent".into(),
    }
}

/// Whose it is and where it lives: in its agent's background, which
/// `claude attach` opens, or open in a terminal of someone's —
/// `Claude's · background`. Whether it can be attached to is its `id`'s to
/// say, not its `kind`'s. One a run carries on already, and that is at work
/// in the terminal it was left open in, is `still open in its terminal`.
/// One a Relay run was sent to (`/background`) says which, once that run
/// has ended: `Claude's · background · was <run title>`.
pub fn lives(s: &Outside) -> String {
    let whose = agent_name(s);
    if s.run.is_some() {
        format!("{whose}'s · still open in its terminal")
    } else if s.in_a_terminal() {
        format!("{whose}'s · in another terminal")
    } else if let Some(was) = &s.was {
        format!("{whose}'s · background · was {}", was.title)
    } else {
        format!("{whose}'s · background")
    }
}

/// A hidden one that has come to need you since it was hidden — asking,
/// blocked or failed after you said so — as `rows::comes_through` has it
/// of a run. It comes through to the list.
pub fn comes_through(s: &Outside, since: Timestamp) -> bool {
    s.wants_you() && s.state_at.is_some_and(|t| t > since)
}

/// Where it ranks among runs: as a run in its state would, except once it is
/// done — a run's finish is news until you have looked at it, and Claude's
/// own is not news Relay keeps, so it goes with the stopped, below what is
/// working.
pub fn score(s: &Outside) -> u8 {
    match s.run_state() {
        RunState::Finished => rank::state_score(RunState::Stopped),
        state => rank::state_score(state),
    }
}

/// Whether it waits on you: blocked in the background, or waiting in a
/// terminal.
pub fn needs_you(s: &Outside) -> bool {
    matches!(s.run_state(), RunState::NeedsInput | RunState::Blocked)
}

/// Its state in a word, for the state column: `working`, `needs you`,
/// `idle`, `done`, `failed`, `stopped` — or Claude's own word where this
/// does not know it.
pub fn word(s: &Outside) -> String {
    if needs_you(s) {
        return "needs you".into();
    }
    match s.state.as_deref() {
        Some("working") => "working".into(),
        Some(w @ ("done" | "failed" | "stopped")) => w.into(),
        Some(other) if !other.is_empty() => other.into(),
        _ => match s.status.as_str() {
            Outside::BUSY => "working".into(),
            other => other.into(),
        },
    }
}

/// What it waits on, while it needs you: `permission prompt`, `input
/// needed`, and the like.
pub fn waiting_for(s: &Outside) -> Option<&str> {
    s.waiting_for.as_deref().filter(|w| needs_you(s) && !w.is_empty())
}

/// Its state in words: the word, and what it waits on —
/// `needs you · permission prompt`.
pub fn state_words(s: &Outside) -> String {
    match waiting_for(s) {
        Some(w) => format!("{} · {w}", word(s)),
        None => word(s),
    }
}

/// Whether it is at work in another terminal: busy or waiting there, so
/// it is not carried on now.
fn busy_there(s: &Outside) -> bool {
    s.in_a_terminal() && s.at_work()
}

/// What enter does with it (`carry`): opens a background session of
/// Claude's here, carries one in another terminal on here, as its agent's
/// profile resumes one — or, while that one is at work there, why not now;
/// or goes to the run that carries it on already.
pub fn enter_words(s: &Outside) -> String {
    if s.run.is_some() {
        return "enter: go to the run that carries it on here".into();
    }
    if !s.in_a_terminal() {
        return match super::carry::attach_words(&s.agent) {
            Some(how) => format!("enter: open it here ({how})"),
            None => "enter: open it here".into(),
        };
    }
    if busy_there(s) {
        return busy_words(s).into();
    }
    match super::carry::resume_words(&s.agent) {
        Some(how) => format!("enter: carry it on here ({how})"),
        None => "enter: carry it on here".into(),
    }
}

/// Why one at work in another terminal is not carried on now, and what
/// would bring it: Claude's `/bg`, or waiting.
fn busy_words(s: &Outside) -> &'static str {
    if s.agent == "claude" {
        "busy in its terminal · /bg there brings it here"
    } else {
        "busy in its terminal · enter once it is idle"
    }
}

/// The same, for the footer: `enter opens it here`, `enter carries it on
/// here`, `busy in its terminal · …`, `enter goes to its run`.
pub fn enter_hint(s: &Outside) -> &'static str {
    if s.run.is_some() {
        "enter goes to its run"
    } else if !s.in_a_terminal() {
        "enter opens it here"
    } else if busy_there(s) {
        busy_words(s)
    } else {
        "enter carries it on here"
    }
}

/// Whether enter does it at once, as on a run, pane open or not: opening
/// one in the background here, or going to the run that carries it on, take
/// nothing from where it is. Carrying one on from its terminal moves it, so
/// enter shows its pane first, saying so.
pub fn enter_at_once(s: &Outside) -> bool {
    s.run.is_some() || !s.in_a_terminal()
}

/// What a search's `is:` takes it as, as a run in its state.
pub fn is_word(s: &Outside) -> &'static str {
    match s.run_state() {
        RunState::Failed => "failed",
        RunState::NeedsInput | RunState::Blocked => "needs",
        RunState::Finished => "done",
        RunState::Stopped => "stopped",
        RunState::Starting | RunState::Working | RunState::Stale | RunState::Idle => "going",
    }
}

/// The pane: its name, where it lives, how it stands, its folder, when it
/// started, its session id, and what enter does.
pub fn pane_lines(s: &Outside, now: Timestamp) -> Vec<Line<'static>> {
    let fact = |k: &str, v: String| Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v)]);
    let mut lines = vec![
        Line::from(Span::raw(format!(" {}", s.title())).bold()),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(state_words(s), theme::state(s.run_state())),
            Span::raw(format!("  {}", lives(s))).dim(),
        ]),
        Line::raw(""),
        fact("where", short_path(&s.cwd)),
    ];
    if let Some(t) = s.started_at {
        lines.push(fact("started", format!("{} ago", fmt_age(t, now))));
    }
    lines.push(fact("session", s.session_id.clone()));
    if let Some(id) = &s.id {
        lines.push(fact("id", id.clone()));
    }
    if let Some(pid) = s.pid {
        lines.push(fact("pid", pid.to_string()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::raw(format!(" {}", enter_words(s))).dim()));
    lines
}

#[cfg(test)]
/// A session as the digest carries it: background with `state`, else in
/// a terminal.
pub fn session(id: &str, state: Option<&str>, status: &str, waiting: Option<&str>) -> Outside {
    Outside {
        agent: "claude".into(),
        session_id: format!("sess-{id}"),
        id: state.map(|_| id.to_string()),
        name: format!("name {id}"),
        cwd: std::path::PathBuf::from("/w"),
        kind: if state.is_some() { "background" } else { "interactive" }.into(),
        status: status.into(),
        state: state.map(Into::into),
        pid: Some(42),
        started_at: Some("2026-09-10T11:00:00Z".parse().unwrap()),
        waiting_for: waiting.map(Into::into),
        run: None,
        new: false,
        workspace: None,
        state_at: None,
        was: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_says_how_it_stands_in_claudes_words() {
        let says = |s: &Outside| (state_words(s), s.run_state());
        // Background, by its state.
        assert_eq!(
            says(&session("a", Some("working"), "busy", None)),
            ("working".into(), RunState::Working)
        );
        assert_eq!(
            says(&session("a", Some("blocked"), "waiting", Some("permission prompt"))),
            ("needs you · permission prompt".into(), RunState::Blocked)
        );
        assert_eq!(
            says(&session("a", Some("blocked"), "", None)),
            ("needs you".into(), RunState::Blocked)
        );
        assert_eq!(
            says(&session("a", Some("done"), "", None)),
            ("done".into(), RunState::Finished)
        );
        assert_eq!(
            says(&session("a", Some("failed"), "", None)),
            ("failed".into(), RunState::Failed)
        );
        assert_eq!(
            says(&session("a", Some("stopped"), "", None)),
            ("stopped".into(), RunState::Stopped)
        );
        // In a terminal, by its status.
        assert_eq!(
            says(&session("i", None, "busy", None)),
            ("working".into(), RunState::Working)
        );
        assert_eq!(
            says(&session("i", None, "waiting", Some("input needed"))),
            ("needs you · input needed".into(), RunState::NeedsInput)
        );
        assert_eq!(says(&session("i", None, "idle", None)), ("idle".into(), RunState::Idle));
        // A word this does not know is said as Claude wrote it, and ranks
        // by its status, else as idle.
        assert_eq!(
            says(&session("a", Some("napping"), "busy", None)),
            ("napping".into(), RunState::Working)
        );
        assert_eq!(
            says(&session("i", None, "dozing", None)),
            ("dozing".into(), RunState::Idle)
        );
        assert_eq!(says(&session("i", None, "", None)), (String::new(), RunState::Idle));
        // What it waits on is said only while it does.
        assert_eq!(state_words(&session("i", None, "idle", Some("input needed"))), "idle");
    }

    #[test]
    fn a_session_says_where_it_lives_and_what_enter_does() {
        let bg = session("a", Some("working"), "busy", None);
        let term = session("i", None, "busy", None);
        assert_eq!(lives(&bg), "Claude's · background");
        assert_eq!(lives(&term), "Claude's · in another terminal");
        assert_eq!(enter_words(&bg), "enter: open it here (claude attach)");
        // At work there: not now, and what would bring it.
        assert_eq!(enter_words(&term), "busy in its terminal · /bg there brings it here");
        assert_eq!(enter_hint(&term), "busy in its terminal · /bg there brings it here");
        let idle = session("i", None, "idle", None);
        assert_eq!(enter_words(&idle), "enter: carry it on here (claude --resume)");
        assert_eq!(enter_hint(&idle), "enter carries it on here");
        assert_eq!(enter_hint(&bg), "enter opens it here");
        // Carried on here already, and at work in the terminal it was left
        // open in.
        let left = Outside {
            run: Some("r1".into()),
            ..term.clone()
        };
        assert_eq!(lives(&left), "Claude's · still open in its terminal");
        assert_eq!(enter_words(&left), "enter: go to the run that carries it on here");
        assert_eq!(enter_hint(&left), "enter goes to its run");
        // Its id says it can be attached to, whatever its kind says.
        let odd = Outside {
            kind: String::new(),
            ..bg.clone()
        };
        assert_eq!(lives(&odd), "Claude's · background");
        // A Relay run sent to the background: which, and attached to as any.
        let went = Outside {
            was: Some(rl_proto::methods::WentFrom {
                run: "r9".into(),
                title: "fix the build".into(),
            }),
            ..bg.clone()
        };
        assert_eq!(lives(&went), "Claude's · background · was fix the build");
        assert_eq!(enter_hint(&went), "enter opens it here");
        assert_eq!(bg.key(), "o:claude:sess-a");
        assert_eq!(bg.title(), "name a");
        let nameless = Outside {
            name: " ".into(),
            session_id: "0123456789abcdef".into(),
            ..bg.clone()
        };
        assert_eq!(nameless.title(), "session 01234567");
    }

    #[test]
    fn another_agents_session_is_said_as_its() {
        let cx = Outside {
            agent: "codex".into(),
            ..session("i", None, "busy", None)
        };
        assert_eq!(agent_name(&cx), "Codex");
        assert_eq!(lives(&cx), "Codex's · in another terminal");
        assert_eq!(cx.key(), "o:codex:sess-i");
        assert_eq!(enter_words(&cx), "busy in its terminal · enter once it is idle");
        let quiet = Outside {
            status: Outside::IDLE.into(),
            ..cx.clone()
        };
        assert_eq!(enter_words(&quiet), "enter: carry it on here (codex resume)");
        let gm = Outside {
            agent: "gemini".into(),
            ..quiet.clone()
        };
        assert_eq!(enter_words(&gm), "enter: carry it on here (gemini --resume)");
        let none = Outside {
            agent: String::new(),
            ..cx.clone()
        };
        assert_eq!(agent_name(&none), "Agent");
    }

    #[test]
    fn done_ranks_below_working() {
        let blocked = score(&session("a", Some("blocked"), "", None));
        let working = score(&session("a", Some("working"), "", None));
        let done = score(&session("a", Some("done"), "", None));
        assert!(blocked < working && working < done, "{blocked} {working} {done}");
    }

    #[test]
    fn the_pane_says_it_all() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let text = |s: &Outside| {
            pane_lines(s, now)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let bg = text(&session("a", Some("blocked"), "waiting", Some("permission prompt")));
        for want in [
            " name a",
            "needs you · permission prompt",
            "Claude's · background",
            "/w",
            "1h ago",
            "sess-a",
            "enter: open it here (claude attach)",
        ] {
            assert!(bg.contains(want), "{want:?} in\n{bg}");
        }
        let term = text(&session("i", None, "idle", None));
        assert!(term.contains("Claude's · in another terminal"), "{term}");
        assert!(term.contains("enter: carry it on here (claude --resume)"), "{term}");
    }
}
