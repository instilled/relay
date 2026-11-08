//! `rl setup`: for each harness, its user hooks (`rl_detect::user_hooks`)
//! and the `rl mcp --harness <h>` server registered at user level. Claude's
//! server goes through `claude mcp`, which owns `~/.claude.json`; Codex's
//! and Gemini's are written into their own files. Edits keep everything
//! else in them; a file that does not read is left as it was. `--remove`
//! takes out only Relay's.

use crate::config::Config;
use anyhow::{Context as _, Result, anyhow, bail};
use rl_core::Home;
use rl_detect::registry::{Profile, find_bin};
use rl_detect::sessions::UserHooks;
use rl_detect::{Registry, user_hooks};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

/// The name the server goes by in each harness's config.
const SERVER: &str = "relay";

/// What Codex hands the server from its own environment: where Relay is,
/// and which held session (and harness) it runs under.
const CODEX_ENV_VARS: [&str; 3] = [rl_core::HOME_ENV, "RELAY_SESSION", crate::spawn::AGENT_ENV];

/// How long a `claude mcp` call may take.
const CLAUDE_WAIT: Duration = Duration::from_secs(30);

/// `rl setup` for the harnesses named, or every agent `bin_of` finds (for
/// `--remove`, every agent with hooks: one whose files hold Relay's entries
/// is cleaned whether or not it is still installed). `env` is how variables
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
    let profiles: Vec<&Profile> = if only.is_empty() {
        registry
            .agents()
            .filter(|p| registry.user_hooks(&p.name).is_some() && (remove || bin_of(p).is_some()))
            .collect()
    } else {
        only.iter()
            .map(|n| match registry.profile(n) {
                Some(p) if registry.user_hooks(n).is_some() => Ok(p),
                _ => bail!("{n}: no such harness to set up"),
            })
            .collect::<Result<_>>()?
    };
    let want = Want {
        rl: rl_bin.to_string_lossy().into_owned(),
        relay_home: relay_home(home, env),
    };
    let mut lines = Vec::new();
    for p in &profiles {
        let hooks = registry.user_hooks(&p.name).expect("filtered above");
        lines.extend(one(p, hooks, remove, &want, env, bin_of(p))?);
    }
    if !remove && only.is_empty() {
        let mut config = Config::load(home)?;
        config.harnesses = profiles.iter().map(|p| p.name.clone()).collect();
        config.save(home)?;
    }
    if lines.is_empty() {
        lines.push("nothing to change".into());
    }
    Ok(lines)
}

/// The `rl setup` of the real process: this binary, the real environment
/// and `PATH`.
pub fn main(home: &Home, only: &[String], remove: bool) -> Result<()> {
    let rl_bin = std::env::current_exe().context("where rl is")?;
    let lines = run(home, only, remove, &rl_bin, &|k| std::env::var_os(k), &|p: &Profile| {
        find_bin(&p.bins)
    })?;
    for l in lines {
        println!("{l}");
    }
    Ok(())
}

