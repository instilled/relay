//! `rl setup`: for each harness, its user hooks (`rl_detect::user_hooks`)
//! where it has them, and the `rl mcp --harness <h>` server registered at
//! user level where it has a place for one. Both are the harness's to say
//! (`Harness::user_hooks`, `Harness::mcp`): Claude's server goes through
//! `claude mcp`, which owns `~/.claude.json`, and Antigravity's through `agy
//! mcp`, which owns `~/.gemini/config/mcp_config.json`; Codex's and
//! Gemini's are written into their own files (`rl_detect::mcp_entry`).
//! Edits keep everything else in them; a file that does not read is left as
//! it was. `--remove` takes out only Relay's.
//!
//! Each part answers "would a run change this?" from its files alone
//! (`Place::need`); a run writes only what that names, and `--check`
//! (`check`) stops there, so it never writes nor runs a harness's CLI. The
//! list asks `check` as it starts and offers `/setup`; its first start asks
//! once (`first_run`).

use crate::config::Config;
use anyhow::{Context as _, Result, anyhow, bail};
use rl_core::Home;
use rl_detect::harness::{self, Harness};
use rl_detect::mcp_entry::{self, SERVER, Want};
use rl_detect::registry::{Profile, find_bin};
use rl_detect::sessions::UserHooks;
use rl_detect::{Registry, user_hooks};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use rl_detect::mcp_entry::State;

/// `rl setup` for the harnesses named, or every agent `bin_of` finds (for
/// `--remove`, every agent with hooks or a server: one whose files hold
/// Relay's entries is cleaned whether or not it is still installed). `env` is how variables
/// are read and `bin_of` where a harness's binary is, so a test can stand
/// in another home and another `claude`. Returns one line per harness that
/// changed; `nothing to change` when none did.
pub fn run(
    home: &Home,
    only: &[String],
    remove: bool,
    rl_bin: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
    bin_of: &impl Fn(&Profile) -> Option<PathBuf>,
) -> Result<Vec<String>> {
    let registry = Registry::load(home)?;
    let harnesses = select(&registry, only, remove, bin_of)?;
    let want = want(home, rl_bin, env);
    let mut lines = Vec::new();
    for h in &harnesses {
        lines.extend(one(h.as_ref(), remove, &want, env)?);
    }
    if !remove && only.is_empty() {
        let mut config = Config::load(home)?;
        config.harnesses = harnesses.iter().map(|h| h.name().to_string()).collect();
        config.save(home)?;
    }
    if lines.is_empty() {
        lines.push("nothing to change".into());
    }
    Ok(lines)
}

/// `rl setup --check`: what `run` would change for the same harnesses,
/// read from the files alone — nothing is written, and no harness's CLI is
/// run (Claude's and agy's entries are read from the files those CLIs
/// keep). One `Need` per harness that has something to change; none when
/// all is set up.
pub fn check(
    home: &Home,
    only: &[String],
    rl_bin: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
    bin_of: &impl Fn(&Profile) -> Option<PathBuf>,
) -> Result<Vec<Need>> {
    let registry = Registry::load(home)?;
    let want = want(home, rl_bin, env);
    let mut needs = Vec::new();
    for h in select(&registry, only, false, bin_of)? {
        let place = Place::of(h.as_ref(), env)?;
        needs.extend(place.need(&want, env)?);
    }
    Ok(needs)
}

/// The harnesses a run handles: those named, or every agent `bin_of` finds
/// (for `remove`, every agent with hooks or a server).
fn select(
    registry: &Registry,
    only: &[String],
    remove: bool,
    bin_of: &impl Fn(&Profile) -> Option<PathBuf>,
) -> Result<Vec<Box<dyn Harness>>> {
    let of = |p: &Profile| harness::of(p, bin_of(p));
    if only.is_empty() {
        return Ok(registry
            .agents()
            .map(of)
            .filter(|h| set_up(h.as_ref()) && (remove || h.found()))
            .collect());
    }
    only.iter()
        .map(|n| match registry.profile(n).map(of) {
            Some(h) if set_up(h.as_ref()) => Ok(h),
            _ => bail!("{n}: no such harness to set up"),
        })
        .collect()
}

/// Whether `rl setup` has anything to do for `h`: hooks of its own, or a
/// place for the server.
fn set_up(h: &dyn Harness) -> bool {
    h.user_hooks().is_some() || h.has_mcp()
}

/// What a harness needs from `rl setup`: each of its parts that a run would
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub harness: String,
    pub items: Vec<Part>,
}

/// One part of a harness's setup that is not as a run would leave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub what: What,
    pub state: State,
    /// The file it is in, with the home folder as `~`.
    pub file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Hooks,
    /// The `relay` MCP server's entry.
    Mcp,
}

impl std::fmt::Display for What {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            What::Hooks => f.write_str("hooks"),
            What::Mcp => write!(f, "mcp {SERVER}"),
        }
    }
}

