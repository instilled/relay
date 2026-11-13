//! What `rl setup` does for one harness, part by part (`Harness::setup`):
//! its user hooks (`user_hooks`), Relay's MCP server in its own list of
//! servers (`mcp_entry`), and each key its profile's `settings` names in its
//! own global config (`settings`). Each part says how it stands against what
//! a run would leave (`need`, from its file alone: nothing written, no CLI
//! run), puts it there (`apply`), and takes Relay's out again (`remove`).

use crate::mcp_entry::{self, State, Want};
use crate::sessions::UserHooks;
use crate::{settings, user_hooks};
use anyhow::{Result, anyhow};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What a part is, as `rl setup` names it.
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
            What::Mcp => write!(f, "mcp {}", mcp_entry::SERVER),
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

/// One part of a harness's setup, at the file it goes in.
#[derive(Debug, Clone)]
pub enum Part {
    /// Its user-level hooks, in this file.
    Hooks { hooks: UserHooks, file: PathBuf },
    /// Relay's MCP server, as its harness keeps it.
    Mcp(mcp_entry::Place),
    /// One key its profile's `settings` names, in this file.
    Setting { path: PathBuf, setting: settings::Setting },
}

impl Part {
    pub fn what(&self) -> What {
        match self {
            Part::Hooks { .. } => What::Hooks,
            Part::Mcp(_) => What::Mcp,
            Part::Setting { setting, .. } => What::Setting {
                key: setting.key.clone(),
                says: setting.check.clone(),
            },
        }
    }

    /// The file it is in.
    pub fn file(&self) -> &Path {
        match self {
            Part::Hooks { file, .. } => file,
            Part::Mcp(m) => &m.path,
            Part::Setting { path, .. } => path,
        }
    }

    /// Said beside what a run did with it (`gemini mcp format unverified`).
    pub fn note(&self) -> Option<&str> {
        match self {
            Part::Mcp(m) => m.note.as_deref(),
            _ => None,
        }
    }

    /// What the harness asks of you once it is put there, if anything.
    pub fn after_install(&self) -> Option<&str> {
        match self {
            Part::Hooks { hooks, .. } => hooks.after_install.as_deref(),
            _ => None,
        }
    }

    /// The harness's own command a run needs, where it is not on PATH
    /// (`claude mcp`).
    pub fn cli_missing(&self) -> Option<String> {
        match self {
            Part::Mcp(m) => m.cli_missing(),
            _ => None,
        }
    }

    /// How it stands against what `apply` would leave, from its file alone:
    /// None where it is so already. An error where `apply` would refuse the
    /// file.
    pub fn need(&self, want: &Want) -> Result<Option<State>> {
        match self {
            Part::Hooks { hooks, file } => {
                let rl = Path::new(&want.rl);
                if !user_hooks::install_needed(file, hooks, rl)? {
                    return Ok(None);
                }
                Ok(Some(if user_hooks::installed(file, hooks)? {
                    State::Stale {
                        was: user_hooks::programs(file, hooks)?.into_iter().find(|p| *p != want.rl),
                    }
                } else {
                    State::Missing
                }))
            }
            Part::Mcp(m) => m.need(want),
            Part::Setting { path, setting } => Ok((!one(path, setting).need()?.is_empty()).then_some(State::Missing)),
        }
    }

    /// Puts it there, as its file is now. Whether anything changed.
    pub fn apply(&self, want: &Want) -> Result<bool> {
        match self {
            Part::Hooks { hooks, file } => {
                user_hooks::install(file, hooks, Path::new(&want.rl))?;
                Ok(true)
            }
            Part::Mcp(m) => m.register(want).map(|()| true),
            Part::Setting { path, setting } => Ok(!one(path, setting).apply()?.is_empty()),
        }
    }

    /// Takes Relay's out: its hooks, its server. A setting stays: it is
    /// yours by then. Whether anything changed.
    pub fn remove(&self) -> Result<bool> {
        match self {
            Part::Hooks { hooks, file } => user_hooks::uninstall(file, hooks),
            Part::Mcp(m) => m.remove(),
            Part::Setting { .. } => Ok(false),
        }
    }

    /// The value a setting is set to, for words: `false`.
    pub fn value(&self) -> Option<String> {
        match self {
            Part::Setting { setting, .. } => Some(setting.value.to_string()),
            _ => None,
        }
    }
}

fn one(path: &Path, setting: &settings::Setting) -> settings::Place {
    settings::Place {
        path: path.to_path_buf(),
        set: vec![setting.clone()],
    }
}

/// The parts of the harness `name`: its `hooks` in their file, its MCP
/// `place`, its `settings` key by key — each in its file as `env` finds it.
/// An error where one's file is not known (`HOME` not set).
pub fn parts(
    name: &str,
    hooks: Option<&UserHooks>,
    mcp: Option<mcp_entry::Place>,
    settings: Option<&settings::Settings>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    if let Some(hooks) = hooks {
        let file = hooks
            .file(env)
            .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its hooks file is is not known"))?;
        parts.push(Part::Hooks {
            hooks: hooks.clone(),
            file,
        });
    }
    if let Some(m) = mcp {
        parts.push(Part::Mcp(m));
    }
    if let Some(s) = settings {
        let path = s
            .file(env)
            .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its global config is is not known"))?;
        parts.extend(s.set.iter().map(|setting| Part::Setting {
            path: path.clone(),
            setting: setting.clone(),
        }));
    }
    Ok(parts)
}
