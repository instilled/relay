//! The `/` commands. Typing is the panel's resting state, so the letters go
//! into the line at the foot and everything a letter used to do is a command
//! here: a name, what may follow it, and one line saying what it does.
//!
//! This is the table and the matching over it, nothing else. The panel maps
//! a `Cmd` onto the handler it already has.

/// What a `/` command does. The panel maps this onto what it already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Stop,
    Done,
    DoneAll,
    Mark,
    Watch,
    Join,
    Info,
    Full,
    Rules,
    Reload,
    Reindex,
    Approve,
    Pause,
    New,
    Edit,
    Editor,
    Summarise,
    Ask,
    Filter,
    LensAll,
    LensAgents,
    LensJobs,
    LensReactions,
    Gc,
    Help,
    Quit,
}

pub struct Command {
    pub name: &'static str,
    /// What follows the name, for the hint; empty when nothing does.
    pub args: &'static str,
    /// One short line: what it does, in plain words.
    pub what: &'static str,
    pub cmd: Cmd,
}

/// Every command, in the order the palette lists them.
pub const ALL: &[Command] = &[
    Command {
        name: "stop",
        args: "",
        what: "stop a run that is still going",
        cmd: Cmd::Stop,
    },
    Command {
        name: "done",
        args: "",
        what: "be done with an ended run: out of the panel, kept in the ledger",
        cmd: Cmd::Done,
    },
    Command {
        name: "done-all",
        args: "",
        what: "be done with everything you have seen",
        cmd: Cmd::DoneAll,
    },
    Command {
        name: "mark",
        args: "done|blocked|working",
        what: "say what a run is really doing",
        cmd: Cmd::Mark,
    },
    Command {
        name: "watch",
        args: "",
        what: "watch a run's terminal without taking the keyboard",
        cmd: Cmd::Watch,
    },
    Command {
        name: "join",
        args: "",
        what: "join a run's terminal and take the keyboard",
        cmd: Cmd::Join,
    },
    Command {
        name: "info",
        args: "",
        what: "show or hide the pane of facts beside the list",
        cmd: Cmd::Info,
    },
    Command {
        name: "full",
        args: "",
        what: "give the pane the whole screen",
        cmd: Cmd::Full,
    },
    Command {
        name: "rules",
        args: "",
        what: "go to the rules of this folder",
        cmd: Cmd::Rules,
    },
    Command {
        name: "reload",
        args: "",
        what: "re-read the rule files from disk",
        cmd: Cmd::Reload,
    },
    Command {
        name: "reindex",
        args: "",
        what: "re-read the shell's aliases and functions",
        cmd: Cmd::Reindex,
    },
    Command {
        name: "approve",
        args: "",
        what: "approve the selected rule, now that it has changed",
        cmd: Cmd::Approve,
    },
    Command {
        name: "pause",
        args: "",
        what: "pause the selected rule, or let it run again",
        cmd: Cmd::Pause,
    },
    Command {
        name: "new",
        args: "",
        what: "write a rule by hand in the form",
        cmd: Cmd::New,
    },
    Command {
        name: "edit",
        args: "",
        what: "edit the selected rule in the form",
        cmd: Cmd::Edit,
    },
    Command {
        name: "editor",
        args: "",
        what: "open the selected rule's file in $EDITOR",
        cmd: Cmd::Editor,
    },
    Command {
        name: "summarise",
        args: "",
        what: "ask an assistant what an ended run did",
        cmd: Cmd::Summarise,
    },
    Command {
        name: "ask",
        args: "<words>",
        what: "hand plain words to an assistant yourself",
        cmd: Cmd::Ask,
    },
    Command {
        name: "filter",
        args: "<text>",
        what: "narrow the list to what matches",
        cmd: Cmd::Filter,
    },
    Command {
        name: "all",
        args: "",
        what: "show the whole list (the lens: also tab)",
        cmd: Cmd::LensAll,
    },
    Command {
        name: "agents",
        args: "",
        what: "the lens: only agent runs",
        cmd: Cmd::LensAgents,
    },
    Command {
        name: "jobs",
        args: "",
        what: "the lens: only what a schedule started",
        cmd: Cmd::LensJobs,
    },
    Command {
        name: "reactions",
        args: "",
        what: "the lens: only what a rule fired",
        cmd: Cmd::LensReactions,
    },
    Command {
        name: "gc",
        args: "",
        what: "let the journals of old runs go",
        cmd: Cmd::Gc,
    },
    Command {
        name: "help",
        args: "",
        what: "list these commands",
        cmd: Cmd::Help,
    },
    Command {
        name: "quit",
        args: "",
        what: "leave the panel",
        cmd: Cmd::Quit,
    },
];

/// The commands whose name matches what has been typed after the `/`; all of
/// them when nothing has been typed yet. Best first.
pub fn matching(typed: &str) -> Vec<&'static Command> {
    let typed = typed.trim().to_ascii_lowercase();
    if typed.is_empty() {
        return ALL.iter().collect();
    }
    let mut out: Vec<&'static Command> = ALL
        .iter()
        .filter(|c| c.name.to_ascii_lowercase().contains(&typed))
        .collect();
    // The name typed in full comes first; then the names it opens, then the
    // names it merely sits inside; alphabetical within each.
    out.sort_by_key(|c| {
        let name = c.name.to_ascii_lowercase();
        let rank = if name == typed {
            0
        } else if name.starts_with(&typed) {
            1
        } else {
            2
        };
        (rank, name)
    });
    out
}

