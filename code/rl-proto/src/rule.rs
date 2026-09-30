//! Rules: everything that starts work without you, seen as one thing. A
//! schedule is a rule about time, a reaction a rule about an event; the files
//! stay two, the list is one.

use crate::reaction::Reaction;
use crate::schedule::{GLOBAL_SCOPE, Schedule, scope_label};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    Schedule,
    Reaction,
}

impl std::fmt::Display for RuleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RuleKind::Schedule => "schedule",
            RuleKind::Reaction => "reaction",
        })
    }
}

impl std::str::FromStr for RuleKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "schedule" => Ok(RuleKind::Schedule),
            "reaction" => Ok(RuleKind::Reaction),
            _ => Err(format!("bad rule kind `{s}`; schedule or reaction")),
        }
    }
}

/// One rule as the daemon tracks it, whichever file it came from.
// A schedule carries more than a reaction does, so the two variants differ
// in size. Boxing one would reach every construction and match site across
// the daemon and the CLI to spare a move the code never makes in a hot
// path — a rule is read through a reference nearly everywhere it is used.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Rule {
    Schedule(Schedule),
    Reaction(Reaction),
}

/// Where a rule stands, in one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleStatus {
    /// A version nobody has said yes to.
    NeedsApproval,
    Paused,
    On,
    Removed,
}

impl std::fmt::Display for RuleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RuleStatus::NeedsApproval => "needs approval",
            RuleStatus::Paused => "paused",
            RuleStatus::On => "on",
            RuleStatus::Removed => "removed",
        })
    }
}

impl Rule {
    pub fn kind(&self) -> RuleKind {
        match self {
            Rule::Schedule(_) => RuleKind::Schedule,
            Rule::Reaction(_) => RuleKind::Reaction,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Rule::Schedule(s) => &s.name,
            Rule::Reaction(r) => &r.name,
        }
    }
    /// `global`, or a workspace path.
    pub fn scope(&self) -> &str {
        match self {
            Rule::Schedule(s) => &s.scope,
            Rule::Reaction(r) => &r.scope,
        }
    }
    pub fn is_global(&self) -> bool {
        self.scope() == GLOBAL_SCOPE
    }
    /// `global`, or the workspace folder's name.
    pub fn scope_label(&self) -> String {
        scope_label(self.scope())
    }
    pub fn source(&self) -> &Path {
        match self {
            Rule::Schedule(s) => &s.source,
            Rule::Reaction(r) => &r.source,
        }
    }
    pub fn paused(&self) -> bool {
        match self {
            Rule::Schedule(s) => s.paused,
            Rule::Reaction(r) => r.paused,
        }
    }
    pub fn approved(&self) -> bool {
        match self {
            Rule::Schedule(s) => s.approved(),
            Rule::Reaction(r) => r.approved(),
        }
    }
    /// Whether any version was ever approved (a changed file keeps the old yes).
    pub fn ever_approved(&self) -> bool {
        match self {
            Rule::Schedule(s) => s.approved_hash.is_some(),
            Rule::Reaction(r) => r.approved_hash.is_some(),
        }
    }
    pub fn removed_at(&self) -> Option<Timestamp> {
        match self {
            Rule::Schedule(s) => s.removed_at,
            Rule::Reaction(r) => r.removed_at,
        }
    }
    pub fn last_fired_at(&self) -> Option<Timestamp> {
        match self {
            Rule::Schedule(s) => s.last_fired_at,
            Rule::Reaction(r) => r.last_fired_at,
        }
    }
    pub fn last_run(&self) -> Option<&str> {
        match self {
            Rule::Schedule(s) => s.last_run.as_deref(),
            Rule::Reaction(r) => r.last_run.as_deref(),
        }
    }
    /// When a schedule fires next; a reaction has no clock.
    pub fn next_at(&self) -> Option<Timestamp> {
        match self {
            Rule::Schedule(s) => s.next_at,
            Rule::Reaction(_) => None,
        }
    }
    pub fn status(&self) -> RuleStatus {
        if self.removed_at().is_some() {
            RuleStatus::Removed
        } else if !self.approved() {
            RuleStatus::NeedsApproval
        } else if self.paused() {
            RuleStatus::Paused
        } else {
            RuleStatus::On
        }
    }
    /// `every 4h`, `cron 0 2 * * *`, `on run.failed`.
    pub fn when(&self) -> String {
        match self {
            Rule::Schedule(s) => s.spec.when(),
            Rule::Reaction(r) => format!("on {}", r.spec.when()),
        }
    }
    /// What it starts, or the question it asks.
    pub fn does(&self) -> String {
        match self {
            Rule::Schedule(s) => s.spec.action().label(),
            Rule::Reaction(r) => match &r.spec.ask {
                Some(ask) => format!("asks: {}", ask.title),
                None => r.spec.action().label(),
            },
        }
    }
    pub fn tags(&self) -> &[String] {
        match self {
            Rule::Schedule(s) => &s.spec.tags,
            Rule::Reaction(r) => &r.spec.tags,
        }
    }
    pub fn cwd(&self) -> Option<&Path> {
        match self {
            Rule::Schedule(s) => s.spec.cwd.as_deref(),
            Rule::Reaction(r) => r.spec.cwd.as_deref(),
        }
    }
}

