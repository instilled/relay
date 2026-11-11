//! Settings `rl setup` puts in a harness's own global config: a profile's
//! `settings` block, keys at the top of a JSON file with the values Relay
//! wants (Claude's `leftArrowOpensAgents: false` in `~/.claude.json`, so `←`
//! does not open its agents view: Relay's list is that view).
//!
//! ```yaml
//! settings:
//!   file: ["$CLAUDE_CONFIG_DIR/.claude.json", "~/.claude.json"]
//!   set:
//!     - { key: leftArrowOpensAgents, value: false, check: "← opens agents on" }
//! ```
//!
//! The harness owns the file and has no command that sets these (Claude's
//! `claude config` is gone), so Relay writes them itself: read, the keys
//! changed, written whole over it (`user_hooks::write`), everything else in
//! it kept in its order. A file that does not read is left as it was. Taking
//! Relay out (`rl setup --remove`) leaves them: they are yours by then.

use crate::user_hooks;
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::PathBuf;

/// A profile's `settings`: where they go, and what.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Its file: the first of these whose variables are all set; `~` is
    /// `$HOME`.
    pub file: Vec<String>,
    pub set: Vec<Setting>,
}

/// One key, the value it is to have, and what `rl setup --check` says
/// while it has another (`← opens agents on`).
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: String,
    pub value: Value,
    pub check: String,
}

impl Settings {
    /// Its file, as the harness finds it from this environment. None where
    /// no `file` has its variables set.
    pub fn file(&self, env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
        self.file.iter().find_map(|f| crate::sessions::expand(f, &env))
    }
}

/// A harness's settings at the file they go in.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub path: PathBuf,
    pub set: Vec<Setting>,
}

impl Place {
    /// The settings that do not have their value there, read from the file
    /// alone; a file not there has none of them. An error where the file
    /// does not read, which `apply` would leave alone.
    pub fn need(&self) -> Result<Vec<&Setting>> {
        let doc = user_hooks::read(&self.path)?.unwrap_or_default();
        Ok(self.unset(&doc))
    }

    fn unset(&self, doc: &Map<String, Value>) -> Vec<&Setting> {
        self.set.iter().filter(|s| doc.get(&s.key) != Some(&s.value)).collect()
    }

    /// Writes the settings that need it, as `need` finds them now; the
    /// rest of the file as it was. What it set.
    pub fn apply(&self) -> Result<Vec<&Setting>> {
        let mut doc = user_hooks::read(&self.path)?.unwrap_or_default();
        let need = self.unset(&doc);
        if need.is_empty() {
            return Ok(need);
        }
        for s in &need {
            doc.insert(s.key.clone(), s.value.clone());
        }
        user_hooks::write(&self.path, &doc)?;
        Ok(need)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn place(dir: &std::path::Path) -> Place {
        Place {
            path: dir.join(".claude.json"),
            set: vec![Setting {
                key: "leftArrowOpensAgents".into(),
                value: json!(false),
                check: "← opens agents on".into(),
            }],
        }
    }

    #[test]
    fn a_setting_is_written_keeping_everything_else_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let p = place(dir.path());
        assert_eq!(p.need().unwrap().len(), 1, "no file: not set");
        std::fs::write(&p.path, r#"{"z": 1, "leftArrowOpensAgents": true, "a": {"b": [2]}}"#).unwrap();
        assert_eq!(p.need().unwrap()[0].key, "leftArrowOpensAgents");
        assert_eq!(p.apply().unwrap().len(), 1);
        let text = std::fs::read_to_string(&p.path).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc, json!({"z": 1, "leftArrowOpensAgents": false, "a": {"b": [2]}}));
        assert!(
            text.find("\"z\"").unwrap() < text.find("leftArrow").unwrap(),
            "in its place: {text}"
        );
        assert!(p.need().unwrap().is_empty());
        assert!(p.apply().unwrap().is_empty(), "nothing to change");
    }

    #[test]
    fn a_file_not_there_is_made_and_one_that_does_not_read_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = place(dir.path());
        p.apply().unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&p.path).unwrap()).unwrap();
        assert_eq!(doc, json!({"leftArrowOpensAgents": false}));
        std::fs::write(&p.path, "{ not json").unwrap();
        assert!(p.need().is_err() && p.apply().is_err());
        assert_eq!(std::fs::read_to_string(&p.path).unwrap(), "{ not json");
    }
}
