//! `/` at the start of the command line: a command, run on the row under the
//! cursor or on the list, instead of a session to start. Each is a key's
//! action by name (`/pin` is `^t`), so what it does and what it refuses stay
//! the key's; typing one is its own confirmation, so `/stop` has no second
//! press.
//!
//! `tab` completes the command's name, then its argument where there are
//! only so many: a colour, or for `/rename` the name the row has now; for
//! `/project`, a folder.
//!
//! The table and the completion are pure; running a command is `impl App`.

use super::app::{App, Mode};
use super::remove::{self, Removal};
use crate::colour::PALETTE;
use crate::config::Order;
use crate::config::SHELL_DEFAULT;
use crate::rows::{self, Line, Names};
use std::path::{Path, PathBuf};

/// What a command takes after its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    Nothing,
    /// A palette name, `#rrggbb`, or `none`.
    Colour,
    /// A name; none, and the name is typed where it stands (`^r`).
    Name,
    /// Nothing, or one of these words.
    Words(&'static [&'static str]),
    /// `shell`, or an agent harness: by its name or a word that starts it.
    Harness,
    /// A folder: `~` is home, a relative one is under the folder the line
    /// starts in.
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Colour,
    Rename,
    Pin,
    Hide,
    Stop,
    Rm,
    Project,
    Agents,
    Shells,
    Hidden,
    Group,
    Default,
    Setup,
    Report,
    Help,
    Quit,
}

/// One command: its name (without the `/`), another it answers to, what it
/// takes, and what it does.
#[derive(Debug)]
pub struct Cmd {
    pub name: &'static str,
    pub alias: Option<&'static str>,
    pub arg: Arg,
    pub verb: Verb,
}

const fn cmd(name: &'static str, arg: Arg, verb: Verb) -> Cmd {
    Cmd {
        name,
        alias: None,
        arg,
        verb,
    }
}

/// Every command, in the order `tab` offers them.
pub const CMDS: [Cmd; 16] = [
    Cmd {
        name: "colour",
        alias: Some("color"),
        arg: Arg::Colour,
        verb: Verb::Colour,
    },
    cmd("rename", Arg::Name, Verb::Rename),
    cmd("pin", Arg::Nothing, Verb::Pin),
    cmd("hide", Arg::Nothing, Verb::Hide),
    cmd("stop", Arg::Nothing, Verb::Stop),
    cmd("rm", Arg::Nothing, Verb::Rm),
    cmd("project", Arg::Folder, Verb::Project),
    cmd("agents", Arg::Nothing, Verb::Agents),
    cmd("shells", Arg::Nothing, Verb::Shells),
    cmd("hidden", Arg::Nothing, Verb::Hidden),
    cmd("group", Arg::Nothing, Verb::Group),
    cmd("default", Arg::Harness, Verb::Default),
    cmd("setup", Arg::Nothing, Verb::Setup),
    cmd("report", Arg::Words(&["dismiss"]), Verb::Report),
    cmd("help", Arg::Nothing, Verb::Help),
    cmd("quit", Arg::Nothing, Verb::Quit),
];

/// The command `word` names, by its name or its alias.
pub fn lookup(word: &str) -> Option<&'static Cmd> {
    CMDS.iter().find(|c| c.name == word || c.alias == Some(word))
}

/// Whether the line is a command: it starts with `/`.
pub fn is_command(text: &str) -> bool {
    text.starts_with('/')
}

/// A command line's name (without the `/`) and the rest, trimmed.
pub fn split(text: &str) -> (&str, &str) {
    let t = text.strip_prefix('/').unwrap_or(text);
    match t.find(char::is_whitespace) {
        Some(i) => (&t[..i], t[i..].trim()),
        None => (t, ""),
    }
}

/// How a command line stands as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    /// A command, by its name or its alias.
    Known(&'static str),
    /// Not one yet, but the start of one: the ghost shows where it goes.
    Partial,
    Unknown,
}

pub fn typed(text: &str) -> Typed {
    let (word, _) = split(text);
    match lookup(word) {
        Some(c) => Typed::Known(c.name),
        None if !text.contains(char::is_whitespace) && CMDS.iter().any(|c| c.name.starts_with(word)) => Typed::Partial,
        None => Typed::Unknown,
    }
}