/// A rules file, or one rule in it, that did not load at the last reload:
/// where, and why. A file that does not parse keeps its rules as they last
/// loaded; a rule that does not check out is left out while the rest load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotLoaded {
    /// `global`, or a workspace path.
    pub scope: String,
    /// What the scope is called: `global`, or the name its folder is
    /// registered under.
    pub name: String,
    pub file: PathBuf,
    /// The rule, when it was one entry that was wrong and not the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Where in the file, from 1, when it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub message: String,
}

impl NotLoaded {
    pub fn is_global(&self) -> bool {
        self.scope == GLOBAL_SCOPE
    }

    /// `schedules.yaml:4:3`, `reactions.yaml:7`, or just the file's name.
    pub fn at(&self) -> String {
        let file = self
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.to_string_lossy().into_owned());
        match (self.line, self.column) {
            (Some(l), Some(c)) => format!("{file}:{l}:{c}"),
            (Some(l), None) => format!("{file}:{l}"),
            _ => file,
        }
    }

    /// What a one-line flash has room for: a rule by its name, a file by
    /// where it broke.
    pub fn short(&self) -> String {
        match &self.rule {
            Some(r) => format!("{r} {}", self.message),
            None => format!("{} {} {}", self.name, self.at(), self.message),
        }
    }
}

/// `global · schedules.yaml:4:3: did not find expected key`, or for one rule
/// `relay · reactions.yaml:7 · bad: needs `cwd``.
impl std::fmt::Display for NotLoaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule {
            Some(r) => write!(f, "{} · {} · {r}: {}", self.name, self.at(), self.message),
            None => write!(f, "{} · {}: {}", self.name, self.at(), self.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Task;
    use crate::reaction::ReactionSpec;
    use crate::schedule::ScheduleSpec;

    #[test]
    fn one_shape_over_both_files() {
        let s = Rule::Schedule(Schedule {
            scope: "/w".into(),
            name: "nightly".into(),
            hash: "h1".into(),
            source: "/w/.relay/schedules.yaml".into(),
            spec: ScheduleSpec {
                every: Some("4h".into()),
                run: Some(Task::Shell("make test".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: None,
            last_run: None,
            next_at: None,
            removed_at: None,
            approved_hash: Some("h0".into()),
        });
        assert_eq!(s.kind(), RuleKind::Schedule);
        assert_eq!(s.when(), "every 4h");
        assert_eq!(s.does(), "make test");
        assert_eq!(s.status(), RuleStatus::NeedsApproval, "the file changed since the yes");
        assert!(s.ever_approved());
        assert_eq!(s.scope_label(), "w");

        let r = Rule::Reaction(Reaction {
            scope: GLOBAL_SCOPE.into(),
            name: "notify".into(),
            hash: "h".into(),
            source: "/h/reactions.yaml".into(),
            spec: ReactionSpec {
                on: vec!["run.failed".into()],
                run: Some(Task::Shell("say no".into())),
                ..Default::default()
            },
            paused: true,
            last_fired_at: None,
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        });
        assert_eq!(r.status(), RuleStatus::Paused);
        assert!(r.is_global());
        assert_eq!(r.when(), "on run.failed");

        // Kind rides along on the wire.
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["kind"], "reaction");
        assert_eq!(json["name"], "notify");
        let back: Rule = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
        assert_eq!("schedule".parse::<RuleKind>().unwrap(), RuleKind::Schedule);
    }

    #[test]
    fn what_did_not_load_says_where_and_why() {
        let file = NotLoaded {
            scope: GLOBAL_SCOPE.into(),
            name: GLOBAL_SCOPE.into(),
            file: "/h/schedules.yaml".into(),
            rule: None,
            line: Some(4),
            column: Some(3),
            message: "did not find expected key".into(),
        };
        assert!(file.is_global());
        assert_eq!(
            file.to_string(),
            "global · schedules.yaml:4:3: did not find expected key"
        );
        assert_eq!(file.short(), "global schedules.yaml:4:3 did not find expected key");
        let rule = NotLoaded {
            scope: "/w/relay".into(),
            name: "relay".into(),
            file: "/w/relay/.relay/reactions.yaml".into(),
            rule: Some("bad".into()),
            line: Some(7),
            column: None,
            message: "needs `cwd`".into(),
        };
        assert_eq!(rule.to_string(), "relay · reactions.yaml:7 · bad: needs `cwd`");
        assert_eq!(rule.short(), "bad needs `cwd`");
        let bare = NotLoaded {
            line: None,
            ..rule.clone()
        };
        assert_eq!(bare.to_string(), "relay · reactions.yaml · bad: needs `cwd`");
        // What an older daemon leaves out reads as not known.
        let back: NotLoaded = serde_json::from_value(serde_json::json!({
            "scope": "global", "name": "global", "file": "/h/x.yaml", "message": "m"
        }))
        .unwrap();
        assert_eq!((back.rule, back.line, back.column), (None, None, None));
    }
}
