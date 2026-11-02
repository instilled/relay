//! Removing a project from the list (`^x` twice on its header), in words:
//! what the first press says a second will do, and what came of it. Apart
//! from `App`, which needs a daemon, so that what the user is told can be
//! tested on its own.

use rl_proto::methods::{DigestItem, WorkspaceRemoved};
use std::path::Path;

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

/// The flash once it is done: `removed relay · stopped 2 · 7 runs archived`.
/// Only what happened is said: a folder the daemon did not have registered
/// was not a project.
pub fn done(name: &str, r: &WorkspaceRemoved) -> String {
    let first = if r.removed {
        format!("removed {name}")
    } else {
        format!("{name} was not a project")
    };
    let mut words = vec![first];
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
    if !r.removed && words.len() == 1 {
        words.push("nothing removed".into());
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
            cwd: None,
            recap: None,
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
    fn the_result_says_only_what_happened() {
        let r = WorkspaceRemoved {
            path: "/w/relay".into(),
            removed: true,
            stopped: 2,
            archived: 7,
            ..Default::default()
        };
        assert_eq!(done("relay", &r), "removed relay · stopped 2 · 7 runs archived");
        let more = WorkspaceRemoved {
            dismissed: 1,
            archived: 1,
            ..r.clone()
        };
        assert_eq!(
            done("relay", &more),
            "removed relay · stopped 2 · 1 run archived · 1 question dismissed"
        );
        let stubborn = WorkspaceRemoved {
            path: "/w/relay".into(),
            stopped: 1,
            still_running: 1,
            ..Default::default()
        };
        assert_eq!(
            done("relay", &stubborn),
            "relay was not a project · stopped 1 · 1 still going"
        );
        assert_eq!(
            done("relay", &WorkspaceRemoved::default()),
            "relay was not a project · nothing removed"
        );
        let bare = WorkspaceRemoved {
            removed: true,
            ..Default::default()
        };
        assert_eq!(done("relay", &bare), "removed relay");
    }
}
