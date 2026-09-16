//! A rule in words, for the pane: when it fires, what it does, where, and
//! which version of it you said yes to. Pure. The rules themselves are seen
//! in the wiring graph (`graph.rs`).

use super::rows::Names;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_duration};
use rl_proto::reaction::Reaction;
use rl_proto::rule::{Rule, RuleStatus};
use std::path::Path;

pub fn key(r: &Rule) -> String {
    format!("{}:{}/{}", r.kind(), r.scope(), r.name())
}

/// Whose file a scope is: global, or a workspace by name.
pub fn owner(scope: &str, names: &Names) -> String {
    if scope == rl_proto::schedule::GLOBAL_SCOPE {
        "global".into()
    } else {
        names.workspace(Path::new(scope))
    }
}

fn fact(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v.into())])
}

/// A path by the name it is registered under, else with `~` for home.
fn place(p: &Path, names: &Names) -> String {
    names.registered(p).unwrap_or_else(|| super::rows::short_path(p))
}

/// A rule in words: when, what, where, and where it stands.
pub fn lines(r: &Rule, names: &Names, now: Timestamp, last_run_title: Option<&str>) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw(format!(" {}", r.name())).bold(),
        Span::raw(format!("  {} · {}", r.kind(), owner(r.scope(), names))).dim(),
    ])];
    let status = match r.status() {
        RuleStatus::NeedsApproval => Span::raw(" needs your yes").yellow().bold(),
        RuleStatus::Paused => Span::raw(" paused").dim(),
        RuleStatus::On => Span::raw(" on"),
        RuleStatus::Removed => Span::raw(" removed from its file").dim(),
    };
    lines.push(Line::from(status));
    lines.push(Line::raw(""));
    lines.push(fact("when", r.when()));
    if let Rule::Reaction(x) = r
        && let Some(f) = &x.spec.filter
    {
        let mut parts = Vec::new();
        if let Some(t) = &f.tag {
            parts.push(format!("tag {t}"));
        }
        if let Some(c) = &f.cause {
            parts.push(format!("cause {c}"));
        }
        if let Some(c) = &f.cli {
            parts.push(format!("cli {c}"));
        }
        if let Some(g) = &f.group {
            parts.push(format!("group {g}"));
        }
        if !parts.is_empty() {
            lines.push(fact("only if", parts.join(" · ")));
        }
    }
    match r {
        Rule::Reaction(Reaction { spec, .. }) if spec.ask.is_some() => {
            let ask = spec.ask.as_ref().unwrap();
            lines.push(fact("asks", ask.title.clone()));
            for o in &ask.options {
                let action = o.action().label();
                let mark = if o.default { " *" } else { "" };
                let mut s = format!("  {:<10} {}{mark}", o.key, o.text());
                if !action.is_empty() {
                    s.push_str(&format!("  → {action}"));
                }
                lines.push(Line::from(Span::raw(format!("          {s}"))));
            }
            if let Some(e) = &ask.expires {
                lines.push(fact("expires", format!("after {e}")));
            }
        }
        _ => lines.push(fact("does", r.does())),
    }
    let mut where_ = match r.cwd() {
        Some(p) => place(p, names),
        None if r.is_global() => "(no cwd)".into(),
        None => owner(r.scope(), names),
    };
    if !r.tags().is_empty() {
        where_.push_str(&format!(" · tags {}", r.tags().join(", ")));
    }
    lines.push(fact("where", where_));
    match r {
        Rule::Schedule(s) => {
            let mut policy = Vec::new();
            if s.spec.missed != Default::default() {
                policy.push(format!("missed: {:?}", s.spec.missed).to_lowercase());
            }
            if s.spec.overlap != Default::default() {
                policy.push(format!("overlap: {:?}", s.spec.overlap).to_lowercase());
            }
            if !policy.is_empty() {
                lines.push(fact("policy", policy.join(" · ")));
            }
        }
        Rule::Reaction(x) => {
            if let Some(t) = &x.spec.throttle {
                lines.push(fact("throttle", t.clone()));
            }
        }
    }
    lines.push(Line::raw(""));
    if let Some(t) = r.next_at()
        && r.status() == RuleStatus::On
    {
        let when = if t > now {
            format!("in {}", fmt_duration(t.duration_since(now)))
        } else {
            "due".into()
        };
        lines.push(fact("next", when));
    }
    let last = match (r.last_fired_at(), last_run_title) {
        (Some(t), Some(title)) => format!("{} ago · {title}", fmt_age(t, now)),
        (Some(t), None) => format!("{} ago", fmt_age(t, now)),
        (None, _) => "never".into(),
    };
    lines.push(fact("last", last));
    let version = if r.approved() {
        "this version is approved".to_string()
    } else if r.ever_approved() {
        "changed since you last said yes".to_string()
    } else {
        "never approved".to_string()
    };
    lines.push(fact("version", version));
    lines.push(fact("file", place(r.source(), names)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::reaction::ReactionSpec;
    use rl_proto::schedule::{Schedule, ScheduleSpec};

    fn schedule(scope: &str, name: &str, approved: bool, paused: bool) -> Rule {
        Rule::Schedule(Schedule {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: format!("{scope}/.relay/schedules.yaml").into(),
            spec: ScheduleSpec {
                every: Some("4h".into()),
                run: Some(Task::Shell("make test".into())),
                ..Default::default()
            },
            paused,
            last_fired_at: None,
            last_run: None,
            next_at: Some("2026-09-10T16:00:00Z".parse().unwrap()),
            removed_at: None,
            approved_hash: approved.then(|| "h".into()),
        })
    }

    fn reaction(scope: &str, name: &str) -> Rule {
        Rule::Reaction(Reaction {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/h/reactions.yaml".into(),
            spec: ReactionSpec {
                on: vec!["run.failed".into()],
                run: Some(Task::Shell("say no".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: Some("2026-09-10T11:00:00Z".parse().unwrap()),
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
    }

    #[test]
    fn a_rule_in_words() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let text = |r: &Rule| {
            lines(r, &names, now, Some("nightly tests"))
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let s = text(&schedule("/home/f/relay", "nightly", false, false));
        assert!(s.starts_with(" nightly  schedule · relay\n needs your yes\n"), "{s}");
        assert!(
            s.contains(" when     every 4h\n does     make test\n where    relay\n"),
            "{s}"
        );
        assert!(
            s.contains(" last     never\n version  never approved\n file     /home/f/relay/.relay/schedules.yaml"),
            "{s}"
        );
        assert!(!s.contains(" next "), "not approved: no next: {s}");

        let s = text(&reaction("global", "notify"));
        assert!(
            s.contains(" when     on run.failed\n does     say no\n where    (no cwd)\n"),
            "{s}"
        );
        assert!(
            s.contains(" last     1h ago · nightly tests\n version  this version is approved"),
            "{s}"
        );
    }
}
