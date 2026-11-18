//! The list's command line: what is typed at the foot of the list starts a
//! session. A first word a harness claims (its profile's `words`: `claude`,
//! `codex`, `agy`…) starts that agent with the rest of the line as its first
//! prompt; any other line starts the default (`list.default`): a shell
//! command, run in a shell Relay holds that stays once the command is done,
//! or — where the default is an agent harness — that agent, the whole line its
//! prompt. A line that starts with `!` is a shell command whatever the
//! default; one that starts with `/` is a command instead (`slash`).
//!
//! A line may have several lines (`shift+enter`, or a paste): an agent gets
//! all of it as its prompt, a shell runs them in order.
//!
//! `tab` completes the word under the cursor: the first word to a harness's
//! word or a program on `PATH`, a later one to a path under the folder the
//! line starts in. The best completion stands after the cursor, dim, until
//! `tab` takes it.
//!
//! Pure but for reading folders: what the line names, and what it completes
//! to.

use super::{line, slash};
use rl_core::state::RunKind;
use rl_detect::Registry;
use rl_detect::registry::Profile;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The programs on `PATH`, by name: read once, for as long as the list is
/// open.
#[derive(Debug, Default, Clone)]
pub struct Commands {
    names: BTreeSet<String>,
}

impl Commands {
    /// The programs in `dirs`: files anyone may run.
    pub fn scan(dirs: &[PathBuf]) -> Commands {
        let mut names = BTreeSet::new();
        for d in dirs {
            let Ok(entries) = std::fs::read_dir(d) else {
                continue;
            };
            for e in entries.flatten() {
                let runs =
                    std::fs::metadata(e.path()).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
                if runs && let Some(n) = e.file_name().to_str() {
                    names.insert(n.to_string());
                }
            }
        }
        Commands { names }
    }

    /// The programs on this process's `PATH`.
    pub fn from_env() -> Commands {
        let _scan = crate::trace::span("commands.scan");
        let dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Commands::scan(&dirs)
    }

    /// Whether `bin` is here to run: on `PATH`, or an absolute path to a
    /// file.
    pub fn has(&self, bin: &str) -> bool {
        if Path::new(bin).is_absolute() {
            return Path::new(bin).is_file();
        }
        self.names.contains(bin)
    }

    fn starting<'a>(&'a self, frag: &'a str) -> impl Iterator<Item = &'a str> {
        self.names
            .range(frag.to_string()..)
            .take_while(move |n| n.starts_with(frag))
            .map(String::as_str)
    }
}

/// What a line starts.
#[derive(Debug, Clone, Copy)]
pub enum Detected<'a> {
    /// The harness its first word names — or, where it names none, the
    /// default (`named` false: the whole line is the prompt) — and the binary
    /// it lacks when none of its own is on `PATH`.
    Agent {
        profile: &'a Profile,
        missing: Option<&'a str>,
        named: bool,
    },
    /// A command, run in a shell.
    Shell,
    /// A `/` command, by its name.
    Command(&'static str),
    /// A `/` line that names no command and starts none.
    UnknownCommand,
}

impl Detected<'_> {
    /// What the right end of the line says.
    pub fn label(&self) -> String {
        match self {
            Detected::Agent {
                profile, missing: None, ..
            } => format!("agent · {}", profile.name),
            Detected::Agent {
                profile,
                missing: Some(bin),
                ..
            } => format!("agent · {} ({bin} not found)", profile.name),
            Detected::Shell => "shell".into(),
            Detected::Command(name) => format!("command · {name}"),
            Detected::UnknownCommand => "unknown command".into(),
        }
    }
}

/// The line's first word, and the rest of it, trimmed.
pub fn split_first(text: &str) -> (&str, &str) {
    let t = text.trim_start();
    match t.find(char::is_whitespace) {
        Some(i) => (&t[..i], t[i..].trim()),
        None => (t, ""),
    }
}

/// The agent harness `default` (`list.default`) names; none for `shell`.
pub fn default_agent<'a>(reg: &'a Registry, default: &str) -> Option<&'a Profile> {
    reg.profile(default).filter(|p| p.kind == RunKind::Agent)
}

/// A line that starts with `!`: a shell command, whatever the default.
fn is_bang(text: &str) -> bool {
    text.trim_start().starts_with('!')
}

