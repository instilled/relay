//! Whether a flow's trigger takes an event: its `on` the event's kind, its `where` what the event
//! is about. The daemon asks of an event that came; the panel, of one a flow's firings would
//! raise, where some of it cannot be known ahead and so rules nothing out. Pure.

use crate::def::Trigger;
use rl_proto::flow::Filter;

/// Whether an `on` pattern takes an event of this kind: `run.failed`, `run.*`, `*`.
pub fn kind_matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == kind,
    }
}

/// Whether a trigger's `on` takes an event of this kind.
pub fn hears(trigger: &Trigger, kind: &str) -> bool {
    trigger.on.iter().any(|p| kind_matches(p, kind))
}

/// An event as a `where` reads it. A field left `None` is not known ahead, and passes.
pub struct Offered<'a> {
    /// The run the event is about, when it is about one.
    pub run: Option<OfferedRun<'a>>,
    /// Its group key.
    pub group: Option<&'a str>,
    /// One field of its `data` as text: `Some(None)` when it has no such field.
    pub data: &'a dyn Fn(&str) -> Option<Option<String>>,
}

/// The run an event is about, as a `where` reads it.
pub struct OfferedRun<'a> {
    /// Its cause as written: `flow:nightly/probe`, `manual`.
    pub cause: String,
    pub tags: Option<&'a [String]>,
    pub cli: Option<Option<&'a str>>,
}

/// Whether `where` lets an event through; when not, the field that said no (`tag`, `cause`,
/// `cli`, `group`, `data.<field>`). A run's tag, cause and cli need a run: an event about none
/// fails each that is given. The cause matches as a prefix: `flow:` takes every flow's runs.
pub fn passes(w: &Filter, e: &Offered) -> Result<(), String> {
    if let Some(tag) = &w.tag
        && e.run
            .as_ref()
            .is_none_or(|r| r.tags.is_some_and(|ts| !ts.iter().any(|t| t == tag)))
    {
        return Err("tag".into());
    }
    if let Some(cause) = &w.cause
        && e.run.as_ref().is_none_or(|r| !r.cause.starts_with(cause.as_str()))
    {
        return Err("cause".into());
    }
    if let Some(cli) = &w.cli
        && e.run
            .as_ref()
            .is_none_or(|r| r.cli.is_some_and(|c| c != Some(cli.as_str())))
    {
        return Err("cli".into());
    }
    if let Some(group) = &w.group
        && e.group.is_some_and(|g| g != group)
    {
        return Err("group".into());
    }
    for (field, want) in &w.data {
        if (e.data)(field).is_some_and(|got| got.is_none_or(|got| !want.matches(&got))) {
            return Err(format!("data.{field}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::Match;
    use std::collections::BTreeMap;

    #[test]
    fn an_on_pattern_takes_its_kind_or_what_its_star_covers() {
        assert!(kind_matches("run.failed", "run.failed"));
        assert!(!kind_matches("run.failed", "run.finished"));
        assert!(kind_matches("run.*", "run.failed"));
        assert!(!kind_matches("run.*", "decision.taken"));
        assert!(kind_matches("*", "deploy.done"));
    }

    #[test]
    fn what_is_not_known_ahead_rules_nothing_out() {
        let none = |_: &str| None;
        let unknown = Offered {
            run: Some(OfferedRun {
                cause: "flow:a/s".into(),
                tags: None,
                cli: None,
            }),
            group: None,
            data: &none,
        };
        let w = Filter {
            tag: Some("x".into()),
            cli: Some("claude".into()),
            group: Some("g".into()),
            data: BTreeMap::from([("severity".to_string(), Match::One("high".into()))]),
            ..Default::default()
        };
        assert_eq!(passes(&w, &unknown), Ok(()));
        // No run at all: what needs one says no.
        let no_run = Offered { run: None, ..unknown };
        assert_eq!(passes(&w, &no_run), Err("tag".into()));
        let cause = Filter {
            cause: Some("flow:a".into()),
            ..Default::default()
        };
        assert_eq!(passes(&cause, &no_run), Err("cause".into()));
        // Known, it decides.
        let flow = |f: &str| Some((f == "flow").then(|| "a".to_string()));
        let ended = Offered {
            run: None,
            group: Some("g"),
            data: &flow,
        };
        let on_a = |name: &str| Filter {
            data: BTreeMap::from([("flow".to_string(), Match::One(name.into()))]),
            ..Default::default()
        };
        assert_eq!(passes(&on_a("a"), &ended), Ok(()));
        assert_eq!(passes(&on_a("b"), &ended), Err("data.flow".into()));
    }
}