/// A finished line: the command it names, and whatever followed it.
/// The keyboard shortcut for a command, when it has one, for the commands list
/// — so `^x` and `^c` are discoverable there, not only in the footer. `^x` is
/// one key over whatever the cursor is on: it stops a run still going and is
/// done with an ended one, so both `/stop` and `/done` wear it.
pub fn key(cmd: Cmd) -> &'static str {
    match cmd {
        Cmd::Quit => "^c",
        Cmd::Stop | Cmd::Done => "^x",
        _ => "",
    }
}

pub fn parse(line: &str) -> Option<(&'static Command, String)> {
    let line = line.trim();
    let line = line.strip_prefix('/').unwrap_or(line).trim_start();
    let mut words = line.splitn(2, char::is_whitespace);
    let name = words.next().unwrap_or_default().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let rest = words.next().unwrap_or_default().trim().to_string();
    if let Some(c) = ALL.iter().find(|c| c.name == name) {
        return Some((c, rest));
    }
    // A prefix will do, so long as it names one command and no other.
    let mut opens = ALL.iter().filter(|c| c.name.starts_with(&name));
    let one = opens.next()?;
    match opens.next() {
        Some(_) => None,
        None => Some((one, rest)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(typed: &str) -> Vec<&'static str> {
        matching(typed).iter().map(|c| c.name).collect()
    }

    #[test]
    fn an_exact_name_beats_a_prefix() {
        assert_eq!(names("done"), vec!["done", "done-all"]);
        assert_eq!(names("DONE"), vec!["done", "done-all"]);
    }

    #[test]
    fn a_prefix_comes_before_a_name_it_only_sits_inside() {
        // Only `rules` carries `rule` at all.
        assert_eq!(names("rule"), vec!["rules"]);
        // `pa` opens `pause`, and `done-all` has no `pa`.
        assert_eq!(names("pa"), vec!["pause"]);
    }

    #[test]
    fn a_name_matches_by_what_it_contains_too() {
        // Nothing starts with `ll`; three names carry it, alphabetically.
        assert_eq!(names("ll"), vec!["all", "done-all", "full"]);
        // `it` opens nothing, and sits inside both of these.
        assert_eq!(names("it"), vec!["edit", "editor", "quit"]);
    }

    #[test]
    fn nothing_typed_lists_every_command_in_order() {
        let all = matching("");
        assert_eq!(all.len(), ALL.len());
        assert_eq!(all[0].name, "stop");
        assert_eq!(all.last().unwrap().name, "quit");
        let listed: Vec<&str> = all.iter().map(|c| c.name).collect();
        let table: Vec<&str> = ALL.iter().map(|c| c.name).collect();
        assert_eq!(listed, table);
    }

    #[test]
    fn nothing_matching_is_an_empty_list() {
        assert!(matching("zzz").is_empty());
    }

    #[test]
    fn a_command_shows_its_key_shortcut_when_it_has_one() {
        assert_eq!(key(Cmd::Quit), "^c");
        assert_eq!(key(Cmd::Stop), "^x");
        assert_eq!(key(Cmd::Done), "^x");
        assert_eq!(key(Cmd::Watch), "", "most commands are the slash line, no key");
    }

    #[test]
    fn a_line_reads_with_the_slash_or_without_it() {
        let (c, rest) = parse("/filter build").unwrap();
        assert_eq!(c.cmd, Cmd::Filter);
        assert_eq!(rest, "build");
        let (c, rest) = parse("filter build").unwrap();
        assert_eq!(c.cmd, Cmd::Filter);
        assert_eq!(rest, "build");
        let (c, rest) = parse("  /QUIT  ").unwrap();
        assert_eq!(c.cmd, Cmd::Quit);
        assert!(rest.is_empty());
    }

    #[test]
    fn what_followed_the_name_comes_back_whole() {
        let (c, rest) = parse("/ask why did the build fail?").unwrap();
        assert_eq!(c.cmd, Cmd::Ask);
        assert_eq!(rest, "why did the build fail?");
        let (c, rest) = parse("/mark   blocked  ").unwrap();
        assert_eq!(c.cmd, Cmd::Mark);
        assert_eq!(rest, "blocked");
    }

    #[test]
    fn a_prefix_that_names_one_command_is_enough() {
        assert_eq!(parse("/sum").unwrap().0.cmd, Cmd::Summarise);
        assert_eq!(parse("/q").unwrap().0.cmd, Cmd::Quit);
        let (c, rest) = parse("/fi this").unwrap();
        assert_eq!(c.cmd, Cmd::Filter);
        assert_eq!(rest, "this");
        // `f` opens both `full` and `filter`, so it names neither.
        assert!(parse("/f").is_none());
    }

    #[test]
    fn an_exact_name_wins_over_the_longer_one_it_opens() {
        assert_eq!(parse("/done").unwrap().0.cmd, Cmd::Done);
        assert_eq!(parse("/done-all").unwrap().0.cmd, Cmd::DoneAll);
    }

    #[test]
    fn an_ambiguous_or_unknown_name_is_nothing() {
        assert!(parse("/d").is_none());
        assert!(parse("/e").is_none());
        assert!(parse("/nope").is_none());
        assert!(parse("/").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn every_command_says_what_it_does_and_says_its_name_once() {
        let mut seen = std::collections::HashSet::new();
        for c in ALL {
            assert!(!c.what.is_empty(), "{} says nothing", c.name);
            assert!(!c.name.is_empty());
            assert_eq!(c.name, c.name.to_ascii_lowercase(), "{} is not lowercase", c.name);
            assert!(seen.insert(c.name), "{} is listed twice", c.name);
        }
        assert_eq!(seen.len(), ALL.len());
    }
}
