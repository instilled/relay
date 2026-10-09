//! A flow in words, for its row and the pane: where its latest firing
//! stands, what starts it, where it runs, whether it has a yes, and each of
//! its steps. Pure.

use super::rows::{FlowRow, Names, short_path};
use super::theme;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_duration};
use rl_flow::def::{Does, FlowDef};
use rl_proto::flow::{Firing, FiringStatus, Flow, FlowStatus, NotLoaded, Reloaded, StepStatus};
use std::path::Path;

/// What starts a flow, in words where they read better than the line it
/// came from: `30 7 * * 1-5` → `at 07:30, Mon–Fri`. Each part of `when()`
/// (`every 30m or on run.failed`) is read on its own.
pub fn when_words(when: &str) -> String {
    when.split(" or ")
        .map(|part| cron_words(part).unwrap_or_else(|| part.to_string()))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// A cron line in words: `0 8-18 * * 1-5` → `hourly 08:00–18:00, Mon–Fri`.
/// `None` for anything this does not recognise, which then stays exactly as
/// written — a wrong reading would be worse than the cron line itself.
fn cron_words(expr: &str) -> Option<String> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    let [min, hour, dom, month, dow] = f[..] else {
        return None;
    };
    if dom != "*" || month != "*" {
        return None;
    }
    let m: u32 = min.parse().ok().filter(|m| *m < 60)?;
    let clock = |h: u32| format!("{h:02}:{m:02}");
    let hours = match hour.split_once('-') {
        _ if hour == "*" => format!("hourly at :{m:02}"),
        Some((a, b)) => match (a.parse::<u32>(), b.parse::<u32>()) {
            (Ok(a), Ok(b)) if a < 24 && b < 24 => format!("hourly {}–{}", clock(a), clock(b)),
            _ => return None,
        },
        None => match hour.parse::<u32>() {
            Ok(h) if h < 24 => format!("at {}", clock(h)),
            _ => return None,
        },
    };
    match days(dow) {
        Some(d) => Some(format!("{hours}, {d}")),
        None if dow == "*" => Some(hours),
        None => None,
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

/// Where a firing stands, as its flow's row says it: `running · triage`,
/// `waiting at review`, `waiting · triage lost`, `done`, `failed at probe`,
/// `stopped`.
pub fn state_words(f: &Firing) -> String {
    let at = |s: StepStatus| -> Vec<&str> {
        f.steps
            .iter()
            .filter(|x| x.status == s)
            .map(|x| x.name.as_str())
            .collect()
    };
    match f.status {
        FiringStatus::Running => match at(StepStatus::Running).as_slice() {
            [] => "running".into(),
            going => format!("running · {}", going.join(", ")),
        },
        FiringStatus::Waiting => match (at(StepStatus::Waiting).as_slice(), at(StepStatus::Lost).as_slice()) {
            ([], []) => "waiting".into(),
            ([], lost) => format!("waiting · {} lost", lost.join(", ")),
            (asking, _) => format!("waiting at {}", asking.join(", ")),
        },
        FiringStatus::Done => "done".into(),
        FiringStatus::Failed => match at(StepStatus::Failed).first() {
            Some(step) => format!("failed at {step}"),
            None => "failed".into(),
        },
        FiringStatus::Stopped => "stopped".into(),
    }
}

/// Where a flow's row says it stands: where its latest firing is, and what that came to —
/// `waiting at review · page on-call?`, `done · *Morning brief*`; with no firing known, what
/// its last run came to.
pub fn stands(f: &FlowRow) -> String {
    let Some(x) = f.firing else { return f.summary() };
    match x.outcome.as_deref().filter(|o| !o.is_empty()) {
        Some(o) => format!("{} · {o}", state_words(x)),
        None => state_words(x),
    }
}

/// A step's mark in the pane: `✓` done, `×` failed, `○` not run (yet, or
/// at all), `?` asking, `…` going.
pub(crate) fn glyph(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Done => "✓",
        StepStatus::Failed => "×",
        StepStatus::Pending | StepStatus::Skipped | StepStatus::Stopped => "○",
        StepStatus::Waiting => "?",
        StepStatus::Running | StepStatus::Lost => "…",
    }
}

fn fact(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v.into())])
}

/// A path by the name it is registered under, else with `~` for home.
fn place(p: &Path, names: &Names) -> String {
    names.registered(p).unwrap_or_else(|| short_path(p))
}

