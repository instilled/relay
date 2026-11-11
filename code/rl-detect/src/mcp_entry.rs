//! Relay's MCP server (`rl mcp --harness <h>`) as an entry in a harness's
//! own list of the user's servers: where that list is, how the entry reads
//! against what `rl setup` would write, and how it gets there and goes. Each
//! harness says which of these is its (`Harness::mcp`); `rl setup` asks it.
//!
//! - Claude's `~/.claude.json`, written only by `claude mcp add|remove
//!   --scope user` (`How::ClaudeCli`).
//! - A harness CLI that owns its file and adds and removes entries itself
//!   (`How::Cli`: `agy mcp add|remove`).
//! - `mcpServers.relay = {command, args, env?}` in a JSON settings file Relay
//!   writes itself (`How::Json`: Gemini).
//! - `[mcp_servers.relay]` with `command`, `args`, `env_vars` and `env?` in a
//!   TOML file (`How::Toml`: Codex's `config.toml`).
//!
//! Edits keep everything else in a file; one that does not read is left as
//! it was. `need` reads the file alone: it never writes nor runs a CLI.

use crate::user_hooks;
use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

/// The name the server goes by in each harness's config.
pub const SERVER: &str = "relay";

/// What a TOML entry hands the server from the harness's own environment:
/// where Relay is, and which held session (and harness) it runs under.
pub const TOML_ENV_VARS: [&str; 3] = [rl_core::HOME_ENV, rl_core::SESSION_ENV, rl_core::AGENT_ENV];

/// How long a harness's `mcp` CLI call may take.
const MCP_CLI_WAIT: Duration = Duration::from_secs(30);

/// What every entry runs: this `rl`, and the Relay home where it is not the
/// default one.
#[derive(Debug, Clone)]
pub struct Want {
    pub rl: String,
    pub relay_home: Option<String>,
}

impl Want {
    fn args(&self, harness: &str) -> Vec<String> {
        vec!["mcp".into(), "--harness".into(), harness.into()]
    }

    /// The environment the server is given: `RELAY_HOME`, where needed.
    fn env(&self) -> Map<String, Value> {
        let mut env = Map::new();
        if let Some(h) = &self.relay_home {
            env.insert(rl_core::HOME_ENV.into(), json!(h));
        }
        env
    }
}

/// How an entry stands against what a run would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// None of Relay's there.
    Missing,
    /// There, but not what a run would write: `was`, the `rl` it points at
    /// where that is another one.
    Stale { was: Option<String> },
}

/// Where a harness keeps the MCP servers of the user, and how Relay's gets
/// there.
#[derive(Debug, Clone)]
pub struct Place {
    /// The harness, as the entry's `--harness` names it.
    pub harness: String,
    pub path: PathBuf,
    pub how: How,
    /// Said beside what a run did (`gemini mcp format unverified`).
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub enum How {
    /// Read here, written only by `<cli> mcp add|remove --scope user`
    /// (Claude: the entry is `{type?, command, args, env}`).
    ClaudeCli { cli: String, bin: Option<PathBuf> },
    /// Read here, written only by `<cli> <words> add|remove` (agy: `add`
    /// replaces; entries of `{args, command, disabled, env?}`).
    Cli {
        cli: String,
        bin: Option<PathBuf>,
        words: Vec<String>,
    },
    /// `mcpServers.<name> = {command, args, env?}`, written here.
    Json,
    /// `[mcp_servers.<name>]` with `command`, `args`, `env_vars`, `env?`.
    Toml,
}

impl Place {
    /// The harness's own command that registers the server (`claude mcp`),
    /// where one is needed and its binary is not on PATH.
    pub fn cli_missing(&self) -> Option<String> {
        match &self.how {
            How::ClaudeCli { cli, bin: None } => Some(format!("{cli} mcp")),
            How::Cli { cli, bin: None, words } => Some(format!("{cli} {}", words.join(" "))),
            _ => None,
        }
    }