/// What every entry runs: this `rl`, and the Relay home where it is not the
/// default one.
struct Want {
    rl: String,
    relay_home: Option<String>,
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

/// The Relay home, unless it is the default (`$HOME/.relay`) the server
/// finds on its own.
fn relay_home(home: &Home, env: &impl Fn(&str) -> Option<OsString>) -> Option<String> {
    let default = env("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".relay"));
    (default.as_deref() != Some(home.path())).then(|| home.path().to_string_lossy().into_owned())
}

/// One harness: its hooks, then its MCP entry. A line naming what changed.
fn one(
    p: &Profile,
    hooks: &UserHooks,
    remove: bool,
    want: &Want,
    env: &impl Fn(&str) -> Option<OsString>,
    bin: Option<PathBuf>,
) -> Result<Vec<String>> {
    let name = &p.name;
    let hooks_file = hooks
        .file(env)
        .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its hooks file is is not known"))?;
    let mcp = Mcp::of(name, &hooks_file, env, bin)?;
    if !remove && let Some(Mcp::Claude { bin: None, .. }) = &mcp {
        bail!("{name}: not on PATH; its MCP server is registered with `claude mcp`, so install it first");
    }
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    let hooks_changed = if remove {
        user_hooks::uninstall(&hooks_file, hooks).with_context(|| name.clone())?
    } else {
        let rl_bin = Path::new(&want.rl);
        let c = user_hooks::install(&hooks_file, hooks, rl_bin).with_context(|| name.clone())?;
        if c && let Some(note) = &hooks.after_install {
            notes.push(format!("{name}: {note}"));
        }
        c
    };
    if hooks_changed {
        parts.push(format!("hooks {}", if remove { "removed" } else { "installed" }));
    }
    if let Some(mcp) = mcp {
        let changed = if remove {
            mcp.remove().with_context(|| name.clone())?
        } else {
            mcp.register(name, want).with_context(|| name.clone())?
        };
        if changed {
            let word = if remove { "removed" } else { "registered" };
            let unverified = match mcp {
                Mcp::Gemini { .. } => " (gemini mcp format unverified)",
                _ => "",
            };
            parts.push(format!("mcp {SERVER} {word} ({}){unverified}", tilde(mcp.path(), env)));
        }
    }
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![format!("{name}: {}", parts.join(" · "))];
    out.extend(notes);
    Ok(out)
}

/// Where a harness keeps the MCP servers of the user, and how they get
/// there.
enum Mcp {
    /// `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` with that set),
    /// read here and written only by `claude mcp add|remove --scope user`.
    Claude { path: PathBuf, bin: Option<PathBuf> },
    /// `mcpServers.<name> = {command, args, env?}` in the settings file
    /// that holds the hooks. Gemini's documented format; not checked
    /// against a gemini.
    Gemini { path: PathBuf },
    /// `[mcp_servers.<name>]` with `command`, `args`, `env_vars` and
    /// `env?` (Codex's `config.toml`, beside its hooks file).
    Codex { path: PathBuf },
}

impl Mcp {
    fn of(
        harness: &str,
        hooks_file: &Path,
        env: &impl Fn(&str) -> Option<OsString>,
        bin: Option<PathBuf>,
    ) -> Result<Option<Mcp>> {
        let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        Ok(match harness {
            "claude" => Some(Mcp::Claude {
                path: match var("CLAUDE_CONFIG_DIR") {
                    Some(dir) => dir.join(".claude.json"),
                    None => var("HOME")
                        .ok_or_else(|| anyhow!("claude: HOME is not set, so where ~/.claude.json is is not known"))?
                        .join(".claude.json"),
                },
                bin,
            }),
            "codex" => Some(Mcp::Codex {
                path: hooks_file.with_file_name("config.toml"),
            }),
            "gemini" => Some(Mcp::Gemini {
                path: hooks_file.to_path_buf(),
            }),
            _ => None,
        })
    }

    fn path(&self) -> &Path {
        match self {
            Mcp::Claude { path, .. } | Mcp::Gemini { path } | Mcp::Codex { path } => path,
        }
    }

    /// True when the entry changed.
    fn register(&self, harness: &str, want: &Want) -> Result<bool> {
        let args = want.args(harness);
        match self {
            Mcp::Claude { path, bin } => {
                let have = claude_entry(path)?;
                if have.as_ref().is_some_and(|h| is_claude_entry(h, want, &args)) {
                    return Ok(false);
                }
                let bin = bin.as_deref().ok_or_else(|| anyhow!("not on PATH"))?;
                let removed = claude(bin, path, &["remove", "--scope", "user", SERVER]);
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
                claude(bin, path, &add)?;
                match claude_entry(path)? {
                    Some(h) if is_claude_entry(&h, want, &args) => Ok(true),
                    _ => bail!(
                        "`claude mcp add` ran, but {} does not hold the relay entry it was asked for",
                        path.display()
                    ),
                }
            }
            Mcp::Gemini { path } => {
                let mut entry = Map::new();
                entry.insert("command".into(), json!(want.rl));
                entry.insert("args".into(), json!(args));
                if want.relay_home.is_some() {
                    entry.insert("env".into(), Value::Object(want.env()));
                }
                let entry = Value::Object(entry);
                edit_json(path, |servers| {
                    if servers.get(SERVER) == Some(&entry) {
                        return false;
                    }
                    servers.insert(SERVER.into(), entry);
                    true
                })
            }
            Mcp::Codex { path } => edit_toml(path, |doc| {
                let mut entry = json!({ "command": want.rl, "args": args, "env_vars": CODEX_ENV_VARS });
                if want.relay_home.is_some() {
                    entry["env"] = Value::Object(want.env());
                }
                let have = doc.get("mcp_servers").and_then(|s| s.get(SERVER)).map(toml_json);
                if have.as_ref() == Some(&entry) {
                    return Ok(false);
                }
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
                t.insert("env_vars", value(Array::from_iter(CODEX_ENV_VARS)));
                if let Some(h) = &want.relay_home {
                    let mut env = InlineTable::new();
                    env.insert(rl_core::HOME_ENV, h.as_str().into());
                    t.insert("env", value(env));
                }
                servers.insert(SERVER, Item::Table(t));
                Ok(true)
            }),
        }
    }

    /// True when the entry was there and is gone.
    fn remove(&self) -> Result<bool> {
        match self {
            Mcp::Claude { path, bin } => {
                if claude_entry(path)?.is_none() {
                    return Ok(false);
                }
                let bin = bin.as_deref().ok_or_else(|| {
                    anyhow!(
                        "relay is registered in {}, but claude is not on PATH to take it out \
                         (`claude mcp remove --scope user relay`)",
                        path.display()
                    )
                })?;
                claude(bin, path, &["remove", "--scope", "user", SERVER])?;
                if claude_entry(path)?.is_some() {
                    bail!("`claude mcp remove` ran, but {} still holds relay", path.display());
                }
                Ok(true)
            }
            Mcp::Gemini { path } => edit_json_existing(path, |servers| servers.remove(SERVER).is_some()),
            Mcp::Codex { path } => edit_toml(path, |doc| {
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

/// The `relay` entry in Claude's file, as it reads; the file is never
/// written here.
fn claude_entry(path: &Path) -> Result<Option<Value>> {
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

/// `claude mcp <words>`, run in the folder of the file it keeps (where a
/// stand-in finds it), within `CLAUDE_WAIT`, in a process group of its own
/// and without the variables of a held session.
fn claude(bin: &Path, file: &Path, words: &[impl AsRef<str>]) -> Result<String> {
    let dir = file.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut argv = vec![bin.to_string_lossy().into_owned(), "mcp".into()];
    argv.extend(words.iter().map(|w| w.as_ref().to_string()));
    rl_detect::adapter::run(&argv, Some(dir), false, CLAUDE_WAIT)
}

/// A TOML item as JSON, so an entry compares by what it says rather than
/// how it is laid out.
fn toml_json(item: &Item) -> Value {
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

fn edit_toml(path: &Path, f: impl FnOnce(&mut DocumentMut) -> Result<bool>) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("{} does not read as TOML; left as it was", path.display()))?;
    if !f(&mut doc).with_context(|| path.display().to_string())? {
        return Ok(false);
    }
    user_hooks::write_text(path, &doc.to_string())?;
    Ok(true)
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
    use serde_json::{Value, json};
    use std::path::PathBuf;

    const RL: &str = "/opt/relay/bin/rl";

    /// The stand-in for `claude` (`fixtures/stand-in-claude-mcp`).
    fn stand_in() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-claude-mcp"))
    }

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

        fn codex(&self) -> Value {
            let text = std::fs::read_to_string(self.dir.join(".codex/config.toml")).unwrap();
            let doc: DocumentMut = text.parse().unwrap();
            toml_json(&doc["mcp_servers"]["relay"])
        }
    }

    /// Claude is the stand-in; the others are never run.
    fn bin(p: &Profile, gone: &[&str]) -> Option<PathBuf> {
        let name = p.name.as_str();
        if gone.contains(&name) || !["claude", "codex", "gemini"].contains(&name) {
            return None;
        }
        Some(if name == "claude" {
            stand_in()
        } else {
            PathBuf::from(format!("/nowhere/{name}"))
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

    #[test]
    fn sets_up_all_three() {
        let f = Fixture::new();
        let lines = f.run(&[], false);
        // One line per harness, and the profile's note after codex's.
        assert_eq!(changes(&lines), 3, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("/hooks") && !l.contains("mcp relay")));
        assert!(lines[0].starts_with("claude: hooks installed · mcp relay registered (~/.claude.json)"));
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
                "env_vars": ["RELAY_HOME", "RELAY_SESSION", "RELAY_SESSION_AGENT"],
            })
        );
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
        ];
        let before = files.map(|p| std::fs::read(f.dir.join(p)).unwrap());
        assert_eq!(f.run(&[], false), ["nothing to change"]);
        assert_eq!(before, files.map(|p| std::fs::read(f.dir.join(p)).unwrap()));
        assert_eq!(f.claude_calls().len(), 2, "claude not run again");
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
        assert_eq!(lines.len(), 3, "{lines:?}");
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
    }

    #[test]
    fn remove_cleans_a_harness_no_longer_on_path() {
        let f = Fixture::new();
        f.run(&[], false);
        let lines = f.try_run(&[], true, &["codex", "gemini"]).unwrap();
        assert_eq!(changes(&lines), 3, "{lines:?}");
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