/// What `text` starts: nothing, for a blank line, or for one that is the
/// start of a command's name still being typed. A line whose first word
/// names no harness starts `default` (`list.default`): an agent harness, or
/// a shell.
pub fn detect<'a>(text: &str, reg: &'a Registry, cmds: &Commands, default: &str) -> Option<Detected<'a>> {
    if slash::is_command(text) {
        return match slash::typed(text) {
            slash::Typed::Known(name) => Some(Detected::Command(name)),
            slash::Typed::Partial => None,
            slash::Typed::Unknown => Some(Detected::UnknownCommand),
        };
    }
    let (first, _) = split_first(text);
    if first.is_empty() {
        return None;
    }
    if is_bang(text) {
        return Some(Detected::Shell);
    }
    let agent = |profile: &'a Profile, named| Detected::Agent {
        profile,
        missing: (!profile.bins.iter().any(|b| cmds.has(b)))
            .then(|| profile.bins.first().map_or(profile.name.as_str(), String::as_str)),
        named,
    };
    Some(match (reg.by_word(first), default_agent(reg, default)) {
        (Some(profile), _) => agent(profile, true),
        (None, Some(profile)) => agent(profile, false),
        (None, None) => Detected::Shell,
    })
}

/// What `text`, as `d` reads it, hands on: an agent's prompt (the rest after
/// the harness's word, or all of it for the default's), or a shell's command
/// (without its `!`). Newlines inside are kept.
pub fn payload<'t>(text: &'t str, d: &Detected) -> &'t str {
    match d {
        Detected::Agent { named: true, .. } => split_first(text).1,
        Detected::Agent { named: false, .. } => text.trim(),
        Detected::Shell => {
            let t = text.trim();
            t.strip_prefix('!').map_or(t, str::trim)
        }
        Detected::Command(_) | Detected::UnknownCommand => text,
    }
}

/// Where the word under the cursor starts: after the last space or newline
/// before it.
fn word_start(text: &str, cur: usize) -> usize {
    text[..cur].rfind([' ', '\n']).map_or(0, |i| i + 1)
}

/// What completing a line looks at: the folder it starts in, the harnesses,
/// the programs on `PATH`, the name of the row under the cursor (for
/// `/rename`), and what a line starts by default (`list.default`).
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub folder: &'a Path,
    pub reg: &'a Registry,
    pub cmds: &'a Commands,
    pub name: Option<&'a str>,
    pub default: &'a str,
}

/// The agent harnesses that are here, and `shell`, by name: what `/default`
/// takes.
pub fn defaults(reg: &Registry, cmds: &Commands) -> Vec<String> {
    let mut out: Vec<String> = reg
        .agents()
        .filter(|p| p.bins.iter().any(|b| cmds.has(b)))
        .map(|p| p.name.clone())
        .chain([crate::config::SHELL_DEFAULT.to_string()])
        .collect();
    out.sort();
    out
}

/// What the word under the cursor completes to, best first, and where that
/// word starts. On a `/` line, a command and its argument (`slash`).
/// Otherwise the first word: the words of the harnesses that are here, then
/// the programs on `PATH` — only the harnesses where the default is an agent
/// (a program would be its prompt), only the programs after a `!`; a later
/// one: the paths under the folder (`~` is home), a folder's with its `/`.
pub fn candidates(text: &str, cur: usize, ctx: &Ctx) -> (usize, Vec<String>) {
    if slash::is_command(text) {
        let found = defaults(ctx.reg, ctx.cmds);
        return slash::candidates(text, cur, ctx.name, &found, ctx.folder);
    }
    let (folder, reg, cmds) = (ctx.folder, ctx.reg, ctx.cmds);
    let start = word_start(text, cur);
    let word = &text[start..cur];
    let before = text[..start].trim();
    let bang = before == "!";
    let first = before.is_empty() || bang;
    let mut out: Vec<String> = Vec::new();
    if first {
        if word.is_empty() {
            return (start, out);
        }
        if !bang {
            for (w, p) in reg.words() {
                if w.starts_with(word) && p.bins.iter().any(|b| cmds.has(b)) {
                    out.push(w.to_string());
                }
            }
        }
        if bang || default_agent(reg, ctx.default).is_none() {
            for n in cmds.starting(word) {
                if !out.iter().any(|o| o == n) {
                    out.push(n.to_string());
                }
            }
        }
    } else {
        out = paths(word, folder, false);
    }
    (start, out)
}

/// The paths `word` may be the start of, under `folder` (`~` is home): hidden
/// ones only once a `.` is typed; only folders where `dirs_only`, and a lone
/// `~` to home's own.
pub fn paths(word: &str, folder: &Path, dirs_only: bool) -> Vec<String> {
    if dirs_only && word == "~" {
        return vec!["~/".into()];
    }
    let (typed, base) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let dir = if typed.is_empty() {
        folder.to_path_buf()
    } else {
        let e = expand(typed);
        if e.is_absolute() { e } else { folder.join(e) }
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !name.starts_with(base) || (name.starts_with('.') && !base.starts_with('.')) {
                return None;
            }
            let dir = e.path().is_dir();
            if dirs_only && !dir {
                return None;
            }
            let slash = if dir { "/" } else { "" };
            Some(format!("{typed}{name}{slash}"))
        })
        .collect();
    found.sort();
    found
}