    /// How the entry stands against what `register` writes, read from the
    /// file alone: None when it is that already. An error where `register`
    /// would refuse the file.
    pub fn need(&self, want: &Want) -> Result<Option<State>> {
        let args = want.args(&self.harness);
        let path = &self.path;
        let stale = |have: &Value| State::Stale {
            was: have
                .get("command")
                .and_then(Value::as_str)
                .filter(|c| *c != want.rl)
                .map(String::from),
        };
        Ok(match &self.how {
            How::ClaudeCli { .. } | How::Cli { .. } => {
                let fits = |h: &Value| match self.how {
                    How::ClaudeCli { .. } => is_claude_entry(h, want, &args),
                    _ => is_cli_entry(h, want, &args),
                };
                match cli_entry(path)? {
                    None => Some(State::Missing),
                    Some(h) if fits(&h) => None,
                    Some(h) => Some(stale(&h)),
                }
            }
            How::Json => {
                let doc = user_hooks::read(path)?.unwrap_or_default();
                let have = match doc.get("mcpServers") {
                    None => None,
                    Some(Value::Object(servers)) => servers.get(SERVER),
                    Some(_) => bail!("{}: `mcpServers` is not an object; left as it was", path.display()),
                };
                match have {
                    None => Some(State::Missing),
                    Some(h) if *h == json_entry(want, &args) => None,
                    Some(h) => Some(stale(h)),
                }
            }
            How::Toml => {
                let doc = read_toml(path)?;
                let servers = doc.get("mcp_servers");
                match servers.and_then(|s| s.get(SERVER)).map(toml_json) {
                    Some(h) if h == toml_entry(want, &args) => None,
                    _ if servers.is_some_and(|s| !s.is_table()) => {
                        bail!("{}: `mcp_servers` is not a table; left as it was", path.display())
                    }
                    None => Some(State::Missing),
                    Some(h) => Some(stale(&h)),
                }
            }
        })
    }

    /// Writes the entry over whatever is there (`need` says whether to).
    pub fn register(&self, want: &Want) -> Result<()> {
        let args = want.args(&self.harness);
        let path = &self.path;
        match &self.how {
            How::ClaudeCli { cli, bin } => {
                let have = cli_entry(path)?;
                let bin = bin.as_deref().ok_or_else(|| anyhow!("not on PATH"))?;
                let mcp = |w: &[String]| mcp_cli(bin, &["mcp".to_string()], path, w);
                let removed = mcp(&["remove", "--scope", "user", SERVER].map(String::from));
                if let Err(e) = removed
                    && have.is_some()
                {
                    return Err(e.context("taking out the relay entry there is"));
                }
                let mut add: Vec<String> = ["add", "--scope", "user", SERVER].map(String::from).into();
                if let Some(h) = &want.relay_home {
                    add.extend(["-e".into(), format!("{}={h}", rl_core::HOME_ENV)]);
                }
                add.extend(["--".into(), want.rl.clone()]);
                add.extend(args.iter().cloned());
                mcp(&add)?;
                match cli_entry(path)? {
                    Some(h) if is_claude_entry(&h, want, &args) => Ok(()),
                    _ => bail!(
                        "`{cli} mcp add` ran, but {} does not hold the relay entry it was asked for",
                        path.display()
                    ),
                }
            }
            How::Cli { cli, bin, words } => {
                let bin = bin.as_deref().ok_or_else(|| anyhow!("not on PATH"))?;
                // `add` replaces an entry of the name: no `remove` first.
                let mut add: Vec<String> = vec!["add".into()];
                if let Some(h) = &want.relay_home {
                    add.extend(["-e".into(), format!("{}={h}", rl_core::HOME_ENV)]);
                }
                add.extend([SERVER.into(), "--".into(), want.rl.clone()]);
                add.extend(args.iter().cloned());
                mcp_cli(bin, words, path, &add)?;
                match cli_entry(path)? {
                    Some(h) if is_cli_entry(&h, want, &args) => Ok(()),
                    _ => bail!(
                        "`{cli} {} add` ran, but {} does not hold the relay entry it was asked for",
                        words.join(" "),
                        path.display()
                    ),
                }
            }
            How::Json => {
                let entry = json_entry(want, &args);
                edit_json(path, |servers| {
                    servers.insert(SERVER.into(), entry);
                    true
                })
                .map(drop)
            }
            How::Toml => edit_toml(path, |doc| {
                let servers = match doc.entry("mcp_servers").or_insert_with(|| {
                    let mut t = Table::new();
                    t.set_implicit(true);
                    Item::Table(t)
                }) {
                    Item::Table(t) => t,
                    _ => bail!("`mcp_servers` is not a table; left as it was"),
                };
                let mut t = Table::new();
                t.insert("command", value(want.rl.as_str()));
                t.insert("args", value(Array::from_iter(args.iter().map(String::as_str))));
                t.insert("env_vars", value(Array::from_iter(TOML_ENV_VARS)));
                if let Some(h) = &want.relay_home {
                    let mut env = InlineTable::new();
                    env.insert(rl_core::HOME_ENV, h.as_str().into());
                    t.insert("env", value(env));
                }
                servers.insert(SERVER, Item::Table(t));
                Ok(true)
            })
            .map(drop),
        }
    }

