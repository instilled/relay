//! `rl setup`: for each harness, its user hooks (`rl_detect::user_hooks`)
//! and the `rl mcp` server registered at user level, in the harness's own
//! files. Edits keep everything else in them; a file that does not read is
//! left as it was. `--remove` takes out only Relay's.

use crate::config::Config;
use anyhow::{Context as _, Result, bail};
use rl_core::Home;
use rl_detect::registry::{Profile, find_bin};
use rl_detect::sessions::UserHooks;
use rl_detect::{Registry, user_hooks};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// The name the server goes by in each harness's config.
const SERVER: &str = "relay";

/// `rl setup` for the harnesses named, or every agent whose binary
/// `on_path`. `env` is how variables are read, so a test can stand in
/// another home. Returns one line per harness that changed; `nothing to
/// change` when none did.
pub fn run(
    home: &Home,
    only: &[String],
    remove: bool,
    rl_bin: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
    on_path: &impl Fn(&Profile) -> bool,
) -> Result<Vec<String>> {
    let registry = Registry::load(home).unwrap_or_else(|_| Registry::builtin());
    let profiles: Vec<&Profile> = if only.is_empty() {
        registry
            .agents()
            .filter(|p| registry.user_hooks(&p.name).is_some() && on_path(p))
            .collect()
    } else {
        only.iter()
            .map(|n| match registry.profile(n) {
                Some(p) if registry.user_hooks(n).is_some() => Ok(p),
                _ => bail!("{n}: no such harness to set up"),
            })
            .collect::<Result<_>>()?
    };
    let mut lines = Vec::new();
    for p in &profiles {
        let hooks = registry.user_hooks(&p.name).expect("filtered above");
        lines.extend(one(p, hooks, remove, rl_bin, env)?);
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
        find_bin(&p.bins).is_some()
    })?;
    for l in lines {
        println!("{l}");
    }
    Ok(())
}

