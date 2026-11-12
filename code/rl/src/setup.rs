//! `rl setup`: for each harness, its user hooks (`rl_detect::user_hooks`)
//! where it has them, and the `rl mcp --harness <h>` server registered at
//! user level where it has a place for one. Both are the harness's to say
//! (`Harness::user_hooks`, `Harness::mcp`): Claude's server goes through
//! `claude mcp`, which owns `~/.claude.json`, and Antigravity's through `agy
//! mcp`, which owns `~/.gemini/config/mcp_config.json`; Codex's and
//! Gemini's are written into their own files (`rl_detect::mcp_entry`).
//! Last, the keys a harness's profile `settings` names in its own global
//! config (Claude's `leftArrowOpensAgents: false`), written into the file by
//! `rl_detect::settings`: Claude has no command that sets them.
//! Edits keep everything else in them; a file that does not read is left as
//! it was. `--remove` takes out only Relay's hooks and server; the settings
//! stay.
//!
//! Each part answers "would a run change this?" from its files alone
//! (`Place::need`); a run writes only what that names, and `--check`
//! (`check`) stops there, so it never writes nor runs a harness's CLI. The
//! list asks `check` as it starts and offers `/setup`. On a terminal, `rl
//! setup`, the list's first start and `/setup` are the wizard
//! (`tui::wizard`), a view on these: `check` per harness (`Host::need_of`),
//! then `apply` per part.

use crate::config::Config;
use anyhow::{Context as _, Result, anyhow, bail};
use rl_core::Home;
use rl_detect::harness::{self, Harness};
use rl_detect::mcp_entry::{self, SERVER, Want};
use rl_detect::registry::{Profile, find_bin};
use rl_detect::sessions::UserHooks;
use rl_detect::settings;
use rl_detect::{Registry, user_hooks};
use std::ffi::OsString;
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
    // What you declined is never made; taking Relay out takes out all of it.
    let declined = if remove { Vec::new() } else { declined(home)? };
    let mut lines = Vec::new();
    for h in &harnesses {
        lines.extend(one(h.as_ref(), remove, &want, env, &declined)?);
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
    check_with(home, only, rl_bin, env, bin_of, &declined(home)?)
}

/// `check`, leaving out the changes `declined` names.
fn check_with(
    home: &Home,
    only: &[String],
    rl_bin: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
    bin_of: &impl Fn(&Profile) -> Option<PathBuf>,
    declined: &[String],
) -> Result<Vec<Need>> {
    let registry = Registry::load(home)?;
    let want = want(home, rl_bin, env);
    let mut needs = Vec::new();
    for h in select(&registry, only, false, bin_of)? {
        let place = Place::of(h.as_ref(), env)?.without(declined);
        needs.extend(place.need(&want, env)?);
    }
    Ok(needs)
}

/// The changes you declined (`config.json`'s `setup.declined`), by id
/// (`What::id`).
pub fn declined(home: &Home) -> Result<Vec<String>> {
    Ok(Config::load(home)?.setup.declined)
}

/// Keeps what you declined in the wizard, and takes back out what you
/// ticked again.
pub fn set_declined(home: &Home, decline: &[String], accept: &[String]) -> Result<()> {
    let mut config = Config::load(home)?;
    let before = config.setup.declined.clone();
    let d = &mut config.setup.declined;
    d.retain(|id| !accept.contains(id));
    for id in decline {
        if !d.contains(id) {
            d.push(id.clone());
        }
    }
    if config.setup.declined != before || !home.config_json().exists() {
        config.save(home)?;
    }
    Ok(())
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

/// Whether `rl setup` has anything to do for `h`: hooks of its own, a
/// place for the server, or settings of its own to set.
fn set_up(h: &dyn Harness) -> bool {
    h.user_hooks().is_some() || h.has_mcp() || h.profile().settings.is_some()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    Hooks,
    /// The `relay` MCP server's entry.
    Mcp,
    /// A key of the harness's own global config not as Relay sets it: the
    /// key, and what its profile says of that (`← opens agents on`).
    Setting {
        key: String,
        says: String,
    },
}

impl std::fmt::Display for What {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            What::Hooks => f.write_str("hooks"),
            What::Mcp => write!(f, "mcp {SERVER}"),
            What::Setting { says, .. } => f.write_str(says),
        }
    }
}