/// A flow in the pane: what starts it, where it runs, whether it has a yes,
/// when it fires next and last did, and each step as the latest firing left
/// it — or, before it has fired, as the flow writes them.
pub fn pane_lines(
    f: &Flow,
    def: &FlowDef,
    firing: Option<&Firing>,
    names: &Names,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let folder = f.folder.as_deref();
    let mut lines = vec![Line::from(vec![
        Span::raw(format!(" {}", f.name)).bold(),
        Span::raw(format!(
            "  flow · {} · {}",
            folder.map_or_else(|| "everywhere".into(), |p| names.workspace(p)),
            f.status()
        ))
        .dim(),
    ])];
    lines.push(Line::raw(""));
    lines.push(fact("when", when_words(&f.when())));
    let where_ = match &folder {
        Some(p) => place(p, names),
        None if def.trigger.on.is_empty() => "the Relay home".into(),
        None => "the event's folder".into(),
    };
    lines.push(fact("in", where_));
    let yes = match &f.approved_hash {
        Some(h) if f.approved() => format!("approved {}", &h[..h.len().min(8)]),
        _ => "needs a yes".into(),
    };
    lines.push(match f.status() {
        FlowStatus::NeedsApproval => Line::from(vec![
            Span::raw(format!(" {:<8} ", "yes")).dim(),
            Span::styled(yes, theme::needs()),
        ]),
        _ => fact("yes", yes),
    });
    if let Some(t) = f.next_at
        && f.status() == FlowStatus::On
    {
        let when = if t > now {
            format!("in {}", fmt_duration(t.duration_since(now)))
        } else {
            "due".into()
        };
        lines.push(fact("next", when));
    }
    let last = match firing {
        Some(x) => format!("{} ago · {}", fmt_age(x.started_at, now), state_words(x)),
        None => "never".into(),
    };
    lines.push(fact("last", last));
    lines.push(fact("folder", short_path(&f.source)));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::raw(" steps").dim()));
    let steps: Vec<(&str, Option<StepStatus>)> = match firing {
        Some(x) => x.steps.iter().map(|s| (s.name.as_str(), Some(s.status))).collect(),
        None => def.steps.iter().map(|s| (s.name.as_str(), None)).collect(),
    };
    let w = steps.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
    for (name, status) in steps {
        let does = def.steps.iter().find(|s| s.name == name).map(|s| match &s.does {
            Does::Run(a) => a.label(),
            Does::Ask(a) => format!("asks: {}", a.text),
        });
        let mut spans = vec![Span::raw(format!(
            "   {} {name:<w$}",
            glyph(status.unwrap_or(StepStatus::Pending))
        ))];
        if let Some(s) = status {
            spans.push(Span::raw(format!("  {:<7}", s.as_str())).dim());
        }
        if let Some(d) = does.filter(|d| !d.is_empty()) {
            spans.push(Span::raw(format!("  {d}")).dim());
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// What `/reload` says in the flash: how many flows loaded, and what did not
/// — the one by name and why, several by count, the pane listing them.
pub fn reload_note(r: &Reloaded) -> String {
    let mut note = format!("reloaded · {} flow{}", r.loaded, if r.loaded == 1 { "" } else { "s" });
    match r.not_loaded.as_slice() {
        [] => {}
        [one] => note.push_str(&format!(" · 1 not loaded: {one}")),
        many => note.push_str(&format!(" · {} not loaded", many.len())),
    }
    note
}

/// What did not load, two lines each: the flow and where in its file, in
/// red, then why, under it. Nothing when all loaded; else a blank line after.
pub fn not_loaded_lines(broken: &[&NotLoaded]) -> Vec<Line<'static>> {
    if broken.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::from(Span::styled(" not loaded", theme::bad()))];
    for n in broken {
        lines.push(Line::from(Span::styled(
            format!("   {} · {}", n.flow, n.at()),
            theme::bad(),
        )));
        lines.push(Line::from(Span::raw(format!("     {}", n.message))));
    }
    lines.push(Line::raw(""));
    lines
}

#[cfg(test)]
mod tests {
    use super::super::rows::FlowRow;
    use super::*;
    use rl_proto::flow::StepState;
    use rl_proto::methods::DigestItem;
    use rl_proto::types::{Run, RunKind};

