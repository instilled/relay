//! `config.json` in the Relay home: the harnesses `new` offers, the header
//! badges, and how the list stands — its grouping, its toggles, and what you
//! moved, hid, pinned and renamed. Strict: a field this does not know is an
//! error, not something to carry along.

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::Registry;
use rl_detect::registry::find_bin;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// What `new` offers, by profile name. Default: every agent in the
    /// registry whose binary is on `PATH`.
    pub harnesses: Vec<String>,
    /// The badge plugins a project header runs. Default: `["gh-pr"]`.
    pub badges: Vec<String>,
    pub list: ListPrefs,
}

/// How the list groups its rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Grouping {
    /// Under their project folders, as the list always had them.
    #[default]
    Project,
    /// Under Needs input · Working · Done, as `claude agents` has them.
    State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListPrefs {
    pub grouping: Grouping,
    pub show_agents: bool,
    pub show_shells: bool,
    pub show_hidden: bool,
    /// Rows in the order you put them, per project; and the projects'.
    pub order: Vec<Order>,
    /// What you hid, by key, and when.
    pub hidden: Vec<Hidden>,
    /// Row keys, in the order they were pinned.
    pub pins: Vec<String>,
    /// Your names for rows (by key) and projects (`g:<folder>`).
    pub names: BTreeMap<String, String>,
}

impl Default for ListPrefs {
    fn default() -> Self {
        Self {
            grouping: Grouping::Project,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            order: Vec::new(),
            hidden: Vec::new(),
            pins: Vec::new(),
            names: BTreeMap::new(),
        }
    }
}

/// A project's rows in the order you put them, first on top — or, under
/// `Order::PROJECTS`, the projects themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    /// The project's header key, `g:<folder>`; or `Order::PROJECTS`.
    pub group: String,
    /// Row keys; under `Order::PROJECTS`, header keys.
    #[serde(default)]
    pub rows: Vec<String>,
}

impl Order {
    /// The order the projects stand in: nothing a header's key could be.
    pub const PROJECTS: &'static str = "projects";
}

/// Something you hid: a row by its key, or a project by `g:<folder>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hidden {
    pub key: String,
    pub at: Timestamp,
}

/// The file as it is written: `harnesses` left out means "whatever is found".
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    harnesses: Option<Vec<String>>,
    #[serde(default = "default_badges")]
    badges: Vec<String>,
    #[serde(default)]
    list: ListPrefs,
}

fn default_badges() -> Vec<String> {
    vec!["gh-pr".into()]
}

/// Every agent in the home's registry whose binary is on `PATH`.
pub fn found_harnesses(home: &Home) -> Vec<String> {
    let registry = Registry::load(home).unwrap_or_else(|_| Registry::builtin());
    registry
        .agents()
        .filter(|p| find_bin(&p.bins).is_some())
        .map(|p| p.name.clone())
        .collect()
}

impl Config {
    /// The file in `home`, or the defaults where there is none.
    pub fn load(home: &Home) -> Result<Config> {
        let path = home.config_json();
        let file: File = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("{}", path.display()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => File {
                harnesses: None,
                badges: default_badges(),
                list: ListPrefs::default(),
            },
            Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        };
        Ok(Config {
            harnesses: file.harnesses.unwrap_or_else(|| found_harnesses(home)),
            badges: file.badges,
            list: file.list,
        })
    }

    /// Writes it atomically. Harnesses that are just what is found are left
    /// out, so an agent installed later is offered too.
    pub fn save(&self, home: &Home) -> Result<()> {
        let harnesses = (self.harnesses != found_harnesses(home)).then(|| self.harnesses.clone());
        let file = File {
            harnesses,
            badges: self.badges.clone(),
            list: self.list.clone(),
        };
        std::fs::create_dir_all(home.path())?;
        rl_core::atomic::write_json(&home.config_json(), &file)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Home) {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        (tmp, home)
    }

    #[test]
    fn no_file_is_the_defaults() {
        let (_t, home) = home();
        let c = Config::load(&home).unwrap();
        assert_eq!(c.badges, ["gh-pr"]);
        assert_eq!(c.list, ListPrefs::default());
        assert!(c.list.show_agents && c.list.show_shells && !c.list.show_hidden);
        assert_eq!(c.list.grouping, Grouping::Project);
    }

    #[test]
    fn prefs_round_trip() {
        let (_t, home) = home();
        let mut c = Config::load(&home).unwrap();
        c.harnesses = vec!["claude".into(), "codex".into()];
        c.list.grouping = Grouping::State;
        c.list.show_shells = false;
        c.list.order.push(Order {
            group: "g:/w/api".into(),
            rows: vec!["r:a".into(), "n:claude:1".into()],
        });
        c.list.hidden.push(Hidden {
            key: "g:/w/web".into(),
            at: "2026-11-07T10:00:00Z".parse().unwrap(),
        });
        c.list.pins.push("r:a".into());
        c.list.names.insert("g:/w/api".into(), "the api".into());
        c.save(&home).unwrap();
        assert_eq!(Config::load(&home).unwrap(), c);
    }

    #[test]
    fn unknown_fields_are_an_error() {
        let (_t, home) = home();
        std::fs::create_dir_all(home.path()).unwrap();
        for text in [
            r#"{"bogus": 1}"#,
            r#"{"list": {"grouping": "project", "bogus": true}}"#,
            r#"{"list": {"order": [{"group": "g:/w", "rows": [], "bogus": 1}]}}"#,
            r#"{"list": {"grouping": "folder"}}"#,
        ] {
            std::fs::write(home.config_json(), text).unwrap();
            assert!(Config::load(&home).is_err(), "{text}");
        }
        std::fs::write(home.config_json(), r#"{"list": {"show_hidden": true}}"#).unwrap();
        let c = Config::load(&home).unwrap();
        assert!(c.list.show_hidden && c.list.show_agents, "the rest default");
    }
}