/// `hooks missing`, `mcp relay stale (points at /old/rl)`.
impl std::fmt::Display for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.state {
            State::Missing => write!(f, "{} missing", self.what),
            State::Stale { was: None } => write!(f, "{} stale", self.what),
            State::Stale { was: Some(rl) } => write!(f, "{} stale (points at {rl})", self.what),
        }
    }
}

/// `codex: hooks missing · mcp relay stale (points at /old/rl)`.
impl std::fmt::Display for Need {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let items: Vec<String> = self.items.iter().map(Part::to_string).collect();
        write!(f, "{}: {}", self.harness, items.join(" · "))
    }
}

impl Need {
    /// What a run would touch, file by file: `codex: ~/.codex/hooks.json
    /// (hooks) · ~/.codex/config.toml (mcp relay)`.
    pub fn touches(&self) -> String {
        // One entry per file, naming everything that changes in it.
        let mut files: Vec<(String, Vec<String>)> = Vec::new();
        for i in &self.items {
            let what = i.what.to_string();
            match files.iter_mut().find(|(f, _)| *f == i.file) {
                Some((_, whats)) => whats.push(what),
                None => files.push((i.file.clone(), vec![what])),
            }
        }
        let files: Vec<String> = files.iter().map(|(f, w)| format!("{f} ({})", w.join(", "))).collect();
        format!("{}: {}", self.harness, files.join(" · "))
    }
}

/// The `rl setup` of the real process: this binary, the real environment
/// and `PATH`.
pub fn main(home: &Home, only: &[String], remove: bool) -> Result<()> {
    let host = Host::real()?;
    let lines = run(home, only, remove, &host.rl_bin, &|k| (host.env)(k), &|p| {
        (host.bin_of)(p)
    })?;
    for l in if remove { lines } else { crate::guide::with_next(lines) } {
        println!("{l}");
    }
    Ok(())
}

/// `rl setup --check` of the real process: one line per harness that needs
/// setting up, or `all set up`. True when one does.
pub fn main_check(home: &Home, only: &[String]) -> Result<bool> {
    let host = Host::real()?;
    let needs = check(home, only, &host.rl_bin, &|k| (host.env)(k), &|p| (host.bin_of)(p))?;
    if needs.is_empty() {
        println!("all set up");
    }
    for n in &needs {
        println!("{n}");
    }
    Ok(!needs.is_empty())
}

/// What setup runs with, kept for the list to run it later and off its
/// thread: this `rl`, how variables are read, where a harness's binary is.
#[derive(Clone)]
pub struct Host {
    pub rl_bin: PathBuf,
    pub env: EnvOf,
    pub bin_of: BinOf,
}

/// How a variable is read.
pub type EnvOf = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;
/// Where a harness's binary is.
pub type BinOf = Arc<dyn Fn(&Profile) -> Option<PathBuf> + Send + Sync>;

impl Host {
    /// This binary, the real environment and `PATH`.
    pub fn real() -> Result<Host> {
        Ok(Host {
            rl_bin: std::env::current_exe().context("where rl is")?,
            env: Arc::new(|k| std::env::var_os(k)),
            bin_of: Arc::new(|p| find_bin(&p.bins)),
        })
    }

    /// `check` for every harness found.
    pub fn check(&self, home: &Home) -> Result<Vec<Need>> {
        check(home, &[], &self.rl_bin, &|k| (self.env)(k), &|p| (self.bin_of)(p))
    }

    /// `rl setup` for every harness found.
    pub fn run(&self, home: &Home) -> Result<Vec<String>> {
        run(home, &[], false, &self.rl_bin, &|k| (self.env)(k), &|p| {
            (self.bin_of)(p)
        })
    }
}

/// Whether the list's start asks to set Relay up: on a terminal, not in a
/// session Relay holds, and before there is a `config.json` — which the
/// first start writes, whatever the answer, so it asks only once.
pub fn should_ask(home: &Home, tty: bool, in_session: bool) -> bool {
    tty && !in_session && !home.config_json().exists()
}

/// Says what setting up touches, one harness a line, and asks. True on
/// enter or `y`; anything else, or the end of the input, is a no.
pub fn ask(needs: &[Need], r: &mut impl BufRead, w: &mut impl Write) -> std::io::Result<bool> {
    writeln!(w, "Relay is not set up yet. Setting it up changes:")?;
    for n in needs {
        writeln!(w, "  {}", n.touches())?;
    }
    let names: Vec<&str> = needs.iter().map(|n| n.harness.as_str()).collect();
    write!(w, "Set up Relay for {}? [Y/n] ", names.join(", "))?;
    w.flush()?;
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        writeln!(w)?;
        return Ok(false);
    }
    Ok(matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes"))
}