    #[test]
    fn a_cadence_reads_in_words_where_it_can() {
        let w = |s: &str| when_words(s);
        assert_eq!(w("0 8-18 * * 1-5"), "hourly 08:00–18:00, Mon–Fri");
        assert_eq!(w("30 2 * * *"), "at 02:30");
        assert_eq!(w("5 * * * *"), "hourly at :05");
        assert_eq!(w("0 9 * * 1,3,5"), "at 09:00, Mon, Wed, Fri");
        assert_eq!(w("0 7 * * 0"), "at 07:00, Sun");
        assert_eq!(w("30 7 * * 1-5 or on run.failed"), "at 07:30, Mon–Fri or on run.failed");
        // Left alone: an interval, an event, by hand, and cron this cannot read.
        assert_eq!(w("every 30m"), "every 30m");
        assert_eq!(w("on run.failed"), "on run.failed");
        assert_eq!(w("by hand"), "by hand");
        assert_eq!(w("*/5 * * * *"), "*/5 * * * *");
        assert_eq!(w("0 2 1 * *"), "0 2 1 * *");
        assert_eq!(w("0 2 * * mon"), "0 2 * * mon");
    }

    fn firing(status: FiringStatus, steps: &[(&str, StepStatus)]) -> Firing {
        Firing {
            id: "fi1".into(),
            flow: "prod-logs".into(),
            trigger: "when".into(),
            event: None,
            status,
            started_at: "2026-09-10T11:00:00Z".parse().unwrap(),
            ended_at: None,
            outcome: None,
            steps: steps
                .iter()
                .map(|(name, status)| StepState {
                    name: name.to_string(),
                    status: *status,
                    run: None,
                    decision: None,
                })
                .collect(),
        }
    }

    fn ran(summary: &str) -> DigestItem {
        let at: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        DigestItem {
            run: Run::new("r1", RunKind::Command, "probe", vec![], "/w", "/w", at),
            summary: summary.into(),
            at,
            unseen: false,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: None,
        }
    }

    fn row<'a>(i: &'a DigestItem, firing: Option<&'a Firing>) -> FlowRow<'a> {
        FlowRow {
            key: "f:prod-logs".into(),
            name: "prod-logs".into(),
            runs: vec![i],
            when: None,
            firing,
        }
    }

    #[test]
    fn a_flows_row_says_where_it_stands_and_what_it_came_to() {
        use FiringStatus as F;
        use StepStatus as S;
        let i = ran("exit 0 · 2s");
        let mut waiting = firing(
            F::Waiting,
            &[("probe", S::Done), ("triage", S::Done), ("review", S::Waiting)],
        );
        waiting.outcome = Some("page on-call?".into());
        assert_eq!(stands(&row(&i, Some(&waiting))), "waiting at review · page on-call?");
        let mut quiet = firing(F::Done, &[("probe", S::Done)]);
        quiet.outcome = Some(String::new());
        assert_eq!(
            stands(&row(&i, Some(&quiet))),
            "done",
            "nothing printed: no dot with nothing after"
        );
        quiet.outcome = None;
        assert_eq!(stands(&row(&i, Some(&quiet))), "done");
        assert_eq!(
            stands(&row(&i, None)),
            "exit 0 · 2s",
            "no firing known: its last run's own line"
        );
    }

    #[test]
    fn a_firing_says_where_it_stands() {
        use FiringStatus as F;
        use StepStatus as S;
        let words = |status, steps: &[(&str, StepStatus)]| state_words(&firing(status, steps));
        assert_eq!(
            words(
                F::Running,
                &[("probe", S::Done), ("triage", S::Running), ("review", S::Pending)]
            ),
            "running · triage"
        );
        assert_eq!(words(F::Running, &[("probe", S::Pending)]), "running");
        assert_eq!(
            words(
                F::Waiting,
                &[("probe", S::Done), ("triage", S::Done), ("review", S::Waiting)]
            ),
            "waiting at review"
        );
        assert_eq!(
            words(F::Waiting, &[("probe", S::Done), ("triage", S::Lost)]),
            "waiting · triage lost"
        );
        assert_eq!(words(F::Done, &[("probe", S::Done)]), "done");
        assert_eq!(
            words(F::Failed, &[("probe", S::Failed), ("triage", S::Skipped)]),
            "failed at probe"
        );
        assert_eq!(words(F::Stopped, &[("probe", S::Stopped)]), "stopped");
    }