    /// True when the entry was there and is gone.
    pub fn remove(&self) -> Result<bool> {
        let path = &self.path;
        match &self.how {
            How::ClaudeCli { cli, bin } => {
                if cli_entry(path)?.is_none() {
                    return Ok(false);
                }
                let bin = bin.as_deref().ok_or_else(|| {
                    anyhow!(
                        "relay is registered in {}, but {cli} is not on PATH to take it out \
                         (`{cli} mcp remove --scope user relay`)",
                        path.display()
                    )
                })?;
                mcp_cli(bin, &["mcp".to_string()], path, &["remove", "--scope", "user", SERVER])?;
                if cli_entry(path)?.is_some() {
                    bail!("`{cli} mcp remove` ran, but {} still holds relay", path.display());
                }
                Ok(true)
            }
            How::Cli { cli, bin, words } => {
                if cli_entry(path)?.is_none() {
                    return Ok(false);
                }
                let words_ = words.join(" ");
                let bin = bin.as_deref().ok_or_else(|| {
                    anyhow!(
                        "relay is registered in {}, but {cli} is not on PATH to take it out (`{cli} {words_} remove relay`)",
                        path.display()
                    )
                })?;
                mcp_cli(bin, words, path, &["remove", SERVER])?;
                if cli_entry(path)?.is_some() {
                    bail!("`{cli} {words_} remove` ran, but {} still holds relay", path.display());
                }
                Ok(true)
            }
            How::Json => edit_json_existing(path, |servers| servers.remove(SERVER).is_some()),
            How::Toml => edit_toml(path, |doc| {
                let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
                    return Ok(false);
                };
                let had = servers.remove(SERVER).is_some();
                if had && servers.is_empty() {
                    doc.remove("mcp_servers");
                }
                Ok(had)
            }),
        }
    }
}

/// What a JSON `mcpServers.relay` is to be: this `rl` with these `args`,
/// and `env` where there is one.
fn json_entry(want: &Want, args: &[String]) -> Value {
    let mut entry = Map::new();
    entry.insert("command".into(), json!(want.rl));
    entry.insert("args".into(), json!(args));
    if want.relay_home.is_some() {
        entry.insert("env".into(), Value::Object(want.env()));
    }
    Value::Object(entry)
}

/// What a TOML `[mcp_servers.relay]` is to be, as JSON (`toml_json`).
fn toml_entry(want: &Want, args: &[String]) -> Value {
    let mut entry = json!({ "command": want.rl, "args": args, "env_vars": TOML_ENV_VARS });
    if want.relay_home.is_some() {
        entry["env"] = Value::Object(want.env());
    }
    entry
}

/// The `relay` entry in a file a harness's CLI keeps, as it reads; the file
/// is never written here.
fn cli_entry(path: &Path) -> Result<Option<Value>> {
    Ok(user_hooks::read(path)?.and_then(|doc| doc.get("mcpServers")?.get(SERVER).cloned()))
}

/// Whether `have` is what `claude mcp add` writes for `want`: a stdio
/// server of this `rl` with these `args` and environment, nothing else.
fn is_claude_entry(have: &Value, want: &Want, args: &[String]) -> bool {
    let Some(o) = have.as_object() else { return false };
    o.keys()
        .all(|k| ["type", "command", "args", "env"].contains(&k.as_str()))
        && o.get("type").is_none_or(|t| t == "stdio")
        && o.get("command") == Some(&json!(want.rl))
        && o.get("args") == Some(&json!(args))
        && o.get("env").cloned().unwrap_or_else(|| json!({})) == Value::Object(want.env())
}

