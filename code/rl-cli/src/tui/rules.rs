//! A rule in words, for the pane: when it fires, what it does, where, and
//! which version of it you said yes to. Pure. The rules themselves are seen
//! in the wiring graph (`graph.rs`).

use super::rows::Names;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_duration};
use rl_proto::reaction::Reaction;
use rl_proto::rule::{NotLoaded, Rule, RuleKind, RuleStatus};
use rl_proto::schedule::ReloadReport;
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

/// A cadence in words, where it reads better than the line it came from:
/// `cron 0 8-18 * * 1-5` → `hourly 08:00–18:00, Mon–Fri`. Anything this
/// does not recognise stays exactly as written — a wrong reading would be
/// worse than the cron line itself.
pub fn when_words(when: &str) -> String {
    let Some(expr) = when.strip_prefix("cron ") else {
        return when.to_string();
    };
    let f: Vec<&str> = expr.split_whitespace().collect();
    let [min, hour, dom, month, dow] = f[..] else {
        return when.to_string();
    };
    if dom != "*" || month != "*" {
        return when.to_string();
    }
    let m: u32 = match min.parse() {
        Ok(m) if m < 60 => m,
        _ => return when.to_string(),
    };
    let clock = |h: u32| format!("{h:02}:{m:02}");
    let hours = match hour.split_once('-') {
        _ if hour == "*" => format!("hourly at :{m:02}"),
        Some((a, b)) => match (a.parse::<u32>(), b.parse::<u32>()) {
            (Ok(a), Ok(b)) if a < 24 && b < 24 => format!("hourly {}–{}", clock(a), clock(b)),
            _ => return when.to_string(),
        },
        None => match hour.parse::<u32>() {
            Ok(h) if h < 24 => format!("at {}", clock(h)),
            _ => return when.to_string(),
        },
    };
    match days(dow) {
        Some(d) => format!("{hours}, {d}"),
        None if dow == "*" => hours,
        None => when.to_string(),
    }
}