/// One harness: its hooks, then its MCP entry. A line naming what changed.
fn one(
    p: &Profile,
    hooks: &UserHooks,
    remove: bool,
    rl_bin: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Vec<String>> {
    let name = &p.name;
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    let hooks_file = hooks
        .file(env)
        .with_context(|| format!("{name}: where its hooks file is"))?;
    let hooks_changed = if remove {
        user_hooks::uninstall(&hooks_file, hooks).with_context(|| name.clone())?
    } else {
        let c = user_hooks::install(&hooks_file, hooks, rl_bin).with_context(|| name.clone())?;
        if c && let Some(note) = &hooks.after_install {
            notes.push(format!("{name}: {note}"));
        }
        c
    };
    if hooks_changed {
        parts.push(format!("hooks {}", if remove { "removed" } else { "installed" }));
    }
    if let Some(mcp) = Mcp::of(name, &hooks_file, env) {
        let changed = if remove { mcp.remove()? } else { mcp.register(rl_bin)? };
        if changed {
            let word = if remove { "removed" } else { "registered" };
            parts.push(format!("mcp {SERVER} {word} ({})", tilde(&mcp.path, env)));
        }
    }
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![format!("{name}: {}", parts.join(" · "))];
    out.extend(notes);
    Ok(out)
}

/// Where a harness keeps the MCP servers of the user, and in what format.
struct Mcp {
    path: PathBuf,
    format: Format,
}

enum Format {
    /// `mcpServers.<name> = {type: "stdio", command, args}` (Claude's
    /// `~/.claude.json`, where `claude mcp add --scope user` writes the same).
    Claude,
    /// `mcpServers.<name> = {command, args}` in the settings file that holds
    /// the hooks. Gemini's documented format; not checked against a gemini.
    Gemini,
    /// `[mcp_servers.<name>]` with `command` and `args` (Codex's
    /// `config.toml`, beside its hooks file).
    Codex,
}

impl Mcp {
    fn of(harness: &str, hooks_file: &Path, env: &impl Fn(&str) -> Option<OsString>) -> Option<Mcp> {
        let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        match harness {
            // With `CLAUDE_CONFIG_DIR` it keeps the file inside that folder.
            "claude" => Some(Mcp {
                path: match var("CLAUDE_CONFIG_DIR") {
                    Some(dir) => dir.join(".claude.json"),
                    None => var("HOME")?.join(".claude.json"),
                },
                format: Format::Claude,
            }),
            "codex" => Some(Mcp {
                path: hooks_file.with_file_name("config.toml"),
                format: Format::Codex,
            }),
            "gemini" => Some(Mcp {
                path: hooks_file.to_path_buf(),
                format: Format::Gemini,
            }),
            _ => None,
        }
    }

    /// True when the file changed.
    fn register(&self, rl_bin: &Path) -> Result<bool> {
        let command = rl_bin.to_string_lossy();
        match self.format {
            Format::Claude | Format::Gemini => {
                let mut want = Map::new();
                if matches!(self.format, Format::Claude) {
                    want.insert("type".into(), json!("stdio"));
                }
                want.insert("command".into(), json!(command));
                want.insert("args".into(), json!(["mcp"]));
                let want = Value::Object(want);
                self.edit_json(|servers| {
                    if servers.get(SERVER) == Some(&want) {
                        return false;
                    }
                    servers.insert(SERVER.into(), want);
                    true
                })
            }
            Format::Codex => self.edit_toml(|doc| {
                let same = doc
                    .get("mcp_servers")
                    .and_then(|s| s.get(SERVER))
                    .and_then(Item::as_table_like)
                    .is_some_and(|t| {
                        t.len() == 2
                            && t.get("command").and_then(Item::as_str) == Some(&command)
                            && t.get("args")
                                .and_then(Item::as_array)
                                .is_some_and(|a| a.len() == 1 && a.get(0).and_then(|v| v.as_str()) == Some("mcp"))
                    });
                if same {
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
                t.insert("command", value(command.as_ref()));
                t.insert("args", value(Array::from_iter(["mcp"])));
                servers.insert(SERVER, Item::Table(t));
                Ok(true)
            }),
        }
    }

    /// True when the file changed.
    fn remove(&self) -> Result<bool> {
        match self.format {
            Format::Claude | Format::Gemini => self.edit_json_existing(|servers| servers.remove(SERVER).is_some()),
            Format::Codex => self.edit_toml(|doc| {
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

    /// Changes `mcpServers` with `f` (true: it did), creating the file and
    /// the object where they are not there.
    fn edit_json(&self, f: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool> {
        let mut doc = user_hooks::read(&self.path)?.unwrap_or_default();
        let servers = match doc.entry("mcpServers").or_insert_with(|| Value::Object(Map::new())) {
            Value::Object(m) => m,
            _ => bail!("{}: `mcpServers` is not an object; left as it was", self.path.display()),
        };
        if !f(servers) {
            return Ok(false);
        }
        user_hooks::write(&self.path, &doc)?;
        Ok(true)
    }

    /// `edit_json` for taking out: a file or key that is not there stays
    /// not there, and an `mcpServers` left empty goes.
    fn edit_json_existing(&self, f: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool> {
        let Some(mut doc) = user_hooks::read(&self.path)? else {
            return Ok(false);
        };
        let Some(servers) = doc.get_mut("mcpServers") else {
            return Ok(false);
        };
        let Value::Object(servers) = servers else {
            bail!("{}: `mcpServers` is not an object; left as it was", self.path.display());
        };
        if !f(servers) {
            return Ok(false);
        }
        if servers.is_empty() {
            doc.shift_remove("mcpServers");
        }
        user_hooks::write(&self.path, &doc)?;
        Ok(true)
    }

    fn edit_toml(&self, f: impl FnOnce(&mut DocumentMut) -> Result<bool>) -> Result<bool> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        let mut doc: DocumentMut = text
            .parse()
            .with_context(|| format!("{} does not read as TOML; left as it was", self.path.display()))?;
        if !f(&mut doc).with_context(|| self.path.display().to_string())? {
            return Ok(false);
        }
        user_hooks::write_text(&self.path, &doc.to_string())?;
        Ok(true)
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
    use serde_json::{Value, json};
    use std::path::PathBuf;

    const RL: &str = "/opt/relay/bin/rl";

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: Home,
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            let home = Home::at(dir.join("relay"));
            Fixture { _tmp: tmp, home, dir }
        }

        fn env(&self, k: &str) -> Option<OsString> {
            match k {
                "HOME" => Some(self.dir.clone().into()),
                _ => None,
            }
        }

        fn run(&self, only: &[&str], remove: bool) -> Vec<String> {
            let only: Vec<String> = only.iter().map(|s| s.to_string()).collect();
            run(
                &self.home,
                &only,
                remove,
                Path::new(RL),
                &|k| self.env(k),
                &|p: &Profile| ["claude", "codex", "gemini"].contains(&p.name.as_str()),
            )
            .unwrap()
        }

        fn json(&self, rel: &str) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.dir.join(rel)).unwrap()).unwrap()
        }

        fn put(&self, rel: &str, text: &str) {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    /// The harnesses a run changed: its lines naming the MCP entry.
    fn changes(lines: &[String]) -> usize {
        lines.iter().filter(|l| l.contains("mcp relay")).count()
    }

    fn entry() -> Value {
        json!({"type": "stdio", "command": RL, "args": ["mcp"]})
    }

    #[test]
    fn sets_up_all_three() {
        let f = Fixture::new();
        let lines = f.run(&[], false);
        // One line per harness, and the profile's note after codex's.
        assert_eq!(changes(&lines), 3, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("/hooks") && !l.contains("mcp relay")));
        assert!(lines[0].starts_with("claude: hooks installed · mcp relay registered (~/.claude.json)"));
        assert!(lines.iter().any(|l| l.starts_with("codex: hooks installed")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("gemini: hooks installed · mcp relay registered (~/.gemini/settings.json)"))
        );

        assert_eq!(f.json(".claude.json")["mcpServers"]["relay"], entry());
        assert!(f.json(".claude/settings.json")["hooks"].is_object());
        assert!(f.json(".codex/hooks.json")["hooks"].is_object());
        let g = f.json(".gemini/settings.json");
        assert_eq!(g["mcpServers"]["relay"], json!({"command": RL, "args": ["mcp"]}));
        assert!(g["hooks"].is_object());
        let toml = std::fs::read_to_string(f.dir.join(".codex/config.toml")).unwrap();
        let doc: toml_edit::DocumentMut = toml.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["relay"]["command"].as_str(), Some(RL));
        assert_eq!(doc["mcp_servers"]["relay"]["args"].as_array().unwrap().len(), 1);
        assert_eq!(doc["mcp_servers"]["relay"]["args"][0].as_str(), Some("mcp"));
    }

    #[test]
    fn second_run_changes_nothing() {
        let f = Fixture::new();
        f.run(&[], false);
        let before = [
            ".claude.json",
            ".claude/settings.json",
            ".codex/hooks.json",
            ".gemini/settings.json",
        ]
        .map(|p| std::fs::read(f.dir.join(p)).unwrap());
        let toml = std::fs::read(f.dir.join(".codex/config.toml")).unwrap();
        assert_eq!(f.run(&[], false), ["nothing to change"]);
        let after = [
            ".claude.json",
            ".claude/settings.json",
            ".codex/hooks.json",
            ".gemini/settings.json",
        ]
        .map(|p| std::fs::read(f.dir.join(p)).unwrap());
        assert_eq!(before, after);
        assert_eq!(toml, std::fs::read(f.dir.join(".codex/config.toml")).unwrap());
    }

    #[test]
    fn keeps_what_else_is_in_the_files() {
        let f = Fixture::new();
        f.put(
            ".claude.json",
            r#"{"zeta": 1, "mcpServers": {"idea": {"type": "sse", "url": "http://x"}}, "alpha": [1, 2]}"#,
        );
        f.put(
            ".codex/config.toml",
            "# my codex\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        );
        f.put(
            ".gemini/settings.json",
            r#"{"theme": "dark", "mcpServers": {"s": {"command": "y"}}}"#,
        );
        f.run(&[], false);

        let c = f.json(".claude.json");
        assert_eq!(c["mcpServers"]["idea"]["url"], "http://x");
        assert_eq!(c["alpha"], json!([1, 2]));
        let keys: Vec<&String> = c.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["zeta", "mcpServers", "alpha"], "the user's order");
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
            ".claude.json",
            r#"{"mcpServers": {"idea": {"type": "sse", "url": "http://x"}}}"#,
        );
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

        assert_eq!(
            f.json(".claude.json"),
            json!({"mcpServers": {"idea": {"type": "sse", "url": "http://x"}}})
        );
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
    fn only_the_named_harness() {
        let f = Fixture::new();
        let lines = f.run(&["codex"], false);
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert!(!f.dir.join(".claude.json").exists() && !f.dir.join(".gemini").exists());
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
            &|_: &Profile| true,
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
        let err = run(
            &f.home,
            &["claude".into()],
            false,
            Path::new(RL),
            &|k| f.env(k),
            &|_: &Profile| true,
        );
        assert!(err.is_err());
        assert_eq!(
            std::fs::read_to_string(f.dir.join(".claude.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn an_unknown_harness_is_an_error() {
        let f = Fixture::new();
        let r = run(
            &f.home,
            &["nope".into()],
            false,
            Path::new(RL),
            &|k| f.env(k),
            &|_: &Profile| true,
        );
        assert!(r.is_err());
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
            &|_: &Profile| true,
        )
        .unwrap();
        assert_eq!(changes(&lines), 1, "{lines:?}");
        assert_eq!(f.json(".claude.json")["mcpServers"]["relay"]["command"], "/new/rl");
    }
}
