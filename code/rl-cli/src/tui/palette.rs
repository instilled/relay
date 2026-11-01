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
    Import,
    Archive,
    Mark,
    Join,
    Resume,
    Shell,
    Rename,
    Pin,
    Hide,
    Sort,
    Pane,
    Events,
    Report,
    Reload,
    Flow,
    Edit,
    New,
    Summary,
    Since,
    Ask,
    Filter,
    Agents,
    Shells,
    Hidden,
    Prune,
    Up,
    Help,
    Quit,
}

pub struct Command {
    pub name: &'static str,
    /// What follows the name, for the hint; empty when nothing does.
    pub args: &'static str,
    /// The words `tab` offers for each argument in turn, when they are a
    /// fixed set; the hint in `args` says the same words.
    pub words: &'static [&'static [&'static str]],
    /// One short line: what it does, in plain words.
    pub what: &'static str,
    pub cmd: Cmd,
}

/// Every command, in the order the palette lists them. One name per act: an
/// act with a variant takes it as an argument rather than a second command,
/// and no word means two things (`done` is only ever what `/mark` says).
pub const ALL: &[Command] = &[
    Command {
        name: "stop",
        args: "",
        words: &[],
        what: "stop a run that is still going",
        cmd: Cmd::Stop,
    },
    Command {
        name: "archive",
        args: "[seen]",
        words: &[&["seen"]],
        what: "out of the panel, kept in the ledger: an ended run or a flow's, or all you have seen",
        cmd: Cmd::Archive,
    },
    Command {
        name: "mark",
        args: "done|blocked|working",
        words: &[&["done", "blocked", "working"]],
        what: "say what a run is really doing",
        cmd: Cmd::Mark,
    },
    Command {
        name: "import",
        args: "",
        words: &[],
        what: "move Claude's background sessions into Relay (claude --bg)",
        cmd: Cmd::Import,
    },
    Command {
        name: "join",
        args: "",
        words: &[],
        what: "take a run's terminal and its keyboard, as enter on it does",
        cmd: Cmd::Join,
    },
    Command {
        name: "resume",
        args: "[all [commands]]",
        words: &[&["all"], &["commands"]],
        what: "pick an ended run back up — its conversation, a fresh shell; all: what a restart took",
        cmd: Cmd::Resume,
    },
    Command {
        name: "shell",
        args: "",
        words: &[],
        what: "another shell in this folder",
        cmd: Cmd::Shell,
    },
    Command {
        name: "rename",
        args: "",
        words: &[],
        what: "rename what the cursor is on: a folder or a run",
        cmd: Cmd::Rename,
    },
    Command {
        name: "pin",
        args: "",
        words: &[],
        what: "keep a run at the top of the list, or let it go back",
        cmd: Cmd::Pin,
    },
    Command {
        name: "hide",
        args: "",
        words: &[],
        what: "take a run or a project off the list, never stopping it; with hidden on, bring it back",
        cmd: Cmd::Hide,
    },
    Command {
        name: "sort",
        args: "",
        words: &[],
        what: "put a project's rows back in the order they came, after ⇧↑↓ put them in yours",
        cmd: Cmd::Sort,
    },
    Command {
        name: "pane",
        args: "",
        words: &[],
        what: "show or hide the row's page, which takes the whole screen",
        cmd: Cmd::Pane,
    },
    Command {
        name: "events",
        args: "",
        words: &[],
        what: "every event, newest first, and what each set off; enter goes to its run or its flow",
        cmd: Cmd::Events,
    },
    Command {
        name: "report",
        args: "",
        words: &[],
        what: "the work and teams board: what needs you, what is live, what finished, by lane",
        cmd: Cmd::Report,
    },
    Command {
        name: "reload",
        args: "[flows|shell]",
        words: &[&["flows", "shell"]],
        what: "read ~/.relay/flows again, or the shell's aliases and functions; both when bare",
        cmd: Cmd::Reload,
    },
    Command {
        name: "flow",
        args: "run|pause|resume|approve|edit|new|test|draft|reload [name]",
        words: &[&[
            "run", "pause", "resume", "approve", "edit", "new", "test", "draft", "reload",
        ]],
        what: "a flow: the one named, else the one under the cursor · new proposes one from words, in the \
               pane, or writes the bare template · edit opens its flow.md, and says yes when it loads · test says what would fire · \
               draft brings back the flow proposal",
        cmd: Cmd::Flow,
    },
    Command {
        name: "edit",
        args: "[flow]",
        words: &[],
        what: "open a flow in $EDITOR (its flow.md, as /flow edit does) — the one named, else the one under the cursor",
        cmd: Cmd::Edit,
    },
    Command {
        name: "new",
        args: "",
        words: &[],
        what: "start a run by hand in the form",
        cmd: Cmd::New,
    },
    Command {
        name: "summary",
        args: "[deep] [again] [since]",
        words: &[&["deep", "again"], &["deep", "again"]],
        what: "say what happened here since you last looked: a run, a flow, a project (also from its overview), everything (the catch-up's top), a question's asker · deep reads the logs · again makes a new one",
        cmd: Cmd::Summary,
    },
    Command {
        name: "since",
        args: "[when]",
        words: &[],
        what: "what happened while you were away, or since when",
        cmd: Cmd::Since,
    },
    Command {
        name: "ask",
        args: "[words]",
        words: &[],
        what: "hand plain words to an assistant yourself; alone, its proposal again",
        cmd: Cmd::Ask,
    },
    Command {
        name: "filter",
        args: "[words in: is: kind: by:]",
        words: &[],
        what: "narrow the list as you type; esc on the list clears it",
        cmd: Cmd::Filter,
    },
    Command {
        name: "agents",
        args: "",
        words: &[],
        what: "show or hide agents' runs on the list (a on the list does too)",
        cmd: Cmd::Agents,
    },
    Command {
        name: "shells",
        args: "",
        words: &[],
        what: "show or hide shell runs on the list (s on the list does too)",
        cmd: Cmd::Shells,
    },
    Command {
        name: "hidden",
        args: "",
        words: &[],
        what: "show or hide what you hid, inline in its place (h on the list does too)",
        cmd: Cmd::Hidden,
    },
    Command {
        name: "prune",
        args: "",
        words: &[],
        what: "let the journals of old runs go; the runs and their events stay",
        cmd: Cmd::Prune,
    },
    Command {
        name: "up",
        args: "<path>",
        words: &[],
        what: "register a folder as a project, as `rl up` does",
        cmd: Cmd::Up,
    },
    Command {
        name: "help",
        args: "",
        words: &[],
        what: "list these commands, and the keys for where you are (^h does that too)",
        cmd: Cmd::Help,
    },
    Command {
        name: "quit",
        args: "",
        words: &[],
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

/// The keyboard shortcut for a command, when it has one, for the commands list
/// — so every key is discoverable there, not only in the footer. `^x` is one
/// key over whatever the cursor is on: it stops a run still going and archives
/// an ended one, so both `/stop` and `/archive` wear it.
pub fn key(cmd: Cmd) -> &'static str {
    match cmd {
        Cmd::Quit => "^c",
        Cmd::Stop | Cmd::Archive => "^x",
        Cmd::Rename => "^r",
        Cmd::Pin => "^t",
        Cmd::Pane => "^o",
        Cmd::Filter => "^f",
        _ => "",
    }
}

/// A finished line: the command it names, and whatever followed it.
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

/// The words a command takes for its argument at `at` (0 the first), when
/// they are a fixed set there; none otherwise.
pub fn arg_words(c: &Command, at: usize) -> &'static [&'static str] {
    c.words.get(at).copied().unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> &'static Command {
        ALL.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn each_command_offers_its_argument_words_in_turn() {
        assert_eq!(
            arg_words(cmd("flow"), 0),
            [
                "run", "pause", "resume", "approve", "edit", "new", "test", "draft", "reload"
            ]
        );
        assert!(arg_words(cmd("flow"), 1).is_empty(), "the flows' names are the panel's");
        assert_eq!(arg_words(cmd("reload"), 0), ["flows", "shell"]);
        assert_eq!(arg_words(cmd("archive"), 0), ["seen"]);
        assert_eq!(arg_words(cmd("resume"), 0), ["all"]);
        assert_eq!(arg_words(cmd("resume"), 1), ["commands"]);
        assert_eq!(arg_words(cmd("mark"), 0), ["done", "blocked", "working"]);
        assert_eq!(arg_words(cmd("summary"), 1), ["deep", "again"]);
        assert!(arg_words(cmd("agents"), 0).is_empty(), "a toggle takes nothing");
        assert!(arg_words(cmd("stop"), 0).is_empty(), "nothing follows /stop");
        assert!(arg_words(cmd("ask"), 0).is_empty(), "free words are not a set");
    }

    #[test]
    fn the_hint_says_the_words_tab_offers() {
        for c in ALL {
            for (i, set) in c.words.iter().enumerate() {
                for w in *set {
                    assert!(
                        c.args.contains(w),
                        "/{} offers {w} but its hint {:?} does not say it",
                        c.name,
                        c.args
                    );
                }
                assert!(!set.is_empty(), "/{} has an empty set at {i}", c.name);
            }
            if !c.words.is_empty() {
                assert!(!c.args.is_empty(), "/{} takes words but hints none", c.name);
            }
        }
    }

    fn names(typed: &str) -> Vec<&'static str> {
        matching(typed).iter().map(|c| c.name).collect()
    }