/// What the word being typed completes to, best first, and where it starts.
/// The name: the commands' names it starts. The argument — all of what
/// follows the name — a colour (by name, or `none`), for `/rename`
/// `name`: what the row is called now, for `/default` one of `defaults`
/// (the agent harnesses that are here, and `shell`), or for `/project` a
/// folder, under `folder` where it is relative.
pub fn candidates(
    text: &str,
    cur: usize,
    name: Option<&str>,
    defaults: &[String],
    folder: &Path,
) -> (usize, Vec<String>) {
    let head = &text[..cur];
    let Some(sp) = head.find(char::is_whitespace) else {
        let out = CMDS
            .iter()
            .map(|c| format!("/{}", c.name))
            .filter(|n| n.starts_with(head))
            .collect();
        return (0, out);
    };
    let start = sp + head[sp..].find(|c: char| !c.is_whitespace()).unwrap_or(head.len() - sp);
    let arg = &head[start..];
    let out = match lookup(&head[1..sp]).map(|c| c.arg) {
        Some(Arg::Colour) => {
            let mut all: Vec<&str> = PALETTE.iter().copied().chain(["none"]).collect();
            all.sort_unstable();
            all.into_iter()
                .filter(|c| c.starts_with(arg))
                .map(String::from)
                .collect()
        }
        Some(Arg::Name) => name
            .filter(|n| n.starts_with(arg) && *n != arg)
            .map(|n| vec![n.to_string()])
            .unwrap_or_default(),
        Some(Arg::Words(words)) => words
            .iter()
            .filter(|w| w.starts_with(arg) && **w != arg)
            .map(|w| w.to_string())
            .collect(),
        Some(Arg::Harness) => defaults.iter().filter(|d| d.starts_with(arg)).cloned().collect(),
        Some(Arg::Folder) => super::cmdline::paths(arg, folder, true),
        _ => vec![],
    };
    (start, out)
}

/// What `/default <arg>` sets `list.default` to: `shell`, or the agent
/// harness `arg` names — by a word that starts it on the line (`agy`), else
/// by its name (`cursor`). Anything else, and why not.
pub fn default_of(arg: &str, reg: &rl_detect::Registry) -> Result<String, String> {
    if arg == SHELL_DEFAULT {
        return Ok(arg.to_string());
    }
    let agents = || reg.agents().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
    if arg.is_empty() {
        return Err(format!(
            "/default takes {SHELL_DEFAULT} or an agent harness: {}",
            agents()
        ));
    }
    reg.by_word(arg)
        .or_else(|| reg.profile(arg))
        .filter(|p| p.kind == rl_core::state::RunKind::Agent && p.name != "generic")
        .map(|p| p.name.clone())
        .ok_or_else(|| {
            format!(
                "not an agent harness: {arg} · /default takes {SHELL_DEFAULT} or {}",
                agents()
            )
        })
}

/// How `/project` is used, in words.
const PROJECT: &str = "/project takes a folder — /project ~/code/shop — tab completes it";

/// The folder `/project <arg>` adds: `arg` (`~` is home; relative, under
/// `base`) as it is on disk, links resolved — and, inside a git checkout,
/// the checkout's project (`git::project`), as the sessions in it will
/// stand. One that is not there, or not a folder, and why not.
pub fn project_folder(arg: &str, base: &Path) -> Result<PathBuf, String> {
    if arg.is_empty() {
        return Err(PROJECT.into());
    }
    let p = super::cmdline::expand(arg);
    let p = if p.is_absolute() { p } else { base.join(p) };
    let real = std::fs::canonicalize(&p).map_err(|_| format!("no such folder: {arg}"))?;
    if !real.is_dir() {
        return Err(format!("not a folder: {arg}"));
    }
    Ok(crate::git::project(&real).unwrap_or(real))
}

/// What `/colour` takes, in words.
const COLOURS: &str = "a name — red, blue, green, yellow, purple, orange, pink, cyan — or #rrggbb, or none";