/// `~` for home, and the rest as typed.
pub fn expand(s: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match s.strip_prefix('~') {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            PathBuf::from(format!("{home}{rest}"))
        }
        _ => PathBuf::from(s),
    }
}

/// `tab` pressed again with several to choose from: what the word is
/// replaced with in turn.
#[derive(Debug, Clone)]
struct Cycle {
    head: String,
    tail: String,
    cands: Vec<String>,
    at: usize,
    /// The line as the last `tab` left it: typing anything since ends it.
    shown: String,
}

/// The line being typed, its ghost, and a `tab` cycle under way.
#[derive(Debug, Default, Clone)]
pub struct CmdLine {
    pub line: line::Line,
    /// What the best completion adds, shown dim after the cursor.
    pub ghost: Option<String>,
    cycle: Option<Cycle>,
}

impl CmdLine {
    pub fn is_empty(&self) -> bool {
        self.line.is_empty()
    }

    pub fn clear(&mut self) {
        *self = CmdLine::default();
    }

    /// The ghost again, for the line as it now stands: only with the
    /// cursor at the end of a word being typed — or, on a `/` line, of an
    /// argument not yet begun.
    pub fn refresh(&mut self, ctx: &Ctx) {
        self.ghost = None;
        let (text, cur) = (self.line.text(), self.line.cursor());
        if cur != text.len() || (text.ends_with([' ', '\n']) && !slash::is_command(text)) || text.is_empty() {
            return;
        }
        let (start, cands) = candidates(text, cur, ctx);
        let word = &text[start..cur];
        if let Some(best) = cands.first()
            && best.len() > word.len()
        {
            self.ghost = Some(best[word.len()..].to_string());
        }
    }

    /// `tab`: the one completion taken whole, with a space after a word that
    /// is done; of several, the best, and the next at each `tab` after.
    pub fn complete(&mut self, ctx: &Ctx) {
        if let Some(c) = self.cycle.as_mut()
            && c.shown == self.line.text()
        {
            c.at = (c.at + 1) % c.cands.len();
            let (text, at) = (
                format!("{}{}{}", c.head, c.cands[c.at], c.tail),
                c.head.len() + c.cands[c.at].len(),
            );
            c.shown = text.clone();
            self.line.set_with_cursor(text, at);
            self.ghost = None;
            return;
        }
        self.cycle = None;
        let (text, cur) = (self.line.text().to_string(), self.line.cursor());
        let (start, cands) = candidates(&text, cur, ctx);
        let (head, tail) = (text[..start].to_string(), text[cur..].to_string());
        let take = match cands.as_slice() {
            [] => return,
            [one] if one.ends_with('/') => one.clone(),
            [one] => format!("{one} "),
            many => many[0].clone(),
        };
        let now = format!("{head}{take}{tail}");
        self.line.set_with_cursor(now.clone(), head.len() + take.len());
        if cands.len() > 1 {
            self.cycle = Some(Cycle {
                head,
                tail,
                cands,
                at: 0,
                shown: now,
            });
            self.ghost = None;
        } else {
            self.refresh(ctx);
        }
    }

    /// Any key but `tab` ends a cycle.
    pub fn typed(&mut self, ctx: &Ctx) {
        self.cycle = None;
        self.refresh(ctx);
    }
}

