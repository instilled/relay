//! `Heuristics` as the registry's generic profile builds them.

use crate::{Profile, Registry};
use jiff::{SignedDuration, Timestamp};
use rl_core::state::{Confidence, RunKind, RunState};
use rl_session::detect::heuristics::Heuristics;

fn heur(profile: &Profile, kind: RunKind, now: Timestamp) -> Heuristics {
    Heuristics::new(profile.patterns.clone(), profile.timing, kind, now)
}

fn t(secs: f64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs_f64(secs)
}

fn shell_run(fg: Option<bool>, kind: RunKind, out: &[u8]) -> RunState {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), kind, t(0.0));
    if let Some(fg) = fg {
        h.set_foreground(fg);
    }
    h.output(out, t(1.0));
    h.tick(t(4.5)).unwrap().state
}

#[test]
fn shell_owning_foreground_is_idle_not_needs_input() {
    let s = shell_run(Some(true), RunKind::Shell, b"Continue? [y/N] ");
    assert_eq!(s, RunState::Idle);
}

#[test]
fn shell_with_child_in_foreground_can_still_need_input() {
    let s = shell_run(Some(false), RunKind::Shell, b"Password: ");
    assert_eq!(s, RunState::NeedsInput);
}

#[test]
fn non_shell_run_ignores_foreground() {
    let s = shell_run(Some(true), RunKind::Agent, b"Continue? [y/N] ");
    assert_eq!(s, RunState::NeedsInput);
}

#[test]
fn unknown_foreground_preserves_todays_behavior() {
    let s = shell_run(None, RunKind::Shell, b"Continue? [y/N] ");
    assert_eq!(s, RunState::NeedsInput);
}

#[test]
fn prompt_then_question_then_output() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Agent, t(0.0));
    assert_eq!(h.tick(t(1.0)), None, "nothing seen yet: no idle verdict");

    let s = h.output(b"\x1b[32mready\x1b[0m\n> ", t(1.0));
    assert_eq!((s.state, s.confidence), (RunState::Working, Confidence::Guess));
    assert_eq!(h.tick(t(2.0)), None, "not quiet long enough");
    let s = h.tick(t(4.5)).unwrap();
    assert_eq!((s.state, s.confidence), (RunState::Idle, Confidence::Likely));
    assert_eq!(s.detail.as_deref(), Some(">"));
    assert_eq!(h.tick(t(5.0)), None, "one verdict per quiet spell");

    h.output(b"Do you want to proceed? [y/N] ", t(6.0));
    let s = h.tick(t(9.5)).unwrap();
    assert_eq!((s.state, s.confidence), (RunState::NeedsInput, Confidence::Likely));
    assert_eq!(
        s.detail.as_deref(),
        Some("> Do you want to proceed? [y/N]"),
        "the whole line, as seen"
    );

    h.output(b"y\nlots of text with no prompt\n", t(10.0));
    let s = h.tick(t(14.0)).unwrap();
    assert_eq!((s.state, s.confidence), (RunState::Idle, Confidence::Guess));
    assert_eq!(h.last_line(80).as_deref(), Some("lots of text with no prompt"));
}

#[test]
fn prompt_marks_make_a_command_with_its_exit_and_how_long_it_took() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Shell, t(0.0));
    h.output(b"\x1b]133;A\x07$ ", t(1.0));
    h.output(b"\x1b]133;B\x07make test\n\x1b]133;C\x07", t(2.0));
    assert!(h.take_commands().is_empty(), "still running: nothing finished yet");
    h.output(b"all green\n", t(3.0));
    h.output(b"\x1b]133;D;0\x07", t(6.0));
    let done = h.take_commands();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].text, "make test", "what was typed, not what it printed");
    assert_eq!(done[0].exit, Some(0));
    assert_eq!(done[0].took, SignedDuration::from_secs(4), "C until D");
    assert!(h.take_commands().is_empty(), "draining: reported once");
    assert!(h.tail().contains("all green"), "the output is still the tail");
}

#[test]
fn a_whole_command_in_one_read_is_still_pulled_apart() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Shell, t(0.0));
    h.output(b"\x1b]133;B\x07echo hi\x1b]133;C\x07hi\n\x1b]133;D;1\x07", t(5.0));
    let done = h.take_commands();
    assert_eq!(done.len(), 1);
    assert_eq!((done[0].text.as_str(), done[0].exit), ("echo hi", Some(1)));
}

#[test]
fn what_the_line_editor_echoed_is_read_back_as_the_line_typed() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Shell, t(0.0));
    // Byte for byte what zsh echoes for `sleep 2`, taken from a real
    // session's journal: the first character, then a backspace and the
    // line over again, then `\r\r\n` before the command starts.
    h.output(b"\x1b]133;B\x07", t(1.0));
    h.output(b"s", t(1.0));
    h.output(b"\x08sleep ", t(1.0));
    h.output(b"2\x1b[?2004l\r\r\n\x1b]133;C\x07", t(2.0));
    h.output(b"\x1b]133;D;0\x07", t(6.0));
    let done = h.take_commands();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].text, "sleep 2", "neither `ssleep 2` nor nothing at all");
    assert_eq!(done[0].exit, Some(0));
    assert_eq!(done[0].took, SignedDuration::from_secs(4));
}

#[test]
fn a_shell_whose_prompt_says_nothing_yields_no_commands() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Shell, t(0.0));
    h.output(b"$ make test\nall green\n", t(1.0));
    assert!(h.take_commands().is_empty(), "nothing is invented without marks");
}

#[test]
fn output_resumes_an_escape_sequence_split_across_reads() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Agent, t(0.0));
    h.output(b"\x1b[", t(0.0));
    h.output(b"32mready\n", t(1.0));
    assert_eq!(h.tail(), "ready\n", "split escape must not leak into the tail");
}

#[test]
fn commands_do_not_idle_but_do_go_stale() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Command, t(0.0));
    let s = h.output(b"compiling...\n", t(1.0));
    assert_eq!(s.confidence, Confidence::Likely);
    assert_eq!(h.tick(t(100.0)), None, "a quiet command is still working");
    let s = h.tick(t(1.0 + 600.0)).unwrap();
    assert_eq!((s.state, s.confidence), (RunState::Stale, Confidence::Likely));
    assert_eq!(s.detail.as_deref(), Some("no output for 10m00s"));
    assert_eq!(h.tick(t(2000.0)), None, "stale reported once");
    h.output(b"done\n", t(2001.0));
    assert!(h.tick(t(2001.0 + 700.0)).is_some(), "silence after output counts again");
}

#[test]
fn decoration_under_a_line_is_read_through_but_history_is_not() {
    let reg = Registry::builtin();
    let mut h = heur(reg.generic(), RunKind::Agent, t(0.0));
    h.output("Do you want to proceed? [y/N]\n╰────────╯\n".as_bytes(), t(0.0));
    assert_eq!(
        h.tick(t(10.0)).unwrap().state,
        RunState::NeedsInput,
        "border under the question"
    );
    h.output("y\nOverwrite file? (y/n)\n│ >\n".as_bytes(), t(20.0));
    let s = h.tick(t(30.0)).unwrap();
    assert_eq!(
        (s.state, s.confidence),
        (RunState::Idle, Confidence::Likely),
        "a newer prompt wins"
    );
}