/// The days of a cron's last field, in words; `None` when it is every day or
/// a shape this does not read.
fn days(dow: &str) -> Option<String> {
    const NAMES: [&str; 8] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let name = |d: &str| d.parse::<usize>().ok().filter(|n| *n <= 7).map(|n| NAMES[n]);
    if dow == "*" {
        return None;
    }
    if let Some((a, b)) = dow.split_once('-') {
        return Some(format!("{}–{}", name(a)?, name(b)?));
    }
    let each: Option<Vec<&str>> = dow.split(',').map(name).collect();
    Some(each?.join(", "))
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
    lines.push(fact("when", when_words(&r.when())));
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
        None if r.is_global() => match r.kind() {
            RuleKind::Reaction => "the event's folder".into(),
            RuleKind::Schedule => "(no cwd)".into(),
        },
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

/// What `/reload` says in the flash: how many of each loaded, and what did
/// not — the one by name and why (`· 1 not loaded: bad needs `cwd``), several
/// by count, the pane listing them.
pub fn reload_note(schedules: &ReloadReport, reactions: &ReloadReport) -> String {
    let n = |k: usize, what: &str| format!("{k} {what}{}", if k == 1 { "" } else { "s" });
    let mut note = format!(
        "reloaded · {} · {}",
        n(schedules.loaded, "schedule"),
        n(reactions.loaded, "reaction")
    );
    let not_loaded: Vec<&NotLoaded> = schedules.not_loaded.iter().chain(&reactions.not_loaded).collect();
    // A daemon from before `not_loaded` says the same in `errors`.
    let errors: Vec<&String> = schedules.errors.iter().chain(&reactions.errors).collect();
    match (not_loaded.as_slice(), errors.as_slice()) {
        ([one], _) => note.push_str(&format!(" · 1 not loaded: {}", one.short())),
        ([], []) => {}
        ([], [one]) => note.push_str(&format!(" · 1 not loaded: {one}")),
        ([], many) => note.push_str(&format!(" · {} not loaded", many.len())),
        (many, _) => note.push_str(&format!(" · {} not loaded", many.len())),
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::reaction::ReactionSpec;
    use rl_proto::schedule::{Schedule, ScheduleSpec};
    use std::path::PathBuf;

    #[test]
    fn a_cadence_reads_in_words_where_it_can() {
        let w = |s: &str| when_words(s);
        assert_eq!(w("cron 0 8-18 * * 1-5"), "hourly 08:00–18:00, Mon–Fri");
        assert_eq!(w("cron 30 2 * * *"), "at 02:30");
        assert_eq!(w("cron 5 * * * *"), "hourly at :05");
        assert_eq!(w("cron 0 9 * * 1,3,5"), "at 09:00, Mon, Wed, Fri");
        assert_eq!(w("cron 0 7 * * 0"), "at 07:00, Sun");
        // Left alone: an interval, a reaction, and cron this cannot read.
        assert_eq!(w("every 30m"), "every 30m");
        assert_eq!(w("on run.failed"), "on run.failed");
        assert_eq!(w("cron */5 * * * *"), "cron */5 * * * *");
        assert_eq!(w("cron 0 2 1 * *"), "cron 0 2 1 * *");
        assert_eq!(w("cron 0 2 * * mon"), "cron 0 2 * * mon");
    }

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
            s.contains(" when     on run.failed\n does     say no\n where    the event's folder\n"),
            "{s}"
        );
        assert!(
            s.contains(" last     1h ago · nightly tests\n version  this version is approved"),
            "{s}"
        );
    }

    fn not_loaded(scope: &str, name: &str, file: &str, rule: Option<&str>, message: &str) -> NotLoaded {
        NotLoaded {
            scope: scope.into(),
            name: name.into(),
            file: file.into(),
            rule: rule.map(Into::into),
            line: Some(4),
            column: rule.is_none().then_some(3),
            message: message.into(),
        }
    }

    #[test]
    fn a_reload_says_what_did_not_load() {
        let report = |loaded, not_loaded: Vec<NotLoaded>| ReloadReport {
            loaded,
            errors: not_loaded.iter().map(ToString::to_string).collect(),
            not_loaded,
            ..Default::default()
        };
        let bad = not_loaded("global", "global", "/h/schedules.yaml", Some("bad"), "needs `cwd`");
        let file = not_loaded(
            "/w/relay",
            "relay",
            "/w/relay/.relay/reactions.yaml",
            None,
            "did not find expected key",
        );
        assert_eq!(
            reload_note(&report(2, vec![]), &report(1, vec![])),
            "reloaded · 2 schedules · 1 reaction"
        );
        assert_eq!(
            reload_note(&report(2, vec![bad.clone()]), &report(1, vec![])),
            "reloaded · 2 schedules · 1 reaction · 1 not loaded: bad needs `cwd`"
        );
        assert_eq!(
            reload_note(&report(1, vec![]), &report(0, vec![file.clone()])),
            "reloaded · 1 schedule · 0 reactions · 1 not loaded: relay reactions.yaml:4:3 did not find expected key"
        );
        let two = not_loaded("global", "global", "/h/schedules.yaml", Some("worse"), "needs `run`");
        assert_eq!(
            reload_note(&report(2, vec![bad, two]), &report(1, vec![file])),
            "reloaded · 2 schedules · 1 reaction · 3 not loaded"
        );
        // An older daemon's words, when that is all there is.
        let old = ReloadReport {
            loaded: 1,
            errors: vec!["bad: needs `cwd`".into()],
            ..Default::default()
        };
        assert_eq!(
            reload_note(&old, &report(0, vec![])),
            "reloaded · 1 schedule · 0 reactions · 1 not loaded: bad: needs `cwd`"
        );
    }

    #[test]
    fn the_pane_lists_what_did_not_load_with_where() {
        let names = Names::new([(PathBuf::from("/w/relay"), "relay".to_string())]);
        let bad = not_loaded("global", "global", "/h/schedules.yaml", Some("bad"), "needs `cwd`");
        let file = not_loaded(
            "/w/relay",
            "old-name",
            "/w/relay/.relay/reactions.yaml",
            None,
            "did not find expected key",
        );
        let text = |names: Option<&Names>| {
            super::super::pane::not_loaded_lines(&[&bad, &file], names)
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(Some(&names)),
            [
                " not loaded",
                "   global · schedules.yaml:4 · bad",
                "     needs `cwd`",
                "   relay · reactions.yaml:4:3",
                "     did not find expected key",
                ""
            ],
            "whose, by the name the list uses now"
        );
        assert_eq!(
            text(None)[1..5],
            [
                "   schedules.yaml:4 · bad",
                "     needs `cwd`",
                "   reactions.yaml:4:3",
                "     did not find expected key"
            ],
            "on a header, whose is already said"
        );
        assert!(super::super::pane::not_loaded_lines(&[], None).is_empty());
    }
}