impl App {
    /// `enter` on a `/` line: the command run on the row under the cursor,
    /// or on the list. One that does not read — unknown, or not given what
    /// it takes — keeps the line, and the footer says why; one that does is
    /// cleared from the line and run, and what it came to is in the footer.
    pub(super) fn run_command(&mut self) {
        let text = self.cmd.line.text().trim().to_string();
        let (word, arg) = split(&text);
        let Some(c) = lookup(word) else {
            return self.say(format!("unknown command /{word} · ctrl+h shows the keys"));
        };
        match c.arg {
            Arg::Nothing if !arg.is_empty() => {
                return self.say(format!("/{} takes nothing after it", c.name));
            }
            Arg::Words(words) if !arg.is_empty() && !words.contains(&arg) => {
                return self.say(format!("/{} takes nothing, or {}", c.name, words.join(" or ")));
            }
            Arg::Colour if arg.is_empty() => return self.say(format!("/{} takes {COLOURS}", c.name)),
            Arg::Colour if arg != "none" && crate::colour::parse(arg).is_none() => {
                return self.say(format!("not a colour: {arg} ({COLOURS})"));
            }
            _ => {}
        }
        let default = match c.arg {
            Arg::Harness => match default_of(arg, crate::rows::registry()) {
                Ok(d) => Some(d),
                Err(why) => return self.say(why),
            },
            _ => None,
        };
        let folder = match c.arg {
            Arg::Folder => match project_folder(arg, &self.line_folder()) {
                Ok(f) => Some(f),
                Err(why) => return self.say(why),
            },
            _ => None,
        };
        let arg = arg.to_string();
        self.cmd.clear();
        let by = format!("/{}", c.name);
        match c.verb {
            Verb::Colour => match self.colour_target(&by) {
                Ok((key, harness)) => self.set_colour(key, harness, (arg != "none").then_some(arg)),
                Err(why) => self.say(why),
            },
            // No name: typed where it stands, as `^r` does.
            Verb::Rename if arg.is_empty() => self.rename_here(&by),
            Verb::Rename => match self.rename_target(&by) {
                Ok(t) => self.apply_name(t, &arg),
                Err(why) => self.say(why),
            },
            Verb::Pin => self.toggle_pin(&by),
            Verb::Hide => self.hide_here(),
            Verb::Stop => self.stop_here(),
            Verb::Rm => self.rm_here(),
            Verb::Project => self.add_project(folder.unwrap_or_default()),
            Verb::Agents => self.toggle_agents(),
            Verb::Shells => self.toggle_shells(),
            Verb::Hidden => self.toggle_hidden(),
            Verb::Group => self.toggle_grouping(),
            Verb::Default => self.set_default(default.unwrap_or_default()),
            Verb::Setup => self.setup_now(),
            Verb::Report if arg == "dismiss" => self.dismiss_drift(),
            Verb::Report => self.report_now(),
            Verb::Help => self.help = true,
            Verb::Quit => self.quit = true,
        }
    }

    /// `/default`: what a line naming no harness starts from now on, kept
    /// in `config.json`.
    fn set_default(&mut self, to: String) {
        let said = if to == SHELL_DEFAULT {
            "a typed line runs in a shell · claude/codex/agy and a prompt start an agent".to_string()
        } else {
            format!("a typed line is a prompt for {to} · ! runs it in a shell")
        };
        // Said first: a config.json that does not take it says so after.
        self.say(said);
        self.change_prefs(|p| p.default = to);
    }

    /// `/project`: `folder` a project on the list from now on, kept in
    /// `config.json`, its header there even with nothing in it; the cursor
    /// goes to it, so a line typed starts there. One that is a project
    /// already — added, named, or with sessions — is only gone to (and
    /// brought back, if you hid it). A folder you named that it is in is
    /// the project, as its sessions would stand there.
    fn add_project(&mut self, folder: PathBuf) {
        let (folder, name, known) = {
            let names = Names::new(&self.config.list.names);
            let folder = names.containing(&folder).unwrap_or(folder);
            let p = &self.config.list;
            let known = rows::added(p).any(|f| f == folder)
                || names.paths().contains(&folder)
                || self.rows.iter().any(|r| rows::project_of(r, &names) == folder);
            (folder.clone(), names.workspace(&folder), known)
        };
        let key = rows::head_key(&folder);
        let hidden = self.config.list.hidden.iter().any(|h| h.key == key);
        if !known || hidden {
            let to = folder.to_string_lossy().into_owned();
            self.change_prefs(|p| {
                if !known {
                    p.projects.push(to);
                }
                p.hidden.retain(|h| h.key != key);
            });
        }
        let said = match (known, hidden) {
            (false, _) => format!("added {name} · type to start an agent or a shell in it"),
            (true, true) => format!("{name} is a project already · brought it back"),
            (true, false) => format!("{name} is a project already"),
        };
        if self.keys().contains(&key) {
            self.select(Some(key));
            return self.say(said);
        }
        let why = match self.prefs().grouping {
            crate::config::Grouping::State => "ctrl+s groups by project to show it",
            crate::config::Grouping::Project => "the search leaves it out",
        };
        self.say(format!("{said} · {why}"));
    }

