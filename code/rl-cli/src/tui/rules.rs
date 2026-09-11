//! The rules view: every schedule and reaction as one list, grouped by whose
//! file it is, what needs your yes at the top. Pure — rows and lines from
//! what the daemon said.

use super::rows::Names;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use relay_core::time::{fmt_age, fmt_duration};
use relay_proto::reaction::Reaction;
use relay_proto::rule::{Rule, RuleKind, RuleStatus};
use std::path::Path;

pub enum RuleRow<'a> {
    Group(String),
    Rule(&'a Rule),
}

impl RuleRow<'_> {
    pub fn key(&self) -> Option<String> {
        match self {
            RuleRow::Group(_) => None,
            RuleRow::Rule(r) => Some(key(r)),
        }
    }
}

pub fn key(r: &Rule) -> String {
    format!("{}:{}/{}", r.kind(), r.scope(), r.name())
}

/// Whose file a scope is: yours, or a workspace by name.
pub fn owner(scope: &str, names: &Names) -> String {
    if scope == relay_proto::schedule::GLOBAL_SCOPE {
        "yours".into()
    } else {
        names.workspace(Path::new(scope))
    }
}

fn rank(r: &Rule) -> u8 {
    match r.status() {
        RuleStatus::NeedsApproval => 0,
        RuleStatus::On => 1,
        RuleStatus::Paused => 2,
        RuleStatus::Removed => 3,
    }
}

/// Groups with something waiting for a yes first, then your own file, then
/// workspaces by name. Inside a group: needs-yes, on, paused; by name.
pub fn build<'a>(rules: &'a [Rule], names: &Names, filter: &str) -> Vec<RuleRow<'a>> {
    let f = filter.to_lowercase();
    let mut groups: Vec<(String, String, Vec<&Rule>)> = Vec::new();
    for r in rules {
        if r.removed_at().is_some() {
            continue;
        }
        let label = owner(r.scope(), names);
        if !f.is_empty()
            && ![r.name(), &label, &r.when(), &r.does()]
                .iter()
                .any(|s| s.to_lowercase().contains(&f))
        {
            continue;
        }
        match groups.iter_mut().find(|(s, _, _)| s == r.scope()) {
            Some((_, _, v)) => v.push(r),
            None => groups.push((r.scope().to_string(), label, vec![r])),
        }
    }
    for (_, _, v) in &mut groups {
        v.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name().cmp(b.name())));
    }
    groups.sort_by(|(sa, la, va), (sb, lb, vb)| {
        let pending = |v: &Vec<&Rule>| v.iter().any(|r| r.status() == RuleStatus::NeedsApproval);
        pending(vb)
            .cmp(&pending(va))
            .then_with(|| {
                sb.eq(relay_proto::schedule::GLOBAL_SCOPE)
                    .cmp(&sa.eq(relay_proto::schedule::GLOBAL_SCOPE))
            })
            .then_with(|| la.cmp(lb))
    });
    let mut rows = Vec::new();
    for (_, label, v) in groups {
        rows.push(RuleRow::Group(label));
        rows.extend(v.into_iter().map(RuleRow::Rule));
    }
    rows
}

/// Where a rule stands, for its row: what is waiting, or when it fires.
pub fn status_word(r: &Rule, now: Timestamp) -> String {
    match r.status() {
        RuleStatus::NeedsApproval => "needs your yes".into(),
        RuleStatus::Paused => "paused".into(),
        RuleStatus::Removed => "removed".into(),
        RuleStatus::On => match (r.kind(), r.next_at(), r.last_fired_at()) {
            (RuleKind::Schedule, Some(t), _) if t > now => format!("in {}", fmt_duration(t.duration_since(now))),
            (RuleKind::Schedule, Some(_), _) => "due".into(),
            (RuleKind::Schedule, None, _) => "on".into(),
            (RuleKind::Reaction, _, Some(t)) => format!("fired {} ago", fmt_age(t, now)),
            (RuleKind::Reaction, _, None) => "never fired".into(),
        },
    }
}

/// `rules · 1 needs your yes · 4 on · 1 paused`.
pub fn headline(rows: &[RuleRow], filter: &str) -> String {
    let (mut yes, mut on, mut paused) = (0, 0, 0);
    for r in rows {
        if let RuleRow::Rule(r) = r {
            match r.status() {
                RuleStatus::NeedsApproval => yes += 1,
                RuleStatus::On => on += 1,
                RuleStatus::Paused => paused += 1,
                RuleStatus::Removed => {}
            }
        }
    }
    let mut s = String::from("rules");
    if yes + on + paused == 0 {
        s.push_str(" · none");
    }
    if yes > 0 {
        s.push_str(&format!(" · {yes} need{} your yes", if yes == 1 { "s" } else { "" }));
    }
    if on > 0 {
        s.push_str(&format!(" · {on} on"));
    }
    if paused > 0 {
        s.push_str(&format!(" · {paused} paused"));
    }
    if !filter.is_empty() {
        s.push_str(&format!(" · /{filter}"));
    }
    s
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
    use relay_proto::action::Task;
    use relay_proto::reaction::ReactionSpec;
    use relay_proto::schedule::{Schedule, ScheduleSpec};

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

    fn keys(rows: &[RuleRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                RuleRow::Group(g) => format!("[{g}]"),
                RuleRow::Rule(r) => r.name().to_string(),
            })
            .collect()
    }

    #[test]
    fn what_needs_a_yes_comes_first() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let rules = vec![
            reaction("global", "notify"),
            schedule("/home/f/api", "deploy", true, true),
            schedule("/home/f/relay", "nightly", false, false),
            schedule("/home/f/relay", "backup", true, false),
        ];
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let rows = build(&rules, &names, "");
        assert_eq!(
            keys(&rows),
            ["[relay]", "nightly", "backup", "[yours]", "notify", "[api]", "deploy"]
        );
        assert_eq!(headline(&rows, ""), "rules · 1 needs your yes · 2 on · 1 paused");
        assert_eq!(status_word(&rules[2], now), "needs your yes");
        assert_eq!(status_word(&rules[3], now), "in 4h00m");
        assert_eq!(status_word(&rules[1], now), "paused");
        assert_eq!(status_word(&rules[0], now), "fired 1h ago");
        assert_eq!(keys(&build(&rules, &names, "notif")), ["[yours]", "notify"]);
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