impl What {
    /// What it is called in `config.json`'s `setup.declined`, for the
    /// harness `h`: `codex/hooks`, `codex/mcp`,
    /// `claude/settings/leftArrowOpensAgents`.
    pub fn id(&self, h: &str) -> String {
        match self {
            What::Hooks => format!("{h}/hooks"),
            What::Mcp => format!("{h}/mcp"),
            What::Setting { key, .. } => format!("{h}/settings/{key}"),
        }
    }
}

/// `hooks missing`, `mcp relay stale (points at /old/rl)`, `← opens agents
/// on`.
impl std::fmt::Display for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let What::Setting { says, .. } = &self.what {
            return f.write_str(says);
        }
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
    let declined = declined(home)?.len();
    if declined > 0 {
        println!("({declined} declined, rl setup to change)");
    }
    // What changed under Relay, not to set up: said, and the exit status
    // stays setup's.
    let drift = crate::drift::lines(&crate::drift::load(&crate::drift::dir(home)));
    if !drift.is_empty() {
        println!("drift:");
        for l in drift {
            println!("{l}");
        }
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

    /// `check` for the one harness `name`: what it needs, if anything —
    /// what you declined too (the wizard shows it, unticked).
    pub fn need_of(&self, home: &Home, name: &str) -> Result<Option<Need>> {
        let only = [name.to_string()];
        Ok(check_with(
            home,
            &only,
            &self.rl_bin,
            &|k| (self.env)(k),
            &|p| (self.bin_of)(p),
            &[],
        )?
        .pop())
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

/// One part of one harness's setup, as `need` named it: what `run` does
/// for it, alone, as the files are now (the wizard runs them one by one,
/// off its screen's thread). What it did, in words: `hooks installed`.
pub fn apply(home: &Home, name: &str, what: &What, host: &Host) -> Result<String> {
    let env = |k: &str| (host.env)(k);
    let registry = Registry::load(home)?;
    let profile = registry
        .profile(name)
        .ok_or_else(|| anyhow!("{name}: no such harness to set up"))?;
    let h = harness::of(profile, (host.bin_of)(profile));
    let place = Place::of(h.as_ref(), &env)?;
    let want = want(home, &host.rl_bin, &env);
    match (what, &place.hooks, &place.mcp, &place.settings) {
        (What::Hooks, Some((hooks, file)), _, _) => {
            user_hooks::install(file, hooks, Path::new(&want.rl)).with_context(|| name.to_string())?;
            Ok("hooks installed".into())
        }
        (What::Mcp, _, Some(mcp), _) => {
            mcp.register(&want).with_context(|| name.to_string())?;
            Ok(format!("mcp {SERVER} registered"))
        }
        (What::Setting { key, .. }, _, _, Some(settings)) => {
            let one = settings::Place {
                path: settings.path.clone(),
                set: settings.set.iter().filter(|s| &s.key == key).cloned().collect(),
            };
            if one.set.is_empty() {
                bail!("{name}: has no setting {key}");
            }
            let set = one.apply().with_context(|| name.to_string())?;
            let words: Vec<String> = set.iter().map(|s| format!("{} set to {}", s.key, s.value)).collect();
            Ok(if words.is_empty() {
                "already set".into()
            } else {
                words.join(", ")
            })
        }
        _ => bail!("{name}: has no {what} to set up"),
    }
}

/// Keeps which harnesses are set up in `config.json`, as `run` does: every
/// agent found that setup has something for. (The wizard's last word.)
pub fn remember(home: &Home, host: &Host) -> Result<()> {
    let registry = Registry::load(home)?;
    let harnesses = select(&registry, &[], false, &|p| (host.bin_of)(p))?;
    let mut config = Config::load(home)?;
    config.harnesses = harnesses.iter().map(|h| h.name().to_string()).collect();
    config.save(home)
}

/// Writes `config.json` where there is none yet, so the first start's
/// setup is not offered again.
pub fn keep_config(home: &Home) -> Result<()> {
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
fn one(
    h: &dyn Harness,
    remove: bool,
    want: &Want,
    env: &impl Fn(&str) -> Option<OsString>,
    declined: &[String],
) -> Result<Vec<String>> {
    let name = h.name();
    let place = Place::of(h, env)?.without(declined);
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
            match (&item.what, &place.hooks, &place.mcp) {
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
    // Last, and as the file is now: a harness's own CLI may have just
    // written the same file.
    if !remove && let Some(settings) = &place.settings {
        for s in settings.apply().with_context(|| name.to_string())? {
            parts.push(format!("{} set to {} ({})", s.key, s.value, tilde(&settings.path, env)));
        }
    }
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![format!("{name}: {}", parts.join(" · "))];
    out.extend(notes);
    Ok(out)
}

/// Where a harness's hooks, MCP server and settings are.
struct Place<'a> {
    name: &'a str,
    hooks: Option<(&'a UserHooks, PathBuf)>,
    mcp: Option<mcp_entry::Place>,
    settings: Option<settings::Place>,
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
        let settings = h.settings(env)?;
        Ok(Place {
            name,
            hooks,
            mcp,
            settings,
        })
    }

    /// The same, without what `declined` names (`What::id`).
    fn without(mut self, declined: &[String]) -> Place<'a> {
        let name = self.name;
        let out = |w: &What| declined.contains(&w.id(name));
        if out(&What::Hooks) {
            self.hooks = None;
        }
        if out(&What::Mcp) {
            self.mcp = None;
        }
        if let Some(s) = &mut self.settings {
            s.set.retain(|x| {
                !out(&What::Setting {
                    key: x.key.clone(),
                    says: String::new(),
                })
            });
            if s.set.is_empty() {
                self.settings = None;
            }
        }
        self
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
        if let Some(settings) = &self.settings {
            for s in settings.need().with_context(|| name.to_string())? {
                items.push(Part {
                    what: What::Setting {
                        key: s.key.clone(),
                        says: s.check.clone(),
                    },
                    state: State::Missing,
                    file: tilde(&settings.path, env),
                });
            }
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
            lines.contains(
                &"claude: hooks installed · mcp relay registered (~/.claude.json) · \
                  leftArrowOpensAgents set to false (~/.claude.json)"
                    .to_string()
            ),
            "{lines:?}"
        );
        assert_eq!(f.json(".claude.json")["leftArrowOpensAgents"], json!(false));
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
                "claude: hooks missing · mcp relay missing · ← opens agents on",
                "codex: hooks missing · mcp relay missing",
                "gemini: hooks missing · mcp relay missing",
            ]
        );
        assert_eq!(f.files(), before, "nothing written");
        let claude = &f.try_check(&["claude"], RL).unwrap()[0];
        assert_eq!(
            claude.touches(),
            "claude: ~/.claude/settings.json (hooks) · ~/.claude.json (mcp relay, ← opens agents on)"
        );
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
    fn apply_does_one_part_at_a_time_as_check_named_it() {
        let f = Fixture::new();
        let host = f.host();
        let codex = host.need_of(&f.home, "codex").unwrap().unwrap();
        assert_eq!(codex.items.len(), 2);
        assert_eq!(apply(&f.home, "codex", &What::Hooks, &host).unwrap(), "hooks installed");
        let left = host.need_of(&f.home, "codex").unwrap().unwrap();
        assert_eq!(
            left.items.iter().map(|p| p.what.clone()).collect::<Vec<_>>(),
            [What::Mcp]
        );
        assert_eq!(
            apply(&f.home, "codex", &What::Mcp, &host).unwrap(),
            "mcp relay registered"
        );
        assert_eq!(host.need_of(&f.home, "codex").unwrap(), None);

        let claude = host.need_of(&f.home, "claude").unwrap().unwrap();
        let setting = claude.items.last().unwrap().what.clone();
        assert_eq!(
            apply(&f.home, "claude", &setting, &host).unwrap(),
            "leftArrowOpensAgents set to false"
        );
        assert!(f.claude_calls().is_empty(), "a setting runs no claude");
        assert!(
            apply(
                &f.home,
                "codex",
                &What::Setting {
                    key: "x".into(),
                    says: "x".into()
                },
                &host
            )
            .is_err()
        );

        assert!(!f.home.config_json().exists());
        remember(&f.home, &host).unwrap();
        assert!(f.home.config_json().exists());
    }

    #[test]
    fn a_declined_change_is_neither_made_nor_named_until_ticked_again() {
        let f = Fixture::new();
        let left = "claude/settings/leftArrowOpensAgents".to_string();
        set_declined(&f.home, std::slice::from_ref(&left), &[]).unwrap();
        assert_eq!(declined(&f.home).unwrap(), std::slice::from_ref(&left));
        assert!(
            f.check(RL)
                .contains(&"claude: hooks missing · mcp relay missing".to_string()),
            "{:?}",
            f.check(RL)
        );
        f.run(&[], false);
        assert!(f.check(RL).is_empty(), "set up, but for what was declined");
        assert!(f.json(".claude.json").get("leftArrowOpensAgents").is_none());
        // The wizard still sees it, to show it unticked.
        let all = f.host().need_of(&f.home, "claude").unwrap().unwrap();
        assert_eq!(all.items.len(), 1);
        assert_eq!(all.items[0].what.id("claude"), left);

        set_declined(&f.home, &[], std::slice::from_ref(&left)).unwrap();
        assert!(declined(&f.home).unwrap().is_empty());
        assert_eq!(f.check(RL), ["claude: ← opens agents on"]);
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
    fn claudes_left_arrow_is_turned_off_keeping_the_rest_and_left_off_on_remove() {
        let f = Fixture::new();
        f.run(&["claude"], false);
        // As the real `claude` leaves it: its own keys, the shortcut on.
        let mut doc = f.json(".claude.json");
        doc["numStartups"] = json!(7);
        doc["leftArrowOpensAgents"] = json!(true);
        f.put(".claude.json", &doc.to_string());
        let check = |f: &Fixture| -> Vec<String> {
            f.try_check(&["claude"], RL)
                .unwrap()
                .iter()
                .map(Need::to_string)
                .collect()
        };
        assert_eq!(check(&f), ["claude: ← opens agents on"]);
        assert_eq!(
            f.run(&["claude"], false),
            ["claude: leftArrowOpensAgents set to false (~/.claude.json)"]
        );
        let doc = f.json(".claude.json");
        assert_eq!(doc["leftArrowOpensAgents"], json!(false));
        assert_eq!(doc["numStartups"], json!(7), "the rest kept");
        assert_eq!(doc["mcpServers"]["relay"]["command"], json!(RL));
        assert!(check(&f).is_empty());
        assert_eq!(
            f.claude_calls().len(),
            2,
            "no claude run for it: one remove, one add, the first time"
        );
        // Taking Relay out leaves it: it is yours by then. (The stand-in
        // `claude mcp remove` keeps no other key; the real one does.)
        let removed = f.run(&["claude"], true);
        assert_eq!(removed, ["claude: hooks removed · mcp relay removed (~/.claude.json)"]);
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
        let global: Value =
            serde_json::from_str(&std::fs::read_to_string(other.join("c/.claude.json")).unwrap()).unwrap();
        assert_eq!(
            global["leftArrowOpensAgents"],
            json!(false),
            "its global config there too"
        );
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