/// The list's first start (`should_ask`): where a harness found needs
/// setting up, asks, and on a yes sets up and says what changed — then
/// waits for enter, so that is read before the list takes the screen.
/// Either way `config.json` is written, so it is not asked again; the
/// list's `/setup` is there for later.
pub fn first_run(home: &Home, host: &Host, r: &mut impl BufRead, w: &mut impl Write) -> Result<()> {
    let needs = host.check(home)?;
    if needs.is_empty() {
        return Ok(());
    }
    if ask(&needs, r, w)? {
        match host.run(home) {
            Ok(lines) => {
                for l in crate::guide::with_next(lines) {
                    writeln!(w, "{l}")?;
                }
            }
            Err(e) => writeln!(w, "rl setup: {e:#}")?,
        }
        write!(w, "enter opens the list ")?;
        w.flush()?;
        r.read_line(&mut String::new())?;
    }
    if !home.config_json().exists() {
        Config::load(home)?.save(home)?;
    }
    Ok(())
}

/// What every entry runs: this `rl`, and the Relay home where it is not the
/// default one.
fn want(home: &Home, rl_bin: &Path, env: &impl Fn(&str) -> Option<OsString>) -> Want {
    Want {
        rl: rl_bin.to_string_lossy().into_owned(),
        relay_home: relay_home(home, env),
    }
}