    /// A project you added, with nothing in it, off the list and out of
    /// `config.json`: its name, its place among the projects and its hide
    /// go with it.
    pub(super) fn forget_project(&mut self, key: String, folder: PathBuf, title: String) {
        let next = self.next_key();
        self.change_prefs(|p| {
            p.projects.retain(|f| Path::new(f) != folder);
            p.names.remove(&key);
            p.hidden.retain(|h| h.key != key);
            for o in p.order.iter_mut().filter(|o| o.group == Order::PROJECTS) {
                o.rows.retain(|k| *k != key);
            }
            p.order.retain(|o| o.group != key && !o.rows.is_empty());
        });
        self.say(format!("took {title} off the list · /project adds it again"));
        if !self.keys().contains(&key) {
            self.select(next);
            self.settle();
        }
    }

    /// `/hide`: the row or project under the cursor hidden, or brought back.
    fn hide_here(&mut self) {
        let plan = match self.selected() {
            Some(line) => remove::hide_plan(&line, &self.config.list),
            None => Err("nothing here to hide".into()),
        };
        match plan {
            Ok(p) => self.remove(p),
            Err(why) => self.say(why.replace("ctrl+x", "/hide")),
        }
    }

    /// `/stop`: a live session stopped at once, by Relay or its harness.
    fn stop_here(&mut self) {
        if !matches!(self.selected(), Some(Line::Session(_))) {
            return self.say("/stop stops a live session");
        }
        match self.removal() {
            Ok(p @ (Removal::Stop { .. } | Removal::StopNative { .. })) => self.remove(p),
            // The stop, at once: no question asked.
            Ok(Removal::Ask(a)) if a.stop => {
                let stop = a.first();
                self.remove(stop)
            }
            Ok(Removal::Ask(a)) => self.say(format!("{} has nothing running to stop · /rm removes it", a.title)),
            Ok(
                Removal::Hide { title, .. }
                | Removal::Unhide { title, .. }
                | Removal::Remove { title, .. }
                | Removal::RemoveNative { title, .. }
                | Removal::Forget { title, .. },
            ) => self.say(format!("{title} is not one the list can stop")),
            Err(why) => self.say(why),
        }
    }

    /// `/rm`: the session under the cursor deleted for good — `^x` twice's
    /// remove, asked about with the remove the only choice (`enter` takes
    /// it).
    fn rm_here(&mut self) {
        if !matches!(self.selected(), Some(Line::Session(_))) {
            return self.say("/rm removes the session under the cursor");
        }
        match self.removal() {
            Ok(Removal::Ask(a)) => self.ask(a.remove_only()),
            Ok(Removal::Stop { title, .. } | Removal::StopNative { title, .. }) => {
                self.say(format!("{title} can only be stopped from here · /stop"))
            }
            Ok(
                Removal::Hide { title, .. }
                | Removal::Unhide { title, .. }
                | Removal::Remove { title, .. }
                | Removal::RemoveNative { title, .. }
                | Removal::Forget { title, .. },
            ) => self.say(format!(
                "/rm removes a session Relay or its harness can remove · {title} is not one (/hide hides it)"
            )),
            Err(why) => self.say(why),
        }
    }