    #[test]
    fn an_exact_name_comes_first() {
        // `flow` is exact; nothing else opens from it.
        assert_eq!(names("flow"), vec!["flow"]);
        assert_eq!(names("FLOW"), vec!["flow"]);
        // `up` is a name, and sits inside nothing else.
        assert_eq!(names("up"), vec!["up"]);
    }

    #[test]
    fn a_prefix_comes_before_a_name_it_only_sits_inside() {
        // `ma` opens `mark` and sits inside `summary`.
        assert_eq!(names("ma"), vec!["mark", "summary"]);
        // `re` opens these four, alphabetically.
        assert_eq!(names("re"), vec!["reload", "rename", "report", "resume"]);
    }

    #[test]
    fn a_name_matches_by_what_it_contains_too() {
        assert_eq!(names("ll"), vec!["shell", "shells"]);
        // `um` opens nothing, and sits inside both of these.
        assert_eq!(names("um"), vec!["resume", "summary"]);
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
        assert_eq!(key(Cmd::Archive), "^x");
        assert_eq!(key(Cmd::Rename), "^r");
        assert_eq!(key(Cmd::Pin), "^t");
        assert_eq!(key(Cmd::Pane), "^o");
        assert_eq!(key(Cmd::Shell), "", "a shell is a command, not a key");
        assert_eq!(key(Cmd::Filter), "^f");
        assert_eq!(key(Cmd::Agents), "", "a toggle is the slash line or a letter, no ^key");
        assert_eq!(key(Cmd::Events), "", "^e is the line's: the end of it");
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
        let (c, rest) = parse("/hidden").unwrap();
        assert_eq!(c.cmd, Cmd::Hidden);
        assert!(rest.is_empty());
    }