/// The Relay home, unless it is the default (`$HOME/.relay`) the server
/// finds on its own.
fn relay_home(home: &Home, env: &impl Fn(&str) -> Option<OsString>) -> Option<String> {
    let default = env("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".relay"));
    (default.as_deref() != Some(home.path())).then(|| home.path().to_string_lossy().into_owned())
}

/// One harness: its hooks, where it has them, then its MCP entry. A line
/// naming what changed. Setting up asks `Place::need` first — what
/// `check` asks — and writes only what it names.
fn one(h: &dyn Harness, remove: bool, want: &Want, env: &impl Fn(&str) -> Option<OsString>) -> Result<Vec<String>> {
    let name = h.name();
    let place = Place::of(h, env)?;
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    let mcp_line = |mcp: &mcp_entry::Place, word: &str| {
        let note = mcp.note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default();
        format!("mcp {SERVER} {word} ({}){note}", tilde(&mcp.path, env))
    };
    if remove {
        if let Some((hooks, file)) = &place.hooks
            && user_hooks::uninstall(file, hooks).with_context(|| name.to_string())?
        {
            parts.push("hooks removed".to_string());
        }
        if let Some(mcp) = &place.mcp
            && mcp.remove().with_context(|| name.to_string())?
        {
            parts.push(mcp_line(mcp, "removed"));
        }
    } else if let Some(need) = place.need(want, env)? {
        for item in &need.items {
            match (item.what, &place.hooks, &place.mcp) {
                (What::Hooks, Some((hooks, file)), _) => {
                    user_hooks::install(file, hooks, Path::new(&want.rl)).with_context(|| name.to_string())?;
                    parts.push("hooks installed".to_string());
                    if let Some(note) = &hooks.after_install {
                        notes.push(format!("{name}: {note}"));
                    }
                }
                (What::Mcp, _, Some(mcp)) => {
                    mcp.register(want).with_context(|| name.to_string())?;
                    parts.push(mcp_line(mcp, "registered"));
                }
                _ => {}
            }
        }
    }
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![format!("{name}: {}", parts.join(" · "))];
    out.extend(notes);
    Ok(out)
}

/// Where a harness's hooks and MCP server are.
struct Place<'a> {
    name: &'a str,
    hooks: Option<(&'a UserHooks, PathBuf)>,
    mcp: Option<mcp_entry::Place>,
}

impl<'a> Place<'a> {
    fn of(h: &'a dyn Harness, env: &impl Fn(&str) -> Option<OsString>) -> Result<Place<'a>> {
        let name = h.name();
        let hooks = h
            .user_hooks()
            .map(|hooks| {
                hooks
                    .file(env)
                    .map(|f| (hooks, f))
                    .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its hooks file is is not known"))
            })
            .transpose()?;
        let mcp = h.mcp(env)?;
        Ok(Place { name, hooks, mcp })
    }

    /// What setting up would change here, read from the files alone: no
    /// harness CLI is run, nothing is written. An error where a run would
    /// fail before changing anything (a CLI it needs not on PATH, a file
    /// that does not read).
    fn need(&self, want: &Want, env: &impl Fn(&str) -> Option<OsString>) -> Result<Option<Need>> {
        let name = self.name;
        if let Some(cli) = self.mcp.as_ref().and_then(mcp_entry::Place::cli_missing) {
            bail!("{name}: not on PATH; its MCP server is registered with `{cli}`, so install it first");
        }
        let mut items = Vec::new();
        if let Some((hooks, file)) = &self.hooks {
            let rl = Path::new(&want.rl);
            if user_hooks::install_needed(file, hooks, rl).with_context(|| name.to_string())? {
                let state = if user_hooks::installed(file, hooks)? {
                    State::Stale {
                        was: user_hooks::programs(file, hooks)?.into_iter().find(|p| *p != want.rl),
                    }
                } else {
                    State::Missing
                };
                items.push(Part {
                    what: What::Hooks,
                    state,
                    file: tilde(file, env),
                });
            }
        }
        if let Some(mcp) = &self.mcp
            && let Some(state) = mcp.need(want).with_context(|| name.to_string())?
        {
            items.push(Part {
                what: What::Mcp,
                state,
                file: tilde(&mcp.path, env),
            });
        }
        Ok((!items.is_empty()).then(|| Need {
            harness: name.to_string(),
            items,
        }))
    }
}

/// `path` with the home folder as `~`.
fn tilde(path: &Path, env: &impl Fn(&str) -> Option<OsString>) -> String {
    env("HOME")
        .and_then(|h| path.strip_prefix(h).ok().map(|r| format!("~/{}", r.display())))
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_detect::mcp_entry::toml_json;
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use toml_edit::DocumentMut;

    const RL: &str = "/opt/relay/bin/rl";

    /// The stand-in for `claude` (`fixtures/stand-in-claude-mcp`).
    fn stand_in() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-claude-mcp"))
    }

    /// The stand-in for `agy` (`fixtures/stand-in-agy-mcp`).
    fn stand_in_agy() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-agy-mcp"))
    }

    /// Where agy keeps its MCP servers, under the HOME.
    const AGY_FILE: &str = ".gemini/config/mcp_config.json";

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: Home,
        dir: PathBuf,
    }

    impl Fixture {
        /// A HOME in a tempdir with the default Relay home in it.
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            let home = Home::at(dir.join(".relay"));
            Fixture { _tmp: tmp, home, dir }
        }

        fn env(&self, k: &str) -> Option<OsString> {
            match k {
                "HOME" => Some(self.dir.clone().into()),
                _ => None,
            }
        }

        /// `rl setup` with every harness on PATH but those `gone`.
        fn try_run(&self, only: &[&str], remove: bool, gone: &[&str]) -> Result<Vec<String>> {
            let only: Vec<String> = only.iter().map(|s| s.to_string()).collect();
            run(
                &self.home,
                &only,
                remove,
                Path::new(RL),
                &|k| self.env(k),
                &|p: &Profile| bin(p, gone),
            )
        }

        fn run(&self, only: &[&str], remove: bool) -> Vec<String> {
            self.try_run(only, remove, &[]).unwrap()
        }

        fn json(&self, rel: &str) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.dir.join(rel)).unwrap()).unwrap()
        }

        fn put(&self, rel: &str, text: &str) {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        /// What `claude` was run with, one call a line.
        fn claude_calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("argv"))
                .map(|s| s.lines().map(String::from).collect())
                .unwrap_or_default()
        }

        /// What `agy` was run with, one call a line.
        fn agy_calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join(".gemini/config/argv"))
                .map(|s| s.lines().map(String::from).collect())
                .unwrap_or_default()
        }

        fn codex(&self) -> Value {
            let text = std::fs::read_to_string(self.dir.join(".codex/config.toml")).unwrap();
            let doc: DocumentMut = text.parse().unwrap();
            toml_json(&doc["mcp_servers"]["relay"])
        }
    }

    /// Claude and agy are the stand-ins; the others are never run.
    fn bin(p: &Profile, gone: &[&str]) -> Option<PathBuf> {
        let name = p.name.as_str();
        if gone.contains(&name) || !["claude", "codex", "gemini", "antigravity"].contains(&name) {
            return None;
        }
        Some(match name {
            "claude" => stand_in(),
            "antigravity" => stand_in_agy(),
            _ => PathBuf::from(format!("/nowhere/{name}")),
        })
    }

    /// The harnesses a run changed: its lines naming the MCP entry.
    fn changes(lines: &[String]) -> usize {
        lines.iter().filter(|l| l.contains("mcp relay")).count()
    }

    fn args(h: &str) -> Value {
        json!(["mcp", "--harness", h])
    }

    const ADD: &str = "mcp add --scope user relay -- /opt/relay/bin/rl mcp --harness claude";
    const REMOVE: &str = "mcp remove --scope user relay";
    const AGY_ADD: &str = "mcp add relay -- /opt/relay/bin/rl mcp --harness antigravity";

    #[test]
    fn sets_up_all_four() {
        let f = Fixture::new();
        let lines = f.run(&[], false);
        // One line per harness, and the profile's note after codex's.
        assert_eq!(changes(&lines), 4, "{lines:?}");
        assert!(
            lines.contains(&"antigravity: mcp relay registered (~/.gemini/config/mcp_config.json)".to_string()),
            "no hooks of its own yet, only the server: {lines:?}"
        );
        assert_eq!(f.agy_calls(), [AGY_ADD], "through agy mcp");
        assert_eq!(
            f.json(AGY_FILE)["mcpServers"]["relay"],
            json!({"args": args("antigravity"), "command": RL, "disabled": false})
        );
        assert!(lines.iter().any(|l| l.contains("/hooks") && !l.contains("mcp relay")));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("claude: hooks installed · mcp relay registered (~/.claude.json)"))
        );
        assert!(lines.iter().any(|l| l.starts_with("codex: hooks installed")));
        assert!(lines.iter().any(|l| l.contains(
            "gemini: hooks installed · mcp relay registered (~/.gemini/settings.json) (gemini mcp format unverified)"
        )));

        assert_eq!(
            f.claude_calls(),
            [REMOVE, ADD],
            "through claude mcp, the old one out first"
        );
        assert_eq!(
            f.json(".claude.json")["mcpServers"]["relay"],
            json!({"type": "stdio", "command": RL, "args": args("claude"), "env": {}})
        );
        assert!(f.json(".claude/settings.json")["hooks"].is_object());
        assert!(f.json(".codex/hooks.json")["hooks"].is_object());
        let g = f.json(".gemini/settings.json");
        assert_eq!(g["mcpServers"]["relay"], json!({"command": RL, "args": args("gemini")}));
        assert!(g["hooks"].is_object());
        assert_eq!(
            f.codex(),
            json!({
                "command": RL,
                "args": args("codex"),
                "env_vars": ["RELAY_HOME", "RL_SESSION", "RL_SESSION_AGENT"],
            })
        );
    }

    impl Fixture {
        /// `rl setup --check` with every harness on PATH, for this `rl`.
        fn try_check(&self, only: &[&str], rl: &str) -> Result<Vec<Need>> {
            let only: Vec<String> = only.iter().map(|s| s.to_string()).collect();
            check(&self.home, &only, Path::new(rl), &|k| self.env(k), &|p: &Profile| {
                bin(p, &[])
            })
        }

        fn check(&self, rl: &str) -> Vec<String> {
            self.try_check(&[], rl).unwrap().iter().map(Need::to_string).collect()
        }

        /// Every file under the HOME, with what is in it.
        fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
            fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
                let Ok(entries) = std::fs::read_dir(dir) else { return };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.push((p.clone(), std::fs::read(&p).unwrap()));
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.dir, &mut out);
            out.sort();
            out
        }

        /// The setup of a process with this HOME, this `rl`, and every
        /// harness on PATH (the stand-ins).
        fn host(&self) -> Host {
            let dir = self.dir.clone();
            Host {
                rl_bin: PathBuf::from(RL),
                env: Arc::new(move |k| (k == "HOME").then(|| dir.clone().into())),
                bin_of: Arc::new(|p| bin(p, &[])),
            }
        }
    }

    #[test]
    fn check_names_what_a_run_would_change_and_changes_nothing() {
        let f = Fixture::new();
        let before = f.files();
        assert_eq!(
            f.check(RL),
            [
                "antigravity: mcp relay missing",
                "claude: hooks missing · mcp relay missing",
                "codex: hooks missing · mcp relay missing",
                "gemini: hooks missing · mcp relay missing",
            ]
        );
        assert_eq!(f.files(), before, "nothing written");
        assert!(f.claude_calls().is_empty() && f.agy_calls().is_empty(), "no CLI run");
        let codex = &f.try_check(&["codex"], RL).unwrap()[0];
        assert_eq!(
            codex.touches(),
            "codex: ~/.codex/hooks.json (hooks) · ~/.codex/config.toml (mcp relay)"
        );
        let gemini = &f.try_check(&["gemini"], RL).unwrap()[0];
        assert_eq!(
            gemini.touches(),
            "gemini: ~/.gemini/settings.json (hooks, mcp relay)",
            "one file named once"
        );

        f.run(&[], false);
        assert!(f.check(RL).is_empty(), "set up: {:?}", f.check(RL));
        assert_eq!(f.claude_calls().len(), 2, "check ran no claude");
    }

    #[test]
    fn check_tells_a_moved_rl_and_a_disabled_entry_as_stale() {
        let f = Fixture::new();
        f.run(&[], false);
        let before = f.files();
        let calls = (f.claude_calls(), f.agy_calls());
        assert_eq!(
            f.check("/new/rl"),
            [
                "antigravity: mcp relay stale (points at /opt/relay/bin/rl)",
                "claude: hooks stale (points at /opt/relay/bin/rl) · mcp relay stale (points at /opt/relay/bin/rl)",
                "codex: hooks stale (points at /opt/relay/bin/rl) · mcp relay stale (points at /opt/relay/bin/rl)",
                "gemini: hooks stale (points at /opt/relay/bin/rl) · mcp relay stale (points at /opt/relay/bin/rl)",
            ]
        );
        assert_eq!(f.files(), before);
        assert_eq!((f.claude_calls(), f.agy_calls()), calls);

        let mut agy = f.json(AGY_FILE);
        agy["mcpServers"]["relay"]["disabled"] = json!(true);
        f.put(AGY_FILE, &agy.to_string());
        assert_eq!(f.check(RL), ["antigravity: mcp relay stale"]);
        // What check says is what a run then does, and no more.
        assert_eq!(f.run(&[], false).len(), 1);
        assert!(f.check(RL).is_empty());
    }

    #[test]
    fn check_refuses_where_a_run_would() {
        let f = Fixture::new();
        f.put(".codex/config.toml", "mcp_servers = 3\n");
        let e = f.try_check(&["codex"], RL).unwrap_err();
        assert!(format!("{e:#}").contains("not a table"), "{e:#}");
        f.put(".codex/config.toml", "");
        f.put(".gemini/settings.json", "{ nope");
        assert!(f.try_check(&["gemini"], RL).is_err());
        assert!(f.try_check(&["nope"], RL).is_err());
    }

    #[test]
    fn the_first_start_asks_only_on_a_terminal_outside_a_session_and_before_a_config() {
        let f = Fixture::new();
        assert!(should_ask(&f.home, true, false));
        assert!(!should_ask(&f.home, false, false), "not a terminal");
        assert!(!should_ask(&f.home, true, true), "in a held session");
        f.put(".relay/config.json", "{}");
        assert!(!should_ask(&f.home, true, false), "asked before");
    }

    #[test]
    fn ask_says_what_it_touches_and_takes_enter_or_y_as_yes() {
        let f = Fixture::new();
        let needs = f.try_check(&[], RL).unwrap();
        let answer = |input: &str| {
            let mut out = Vec::new();
            let yes = ask(&needs, &mut input.as_bytes(), &mut out).unwrap();
            (yes, String::from_utf8(out).unwrap())
        };
        let (yes, out) = answer("\n");
        assert!(yes);
        assert!(
            out.contains("  codex: ~/.codex/hooks.json (hooks) · ~/.codex/config.toml (mcp relay)\n"),
            "{out}"
        );
        assert!(
            out.contains("  antigravity: ~/.gemini/config/mcp_config.json (mcp relay)\n"),
            "{out}"
        );
        assert!(
            out.ends_with("Set up Relay for antigravity, claude, codex, gemini? [Y/n] "),
            "{out}"
        );
        assert!(answer("y\n").0 && answer(" Yes \n").0);
        assert!(!answer("n\n").0 && !answer("no\n").0 && !answer("later\n").0);
        assert!(!answer("").0, "the end of the input is a no");
    }

    #[test]
    fn a_yes_on_the_first_start_sets_up_and_keeps_a_config() {
        let f = Fixture::new();
        let mut out = Vec::new();
        first_run(&f.home, &f.host(), &mut "\n\n".as_bytes(), &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("codex: hooks installed"), "{out}");
        assert!(out.contains("/hooks"), "the profile's note: {out}");
        assert!(out.ends_with("enter opens the list "), "{out}");
        assert!(f.check(RL).is_empty());
        assert!(f.home.config_json().exists());
        assert!(!should_ask(&f.home, true, false));
    }

    #[test]
    fn a_no_on_the_first_start_changes_nothing_but_is_kept() {
        let f = Fixture::new();
        let mut out = Vec::new();
        first_run(&f.home, &f.host(), &mut "n\n".as_bytes(), &mut out).unwrap();
        assert_eq!(f.check(RL).len(), 4, "not set up");
        assert!(f.claude_calls().is_empty() && f.agy_calls().is_empty());
        assert!(f.home.config_json().exists(), "so it is not asked again");
    }

    #[test]
    fn a_first_start_with_nothing_to_set_up_asks_nothing() {
        let f = Fixture::new();
        f.run(&[], false);
        std::fs::remove_file(f.home.config_json()).unwrap();
        let mut out = Vec::new();
        first_run(&f.home, &f.host(), &mut "".as_bytes(), &mut out).unwrap();
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn a_relay_home_not_the_default_goes_in_each_entry() {
        let mut f = Fixture::new();
        f.home = Home::at(f.dir.join("elsewhere"));
        f.run(&[], false);
        let home = f.dir.join("elsewhere").to_string_lossy().into_owned();
        assert_eq!(
            f.claude_calls()[1],
            format!("mcp add --scope user relay -e RELAY_HOME={home} -- {RL} mcp --harness claude")
        );
        assert_eq!(
            f.json(".claude.json")["mcpServers"]["relay"]["env"],
            json!({"RELAY_HOME": home})
        );
        assert_eq!(
            f.json(".gemini/settings.json")["mcpServers"]["relay"]["env"],
            json!({"RELAY_HOME": home})
        );
        assert_eq!(f.codex()["env"], json!({"RELAY_HOME": home}));
        assert_eq!(
            f.agy_calls(),
            [format!(
                "mcp add -e RELAY_HOME={home} relay -- {RL} mcp --harness antigravity"
            )]
        );
        assert_eq!(
            f.json(AGY_FILE)["mcpServers"]["relay"]["env"],
            json!({"RELAY_HOME": home})
        );
        assert_eq!(f.run(&[], false), ["nothing to change"]);
    }

    #[test]
    fn second_run_changes_nothing() {
        let f = Fixture::new();
        f.run(&[], false);
        let files = [
            ".claude.json",
            ".claude/settings.json",
            ".codex/hooks.json",
            ".codex/config.toml",
            ".gemini/settings.json",
            AGY_FILE,
        ];
        let before = files.map(|p| std::fs::read(f.dir.join(p)).unwrap());
        assert_eq!(f.run(&[], false), ["nothing to change"]);
        assert_eq!(before, files.map(|p| std::fs::read(f.dir.join(p)).unwrap()));
        assert_eq!(f.claude_calls().len(), 2, "claude not run again");
        assert_eq!(f.agy_calls().len(), 1, "agy not run again");
    }

    #[test]
    fn keeps_what_else_is_in_the_files() {
        let f = Fixture::new();
        f.put(
            ".codex/config.toml",
            "# my codex\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        );
        f.put(
            ".gemini/settings.json",
            r#"{"theme": "dark", "mcpServers": {"s": {"command": "y"}}}"#,
        );
        f.run(&[], false);

        let g = f.json(".gemini/settings.json");
        assert_eq!(g["theme"], "dark");
        assert_eq!(g["mcpServers"]["s"]["command"], "y");
        let toml = std::fs::read_to_string(f.dir.join(".codex/config.toml")).unwrap();
        assert!(toml.starts_with("# my codex\nmodel = \"o3\""), "{toml}");
        assert!(toml.contains("[mcp_servers.other]\ncommand = \"x\""), "{toml}");
        assert!(toml.contains("[mcp_servers.relay]"), "{toml}");
    }

    #[test]
    fn remove_takes_out_only_relays() {
        let f = Fixture::new();
        f.put(
            ".codex/config.toml",
            "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        );
        f.run(&[], false);
        let lines = f.run(&[], true);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines.contains(&"antigravity: mcp relay removed (~/.gemini/config/mcp_config.json)".to_string()));
        assert_eq!(f.agy_calls().last().map(String::as_str), Some("mcp remove relay"));
        assert_eq!(f.json(AGY_FILE), json!({"mcpServers": {}}));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("claude: hooks removed · mcp relay removed (~/.claude.json)"))
        );
        assert_eq!(f.claude_calls().last().map(String::as_str), Some(REMOVE));
        assert_eq!(f.json(".claude.json"), json!({"mcpServers": {}}));
        let g = f.json(".gemini/settings.json");
        assert!(
            g.get("mcpServers").is_none() && g["hooks"].as_object().is_none_or(|h| h.is_empty()),
            "{g}"
        );
        let toml = std::fs::read_to_string(f.dir.join(".codex/config.toml")).unwrap();
        assert_eq!(toml, "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n");
        assert_eq!(f.run(&[], true), ["nothing to change"]);
        assert_eq!(f.agy_calls().len(), 2, "agy not asked to remove what is not there");
    }

    #[test]
    fn remove_cleans_a_harness_no_longer_on_path() {
        let f = Fixture::new();
        f.run(&[], false);
        let lines = f.try_run(&[], true, &["codex", "gemini"]).unwrap();
        assert_eq!(changes(&lines), 4, "{lines:?}");
        assert!(f.codex_gone() && f.json(".gemini/settings.json").get("mcpServers").is_none());
    }

    impl Fixture {
        fn codex_gone(&self) -> bool {
            let text = std::fs::read_to_string(self.dir.join(".codex/config.toml")).unwrap();
            !text.contains("relay")
        }
    }

    #[test]
    fn claude_not_on_path_is_an_error_where_it_is_needed() {
        let f = Fixture::new();
        let e = f.try_run(&["claude"], false, &["claude"]).unwrap_err();
        assert!(format!("{e:#}").contains("not on PATH"), "{e:#}");
        assert!(!f.dir.join(".claude").exists(), "its hooks are left alone too");
        // Without --harness it is simply not set up.
        let lines = f.try_run(&[], false, &["claude"]).unwrap();
        assert!(lines.iter().all(|l| !l.starts_with("claude")), "{lines:?}");

        // Registered, then claude went: --remove cannot take it out, and says so.
        let f = Fixture::new();
        f.run(&["claude"], false);
        let e = f.try_run(&[], true, &["claude"]).unwrap_err();
        assert!(format!("{e:#}").contains("claude is not on PATH"), "{e:#}");
        assert!(f.json(".claude.json")["mcpServers"]["relay"].is_object());
    }

    #[test]
    fn agy_not_on_path_is_an_error_where_it_is_needed() {
        let f = Fixture::new();
        let e = f.try_run(&["antigravity"], false, &["antigravity"]).unwrap_err();
        assert!(format!("{e:#}").contains("not on PATH"), "{e:#}");
        let lines = f.try_run(&[], false, &["antigravity"]).unwrap();
        assert!(lines.iter().all(|l| !l.starts_with("antigravity")), "{lines:?}");
        assert!(!f.dir.join(AGY_FILE).exists());
    }

    #[test]
    fn a_moved_rl_is_brought_up_to_date_in_agy() {
        let f = Fixture::new();
        f.run(&["antigravity"], false);
        let lines = run(
            &f.home,
            &["antigravity".into()],
            false,
            Path::new("/new/rl"),
            &|k| f.env(k),
            &|p: &Profile| bin(p, &[]),
        )
        .unwrap();
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert_eq!(f.json(AGY_FILE)["mcpServers"]["relay"]["command"], "/new/rl");
        assert_eq!(f.agy_calls()[1], "mcp add relay -- /new/rl mcp --harness antigravity");
    }

    #[test]
    fn a_disabled_relay_in_agy_is_turned_back_on() {
        let f = Fixture::new();
        f.put(
            AGY_FILE,
            &json!({"mcpServers": {"relay": {"args": args("antigravity"), "command": RL, "disabled": true}}})
                .to_string(),
        );
        let lines = f.run(&["antigravity"], false);
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert_eq!(f.json(AGY_FILE)["mcpServers"]["relay"]["disabled"], false);
    }

    #[test]
    fn without_home_it_says_so() {
        let f = Fixture::new();
        let e = run(
            &f.home,
            &["claude".into()],
            false,
            Path::new(RL),
            &|_| None,
            &|p: &Profile| bin(p, &[]),
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("HOME is not set"), "{e:#}");
    }

    #[test]
    fn a_broken_profile_is_an_error() {
        let f = Fixture::new();
        f.put(".relay/detect/claude.yaml", "bins: [unclosed\n");
        let e = f.try_run(&[], false, &[]).unwrap_err();
        assert!(format!("{e:#}").contains("claude.yaml"), "{e:#}");
        assert!(!f.dir.join(".claude").exists());
    }

    #[test]
    fn only_the_named_harness() {
        let f = Fixture::new();
        let lines = f.run(&["codex"], false);
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert!(!f.dir.join(".claude.json").exists() && !f.dir.join(".gemini").exists());
        assert!(f.claude_calls().is_empty());
        assert!(f.dir.join(".codex/config.toml").exists());
    }

    #[test]
    fn follows_claude_config_dir_and_codex_home() {
        let f = Fixture::new();
        let other = f.dir.join("elsewhere");
        let r = run(
            &f.home,
            &["claude".into(), "codex".into()],
            false,
            Path::new(RL),
            &|k| match k {
                "CLAUDE_CONFIG_DIR" => Some(other.join("c").into()),
                "CODEX_HOME" => Some(other.join("x").into()),
                k => f.env(k),
            },
            &|p: &Profile| bin(p, &[]),
        )
        .unwrap();
        assert_eq!(changes(&r), 2, "{r:?}");
        assert!(other.join("c/.claude.json").exists() && other.join("c/settings.json").exists());
        assert!(other.join("x/config.toml").exists() && other.join("x/hooks.json").exists());
        assert!(!f.dir.join(".claude.json").exists() && !f.dir.join(".codex").exists());
    }

    #[test]
    fn a_file_that_does_not_read_is_left_alone() {
        let f = Fixture::new();
        f.put(".claude.json", "{ not json");
        assert!(f.try_run(&["claude"], false, &[]).is_err());
        assert_eq!(
            std::fs::read_to_string(f.dir.join(".claude.json")).unwrap(),
            "{ not json"
        );
        assert!(f.claude_calls().is_empty());
    }

    #[test]
    fn an_unknown_harness_is_an_error() {
        let f = Fixture::new();
        assert!(f.try_run(&["nope"], false, &[]).is_err());
    }

    #[test]
    fn a_moved_rl_is_brought_up_to_date() {
        let f = Fixture::new();
        f.run(&["claude"], false);
        let lines = run(
            &f.home,
            &["claude".into()],
            false,
            Path::new("/new/rl"),
            &|k| f.env(k),
            &|p: &Profile| bin(p, &[]),
        )
        .unwrap();
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert_eq!(f.json(".claude.json")["mcpServers"]["relay"]["command"], "/new/rl");
        let calls = f.claude_calls();
        assert_eq!(calls[2], REMOVE, "{calls:?}");
        assert!(calls[3].ends_with("-- /new/rl mcp --harness claude"), "{calls:?}");
    }
}