    /// The name the row under the cursor goes by, for `/rename`'s ghost.
    pub(super) fn name_here(&self) -> Option<String> {
        if !matches!(self.mode, Mode::List) {
            return None;
        }
        self.rename_target("").ok().map(|t| t.was)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_known_by_its_name_or_alias_and_a_start_of_one_is_partial() {
        assert_eq!(typed("/colour pink"), Typed::Known("colour"));
        assert_eq!(typed("/color"), Typed::Known("colour"));
        assert_eq!(typed("/rm"), Typed::Known("rm"));
        assert_eq!(typed("/col"), Typed::Partial);
        assert_eq!(typed("/"), Typed::Partial);
        assert_eq!(typed("/nope"), Typed::Unknown);
        assert_eq!(typed("/col pink"), Typed::Unknown);
        assert_eq!(split("/rename  build box "), ("rename", "build box"));
        assert!(is_command("/x") && !is_command("x/"));
    }

    #[test]
    fn the_name_completes_then_a_colour_or_the_rows_name() {
        let c = |t: &str, name| candidates(t, t.len(), name, &[], Path::new("/"));
        assert_eq!(
            c("/col", None),
            (0, vec!["/colour".to_string()]),
            "the alias is not offered"
        );
        assert_eq!(c("/h", None).1, ["/hide", "/hidden", "/help"]);
        assert_eq!(c("/se", None).1, ["/setup"]);
        assert_eq!(c("/rep", None).1, ["/report"]);
        assert_eq!(c("/report ", None), (8, vec!["dismiss".into()]));
        assert_eq!(c("/report d", None).1, ["dismiss"]);
        assert_eq!(typed("/report dismiss"), Typed::Known("report"));
        assert_eq!(typed("/setup"), Typed::Known("setup"));
        assert_eq!(c("/colour p", None), (8, vec!["pink".into(), "purple".into()]));
        assert_eq!(c("/color n", None).1, ["none"]);
        assert_eq!(c("/colour ", None).1.len(), PALETTE.len() + 1);
        assert_eq!(c("/rename ", Some("shop-sh")), (8, vec!["shop-sh".into()]));
        assert_eq!(c("/rename sh", Some("shop-sh")).1, ["shop-sh"]);
        assert!(c("/rename x", Some("shop-sh")).1.is_empty());
        assert!(c("/rename shop-sh", Some("shop-sh")).1.is_empty(), "done");
        assert!(c("/pin ", None).1.is_empty());
        assert!(c("/nope ", None).1.is_empty());
    }

    #[test]
    fn default_takes_shell_or_an_agent_harness_by_name_or_word() {
        let reg = rl_detect::Registry::builtin();
        assert_eq!(default_of("shell", &reg).as_deref(), Ok("shell"));
        assert_eq!(default_of("claude", &reg).as_deref(), Ok("claude"));
        assert_eq!(default_of("agy", &reg).as_deref(), Ok("antigravity"), "by its word");
        assert_eq!(default_of("cursor", &reg).as_deref(), Ok("cursor"), "by its name");
        for bad in ["sh", "generic", "nope", ""] {
            let e = default_of(bad, &reg).unwrap_err();
            assert!(
                e.contains("/default takes shell or") && e.contains("claude"),
                "{bad}: {e}"
            );
        }
        let found = ["claude".to_string(), "codex".into(), "shell".into()];
        assert_eq!(
            candidates("/default c", 10, None, &found, Path::new("/")),
            (9, vec!["claude".into(), "codex".into()])
        );
        assert_eq!(typed("/default codex"), Typed::Known("default"));
    }

    #[test]
    fn project_completes_folders_only_under_the_lines_folder_or_from_home() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(here.join("shop/src")).unwrap();
        std::fs::create_dir_all(here.join("shed")).unwrap();
        std::fs::write(here.join("shopping.txt"), "").unwrap();
        let c = |t: &str| candidates(t, t.len(), None, &[], &here);
        assert_eq!(c("/pro").1, ["/project"]);
        assert_eq!(c("/project sh"), (9, vec!["shed/".into(), "shop/".into()]), "no files");
        assert_eq!(c("/project shop/").1, ["shop/src/"]);
        let abs = format!("/project {}/sho", here.display());
        assert_eq!(c(&abs).1, [format!("{}/shop/", here.display())], "an absolute one");
        assert_eq!(c("/project ~").1, ["~/"]);
        assert_eq!(typed("/project ~/code"), Typed::Known("project"));
    }

    #[test]
    fn project_takes_a_folder_that_is_there_and_reads_it_as_its_checkouts_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("notes/old")).unwrap();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("repo/app/src")).unwrap();
        std::fs::write(root.join("file.txt"), "").unwrap();
        assert_eq!(project_folder("notes", &root), Ok(root.join("notes")), "under the base");
        assert_eq!(
            project_folder("notes/old/..", &root),
            Ok(root.join("notes")),
            "as on disk"
        );
        let abs = root.join("notes/old");
        assert_eq!(project_folder(abs.to_str().unwrap(), Path::new("/")), Ok(abs));
        assert_eq!(
            project_folder("repo/app/src", &root),
            Ok(root.join("repo")),
            "the checkout"
        );
        assert_eq!(project_folder("nope", &root), Err("no such folder: nope".into()));
        assert_eq!(project_folder("file.txt", &root), Err("not a folder: file.txt".into()));
        assert!(
            project_folder("", &root)
                .unwrap_err()
                .contains("/project takes a folder")
        );
    }
}