/// A `PATH` of one folder holding these programs.
#[cfg(test)]
pub fn bin_dir(dir: &Path, names: &[&str]) -> Commands {
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        let p = dir.join(n);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Commands::scan(&[dir.to_path_buf()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_word_names_its_agent_and_anything_else_is_a_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let cmds = bin_dir(&tmp.path().join("bin"), &["claude", "codex", "make"]);
        std::fs::write(tmp.path().join("bin/notrun"), "").unwrap();
        let reg = Registry::builtin();
        let label = |t: &str| detect(t, &reg, &cmds, "shell").map(|d| d.label());
        assert_eq!(label("claude fix the login").as_deref(), Some("agent · claude"));
        assert_eq!(label("codex").as_deref(), Some("agent · codex"));
        assert_eq!(
            label("gemini hi").as_deref(),
            Some("agent · antigravity (agy not found)")
        );
        assert_eq!(label("make test").as_deref(), Some("shell"));
        assert_eq!(label("  "), None);
        assert_eq!(split_first("  claude  fix it "), ("claude", "fix it"));
        assert!(cmds.has("make") && !cmds.has("notrun"), "only what runs");
        assert_eq!(label("! make test").as_deref(), Some("shell"), "a ! changes nothing");
    }

    #[test]
    fn with_an_agent_for_default_a_line_naming_no_harness_is_its_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let cmds = bin_dir(&tmp.path().join("bin"), &["claude", "codex", "make"]);
        let reg = Registry::builtin();
        let read = |t: &'static str| {
            let d = detect(t, &reg, &cmds, "claude").unwrap();
            (d.label(), payload(t, &d).to_string())
        };
        assert_eq!(read("fix the login"), ("agent · claude".into(), "fix the login".into()));
        assert_eq!(
            read("codex fix"),
            ("agent · codex".into(), "fix".into()),
            "a word still picks"
        );
        assert_eq!(read("! make test"), ("shell".into(), "make test".into()));
        assert_eq!(read("!ls"), ("shell".into(), "ls".into()));
        assert_eq!(
            read("fix the login\nand the logout\n"),
            ("agent · claude".into(), "fix the login\nand the logout".into()),
            "the newlines kept"
        );
        assert_eq!(
            read("claude fix\n- the login"),
            ("agent · claude".into(), "fix\n- the login".into())
        );
        assert!(matches!(
            detect("/pin", &reg, &cmds, "claude"),
            Some(Detected::Command("pin"))
        ));
        // Not an agent: the default reads as a shell.
        for d in ["shell", "sh", "nope"] {
            assert_eq!(
                detect("make", &reg, &cmds, d).map(|d| d.label()).as_deref(),
                Some("shell")
            );
        }
        assert_eq!(defaults(&reg, &cmds), ["claude", "codex", "shell"]);
    }

    #[test]
    fn with_an_agent_for_default_the_first_word_completes_to_harnesses_only_but_after_a_bang() {
        let tmp = tempfile::tempdir().unwrap();
        let cmds = bin_dir(&tmp.path().join("bin"), &["claude", "clang", "codex"]);
        let reg = Registry::builtin();
        let here = tmp.path().to_path_buf();
        let ctx = Ctx {
            folder: &here,
            reg: &reg,
            cmds: &cmds,
            name: None,
            default: "claude",
        };
        let c = |t: &str| candidates(t, t.len(), &ctx).1;
        assert_eq!(c("cl"), ["claude"], "clang would be the prompt");
        assert_eq!(c("! cl"), ["clang", "claude"], "after a !, the programs");
        assert_eq!(c("/default c"), ["claude", "codex"]);
        assert_eq!(c("/default s"), ["shell"]);
    }

    #[test]
    fn the_first_word_completes_to_harnesses_then_programs_and_later_ones_to_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let cmds = bin_dir(&tmp.path().join("bin"), &["claude", "clang", "agy"]);
        let reg = Registry::builtin();
        let here = tmp.path().join("shop");
        std::fs::create_dir_all(here.join("src/app")).unwrap();
        std::fs::write(here.join("src/main.rs"), "").unwrap();
        std::fs::write(here.join(".env"), "").unwrap();
        let ctx = Ctx {
            folder: &here,
            reg: &reg,
            cmds: &cmds,
            name: None,
            default: "shell",
        };
        let c = |t: &str| candidates(t, t.len(), &ctx).1;
        assert_eq!(c("cl"), ["claude", "clang"]);
        assert_eq!(c("gem"), ["gemini"], "agy is here, so gemini is offered");
        assert!(c("cod").is_empty(), "codex is not installed");
        assert_eq!(c("cat s"), ["src/"]);
        assert_eq!(c("cat src/"), ["src/app/", "src/main.rs"]);
        assert_eq!(c("cat src/m"), ["src/main.rs"]);
        assert!(c("ls ").iter().all(|p| !p.contains(".env")), "{:?}", c("ls "));
        assert_eq!(c("ls ."), [".env"]);
        assert!(c("").is_empty());
    }

    #[test]
    fn tab_takes_the_one_and_cycles_the_many_and_the_ghost_shows_the_best() {
        let tmp = tempfile::tempdir().unwrap();
        let cmds = bin_dir(&tmp.path().join("bin"), &["claude", "clang", "codex"]);
        let reg = Registry::builtin();
        let here = tmp.path().to_path_buf();
        let ctx = Ctx {
            folder: &here,
            reg: &reg,
            cmds: &cmds,
            name: None,
            default: "shell",
        };
        let mut l = CmdLine::default();
        l.line.insert("cod");
        l.typed(&ctx);
        assert_eq!(l.ghost.as_deref(), Some("ex"));
        l.complete(&ctx);
        assert_eq!(l.line.text(), "codex ");
        assert_eq!(l.ghost, None);
        let mut l = CmdLine::default();
        l.line.insert("cl");
        l.typed(&ctx);
        assert_eq!(l.ghost.as_deref(), Some("aude"), "the harness first");
        l.complete(&ctx);
        assert_eq!(l.line.text(), "claude");
        l.complete(&ctx);
        assert_eq!(l.line.text(), "clang");
        l.complete(&ctx);
        assert_eq!(l.line.text(), "claude", "round again");
    }
}