    fn flow(approved: Option<&str>, def: serde_json::Value) -> Flow {
        Flow {
            name: "prod-logs".into(),
            hash: "0123456789abcdef".into(),
            folder: def["in_"].as_str().map(Into::into),
            def,
            source: "/h/flows/prod-logs".into(),
            paused: false,
            approved_hash: approved.map(Into::into),
            last_fired_at: None,
            next_at: Some("2026-09-10T12:30:00Z".parse().unwrap()),
            removed_at: None,
        }
    }

    fn def(in_: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "name": "prod-logs",
            "in_": in_,
            "trigger": { "when": { "Cron": "30 2 * * *" }, "on": [], "filter": null },
            "missed": "skip",
            "overlap": "skip",
            "throttle_secs": 0,
            "steps": [
                { "name": "probe", "after": [], "if_": null, "in_": null,
                  "does": { "Run": { "run": "new-log-errors.sh" } } },
                { "name": "review", "after": ["probe"], "if_": null, "in_": null,
                  "does": { "Ask": { "text": "fix it?", "options": [], "default": null,
                                     "expires": null, "group": null, "multi": false } } }
            ]
        })
    }

    #[test]
    fn a_flow_in_words() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let names = Names::new([("/w/api".into(), "api".to_string())]);
        let text = |f: &Flow, firing: Option<&Firing>| {
            let d: FlowDef = serde_json::from_value(f.def.clone()).unwrap();
            pane_lines(f, &d, firing, &names, now)
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let waiting = firing(
            FiringStatus::Waiting,
            &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
        );
        let s = text(&flow(Some("0123456789abcdef"), def(Some("/w/api"))), Some(&waiting));
        assert!(s.starts_with(" prod-logs  flow · api · on\n\n"), "{s}");
        assert!(
            s.contains(" when     at 02:30\n in       api\n yes      approved 01234567\n next     in 30m00s\n"),
            "{s}"
        );
        assert!(s.contains(" last     1h ago · waiting at review\n"), "{s}");
        assert!(
            s.contains(" steps\n   ✓ probe   done     new-log-errors.sh\n   ? review  waiting  asks: fix it?"),
            "{s}"
        );

        // Not said yes to, never fired, no `in`: the steps as the flow
        // writes them, and nothing next.
        let s = text(&flow(None, def(None)), None);
        assert!(s.starts_with(" prod-logs  flow · everywhere · needs approval\n"), "{s}");
        assert!(
            s.contains(" in       the Relay home\n yes      needs a yes\n last     never\n"),
            "{s}"
        );
        assert!(!s.contains(" next "), "{s}");
        assert!(
            s.contains("   ○ probe   new-log-errors.sh\n   ○ review  asks: fix it?"),
            "{s}"
        );
    }

    fn not_loaded(flow: &str, line: Option<usize>, message: &str) -> NotLoaded {
        NotLoaded {
            flow: flow.into(),
            file: format!("/h/flows/{flow}/flow.md").into(),
            line,
            column: None,
            message: message.into(),
        }
    }

    #[test]
    fn a_reload_says_what_did_not_load() {
        let report = |loaded, not_loaded| Reloaded {
            loaded,
            not_loaded,
            ..Default::default()
        };
        assert_eq!(reload_note(&report(2, vec![])), "reloaded · 2 flows");
        assert_eq!(reload_note(&report(1, vec![])), "reloaded · 1 flow");
        let bad = not_loaded("prod-logs", Some(3), "in: ~/x is not a project Relay knows");
        assert_eq!(
            reload_note(&report(2, vec![bad.clone()])),
            "reloaded · 2 flows · 1 not loaded: prod-logs · flow.md:3 · in: ~/x is not a project Relay knows"
        );
        let worse = not_loaded("pulse", None, "a flow needs at least one step");
        assert_eq!(
            reload_note(&report(0, vec![bad, worse])),
            "reloaded · 0 flows · 2 not loaded"
        );
    }

    #[test]
    fn the_pane_lists_what_did_not_load_with_where() {
        let bad = not_loaded("prod-logs", Some(3), "in: ~/x is not a project Relay knows");
        let worse = not_loaded("pulse", None, "a flow needs at least one step");
        let text: Vec<String> = not_loaded_lines(&[&bad, &worse])
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(
            text,
            [
                " not loaded",
                "   prod-logs · flow.md:3",
                "     in: ~/x is not a project Relay knows",
                "   pulse · flow.md",
                "     a flow needs at least one step",
                ""
            ]
        );
        assert!(not_loaded_lines(&[]).is_empty());
    }
}