/// Whether `have` is what `agy mcp add` writes for `want`, and is on: this
/// `rl` with these `args` and environment.
fn is_cli_entry(have: &Value, want: &Want, args: &[String]) -> bool {
    let Some(o) = have.as_object() else { return false };
    o.keys()
        .all(|k| ["type", "command", "args", "env", "disabled"].contains(&k.as_str()))
        && o.get("type").is_none_or(|t| t == "stdio")
        && o.get("disabled").is_none_or(|d| d == false)
        && o.get("command") == Some(&json!(want.rl))
        && o.get("args") == Some(&json!(args))
        && o.get("env").cloned().unwrap_or_else(|| json!({})) == Value::Object(want.env())
}

/// `<bin> <before> <words>` (`claude mcp …`, `agy mcp …`), run in the folder
/// of the file it keeps (where a stand-in finds it), within `MCP_CLI_WAIT`,
/// in a process group of its own and without the variables of a held
/// session.
fn mcp_cli(bin: &Path, before: &[String], file: &Path, words: &[impl AsRef<str>]) -> Result<String> {
    let dir = file.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut argv = vec![bin.to_string_lossy().into_owned()];
    argv.extend(before.iter().cloned());
    argv.extend(words.iter().map(|w| w.as_ref().to_string()));
    crate::harness::run(&argv, Some(dir), false, MCP_CLI_WAIT)
}

/// A TOML item as JSON, so an entry compares by what it says rather than
/// how it is laid out.
pub fn toml_json(item: &Item) -> Value {
    fn of(v: &toml_edit::Value) -> Value {
        match v {
            toml_edit::Value::String(s) => json!(s.value()),
            toml_edit::Value::Array(a) => Value::Array(a.iter().map(of).collect()),
            toml_edit::Value::InlineTable(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), of(v))).collect()),
            other => json!(other.to_string().trim()),
        }
    }
    match item {
        Item::Value(v) => of(v),
        Item::Table(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), toml_json(v))).collect()),
        Item::ArrayOfTables(a) => Value::Array(a.iter().map(|t| toml_json(&Item::Table(t.clone()))).collect()),
        Item::None => Value::Null,
    }
}

/// Changes `mcpServers` with `f` (true: it did), creating the file and the
/// object where they are not there.
fn edit_json(path: &Path, f: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool> {
    let mut doc = user_hooks::read(path)?.unwrap_or_default();
    let servers = match doc.entry("mcpServers").or_insert_with(|| Value::Object(Map::new())) {
        Value::Object(m) => m,
        _ => bail!("{}: `mcpServers` is not an object; left as it was", path.display()),
    };
    if !f(servers) {
        return Ok(false);
    }
    user_hooks::write(path, &doc)?;
    Ok(true)
}

/// `edit_json` for taking out: a file or key that is not there stays not
/// there, and an `mcpServers` left empty goes.
fn edit_json_existing(path: &Path, f: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool> {
    let Some(mut doc) = user_hooks::read(path)? else {
        return Ok(false);
    };
    let Some(servers) = doc.get_mut("mcpServers") else {
        return Ok(false);
    };
    let Value::Object(servers) = servers else {
        bail!("{}: `mcpServers` is not an object; left as it was", path.display());
    };
    if !f(servers) {
        return Ok(false);
    }
    if servers.is_empty() {
        doc.shift_remove("mcpServers");
    }
    user_hooks::write(path, &doc)?;
    Ok(true)
}

/// The TOML file as it reads; empty where it is not there.
fn read_toml(path: &Path) -> Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    text.parse()
        .with_context(|| format!("{} does not read as TOML; left as it was", path.display()))
}

fn edit_toml(path: &Path, f: impl FnOnce(&mut DocumentMut) -> Result<bool>) -> Result<bool> {
    let mut doc = read_toml(path)?;
    if !f(&mut doc).with_context(|| path.display().to_string())? {
        return Ok(false);
    }
    user_hooks::write_text(path, &doc.to_string())?;
    Ok(true)
}