    #[test]
    fn a_prefix_that_names_one_command_is_enough() {
        assert_eq!(parse("/sum").unwrap().0.cmd, Cmd::Summary);
        assert_eq!(parse("/q").unwrap().0.cmd, Cmd::Quit);
        assert_eq!(parse("/ren").unwrap().0.cmd, Cmd::Rename);
        assert_eq!(parse("/rel").unwrap().0.cmd, Cmd::Reload);
        assert_eq!(parse("/ev").unwrap().0.cmd, Cmd::Events);
        // `hide`/`hidden` share a prefix, so the full name is what names
        // them; it is an exact match, which wins.
        assert_eq!(parse("/flow run").unwrap().0.cmd, Cmd::Flow);
        assert_eq!(parse("/hide").unwrap().0.cmd, Cmd::Hide);
        assert_eq!(parse("/so").unwrap().0.cmd, Cmd::Sort);
        assert_eq!(parse("/he").unwrap().0.cmd, Cmd::Help);
        let (c, rest) = parse("/fi this").unwrap();
        assert_eq!(c.cmd, Cmd::Filter);
        assert_eq!(rest, "this");
    }

    #[test]
    fn an_ambiguous_or_unknown_name_is_nothing() {
        // `re` opens `reload` and `rename`; `p` opens three.
        assert!(parse("/re").is_none());
        assert!(parse("/p").is_none());
        // The toggles share a prefix with `hide` and `shell`.
        assert!(parse("/hi").is_none(), "hide and hidden");
        assert!(parse("/sh").is_none(), "shell and shells");
        assert!(parse("/done").is_none(), "gone: /archive, or /mark done");
        assert!(parse("/approve").is_none(), "gone: /flow approve");
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
