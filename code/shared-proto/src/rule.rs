//! Rules: everything that starts work without you, seen as one thing. A
//! schedule is a rule about time, a reaction a rule about an event; the files
//! stay two, the list is one.

use crate::reaction::Reaction;
use crate::schedule::{GLOBAL_SCOPE, Schedule, scope_label};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::Path;

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
}
