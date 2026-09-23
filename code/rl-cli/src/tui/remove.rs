//! Removing a project from the list (`^x` twice on its header), in words:
//! what the first press says a second will do, the question about the
//! folder's `.relay`, and what came of it. Apart from `App`, which needs a
//! daemon, so that what the user is told can be tested on its own.

use rl_core::rules_dir::RulesDir;
use rl_proto::methods::{DigestItem, WorkspaceRemoved};
use std::path::{Path, PathBuf};

/// What removing a folder would take with it, as the list has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub name: String,
    /// Runs there still going: every one is stopped.
    pub live: usize,
    /// Runs there that have ended and are still on the list: archived.
    pub ended: usize,
}

impl Plan {
    /// Counted from the digest: the runs whose folder is `folder` itself (a
    /// folder nested inside has a header of its own).
    pub fn of(name: &str, folder: &Path, items: &[DigestItem]) -> Self {
        let mine = items.iter().filter(|i| i.run.workspace == folder);
        let (mut live, mut ended) = (0, 0);
        for i in mine {
            if i.run.state.is_terminal() {
                ended += 1;
            } else {
                live += 1;
            }
        }
        Self {
            name: name.to_string(),
            live,
            ended,
        }
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The armed hint's words, after "press ^x again to":
/// `remove relay · stops 2 · archives 7 ended runs`.
pub fn arm(p: &Plan) -> String {
    let mut words = vec![format!("remove {}", p.name)];
    if p.live > 0 {
        words.push(format!("stops {}", p.live));
    }
    if p.ended > 0 {
        words.push(format!("archives {}", count(p.ended, "ended run", "ended runs")));
    }
    words.join(" · ")
}

/// What is in a `.relay`: the rules loaded from it, or — when none are, as
/// in a folder never registered — the files that hold them.
pub fn contents(schedules: usize, reactions: usize, files: &[PathBuf]) -> String {
    let mut words = Vec::new();
    if schedules > 0 {
        words.push(count(schedules, "schedule", "schedules"));
    }
    if reactions > 0 {
        words.push(count(reactions, "reaction", "reactions"));
    }
    if words.is_empty() {
        words = files
            .iter()
            .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
    }
    words.join(" · ")
}

/// Asked on the second `^x`, before anything is done, when the folder has a
/// `.relay` that can be moved. `removed` is where it would go, as shown.
pub fn question(name: &str, contents: &str, removed: &str) -> String {
    format!("also move {name}/.relay ({contents}) to {removed}? y moves · n keeps · any other key cancels")
}

/// What becomes of a folder's `.relay` in a removal, for the result's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesFate {
    /// There is none worth a word.
    Nothing,
    /// Asked, and the answer was to keep it.
    Kept,
    /// Not movable, so not asked: kept, and why.
    KeptBecause(&'static str),
    /// Asked, and the answer was to move it.
    Moved,
}

impl RulesFate {
    /// The fate of a `.relay` nobody is asked about: one there is nothing
    /// to move out of, or one that may not be moved.
    pub fn unasked(dir: &RulesDir) -> Self {
        match dir {
            RulesDir::Symlink => RulesFate::KeptBecause("a symlink"),
            RulesDir::Home => RulesFate::KeptBecause("it is the Relay home"),
            RulesDir::Nothing | RulesDir::Movable(_) => RulesFate::Nothing,
        }
    }
}

/// The flash once it is done: `removed relay · stopped 2 · 7 runs archived ·
/// .relay kept`. Only what happened is said. `show` renders a path for humans.
pub fn done(name: &str, r: &WorkspaceRemoved, fate: &RulesFate, show: impl Fn(&Path) -> String) -> String {
    let mut words = vec![format!("removed {name}")];
    if r.stopped > 0 {
        words.push(format!("stopped {}", r.stopped));
    }
    if r.still_running > 0 {
        words.push(format!("{} still going", r.still_running));
    }
    if r.archived > 0 {
        words.push(format!("{} archived", count(r.archived, "run", "runs")));
    }
    if r.dismissed > 0 {
        words.push(format!("{} dismissed", count(r.dismissed, "question", "questions")));
    }
    match (fate, &r.rules_moved_to) {
        (_, Some(to)) => words.push(format!(".relay moved to {}", show(to))),
        (RulesFate::Kept, None) => words.push(".relay kept".into()),
        (RulesFate::KeptBecause(why), None) => words.push(format!(".relay kept: {why}")),
        // Asked to move and nothing moved: it held no rules by then.
        (RulesFate::Moved, None) | (RulesFate::Nothing, None) => {}
    }
    words.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::types::{Run, RunKind, RunState};

    fn item(id: &str, ws: &str, state: RunState) -> DigestItem {
        let mut run = Run::new(id, RunKind::Command, id, vec![], ws, ws, Timestamp::UNIX_EPOCH);
        run.state = state;
        DigestItem {
            run,
            at: Timestamp::UNIX_EPOCH,
            summary: String::new(),
            unseen: false,
            joined_by: None,
            emitted: Vec::new(),
        }
    }

    #[test]
    fn the_first_press_says_what_the_second_will_take() {
        let items = [
            item("a", "/w/relay", RunState::Working),
            item("b", "/w/relay", RunState::Idle),
            item("c", "/w/relay", RunState::Finished),
            item("d", "/w/relay", RunState::Failed),
            item("e", "/w/relay/inner", RunState::Working),
            item("f", "/w/other", RunState::Stopped),
        ];
        let p = Plan::of("relay", Path::new("/w/relay"), &items);
        assert_eq!(p.live, 2);
        assert_eq!(p.ended, 2);
        assert_eq!(arm(&p), "remove relay · stops 2 · archives 2 ended runs");

        let one = Plan {
            name: "x".into(),
            live: 0,
            ended: 1,
        };
        assert_eq!(arm(&one), "remove x · archives 1 ended run");
        let idle = Plan {
            name: "x".into(),
            live: 0,
            ended: 0,
        };
        assert_eq!(arm(&idle), "remove x");
    }

    #[test]
    fn the_relay_question_names_what_would_move_and_where() {
        let files = [PathBuf::from("/w/relay/.relay/schedules.yaml")];
        assert_eq!(contents(2, 1, &files), "2 schedules · 1 reaction");
        assert_eq!(contents(0, 3, &files), "3 reactions");
        // Nothing loaded from it (a folder never registered): its files.
        assert_eq!(contents(0, 0, &files), "schedules.yaml");
        assert_eq!(
            question("relay", "2 schedules · 1 reaction", "~/.relay/removed"),
            "also move relay/.relay (2 schedules · 1 reaction) to ~/.relay/removed? y moves · n keeps · any other key cancels"
        );
    }

    #[test]
    fn the_result_says_only_what_happened() {
        let show = |p: &Path| p.display().to_string();
        let r = WorkspaceRemoved {
            path: "/w/relay".into(),
            removed: true,
            stopped: 2,
            archived: 7,
            ..Default::default()
        };
        assert_eq!(
            done("relay", &r, &RulesFate::Kept, show),
            "removed relay · stopped 2 · 7 runs archived · .relay kept"
        );
        let moved = WorkspaceRemoved {
            rules_moved_to: Some("/h/removed/relay-20260923-101500".into()),
            dismissed: 1,
            archived: 1,
            ..r.clone()
        };
        assert_eq!(
            done("relay", &moved, &RulesFate::Moved, show),
            "removed relay · stopped 2 · 1 run archived · 1 question dismissed · .relay moved to /h/removed/relay-20260923-101500"
        );
        let stubborn = WorkspaceRemoved {
            path: "/w/relay".into(),
            stopped: 1,
            still_running: 1,
            ..Default::default()
        };
        assert_eq!(
            done("relay", &stubborn, &RulesFate::unasked(&RulesDir::Symlink), show),
            "removed relay · stopped 1 · 1 still going · .relay kept: a symlink"
        );
        assert_eq!(
            done(
                "relay",
                &WorkspaceRemoved::default(),
                &RulesFate::unasked(&RulesDir::Nothing),
                show
            ),
            "removed relay"
        );
        assert_eq!(
            RulesFate::unasked(&RulesDir::Home),
            RulesFate::KeptBecause("it is the Relay home")
        );
    }
}
