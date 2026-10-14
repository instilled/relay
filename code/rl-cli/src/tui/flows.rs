//! A flow in words, for its row and the pane: where its latest firing
//! stands, what starts it, where it runs, whether it has a yes, and each of
//! its steps. Pure.

use super::flow_graph;
use super::flow_links::{self, Links};
use super::rows::{FlowRow, Names, short_path};
use super::theme;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_duration};
use rl_flow::def::{Does, FlowDef};
use rl_proto::flow::{Filter, Firing, FiringStatus, Flow, FlowStatus, Match, NotLoaded, Reloaded, StepStatus};
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

/// What starts a flow, whole: its time in words, then its `on` with what its `where` asks
/// of the run — `every 30m · on run.failed where cause flow:nightly`. With neither, `by hand`.
pub fn trigger_words(f: &Flow, def: &FlowDef) -> String {
    let when = f.when();
    let mut said: Vec<String> = when
        .split(" or ")
        .filter(|part| !part.starts_with("on ") && *part != "by hand")
        .map(when_words)
        .collect();
    if !def.trigger.on.is_empty() {
        let mut on = format!("on {}", def.trigger.on.join(", "));
        if let Some(w) = def.trigger.filter.as_ref().map(where_words).filter(|w| !w.is_empty()) {
            on.push_str(&format!(" where {w}"));
        }
        said.push(on);
    }
    if said.is_empty() {
        return "by hand".into();
    }
    said.join(" · ")
}

/// A `where` in words, each field it gives: `tag nightly, cause flow:nightly`, a `data` field
/// by its name — `severity ERROR or WARNING`.
fn where_words(w: &Filter) -> String {
    let fields = [
        ("tag", &w.tag),
        ("cause", &w.cause),
        ("cli", &w.cli),
        ("group", &w.group),
    ];
    let mut said: Vec<String> = fields
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}")))
        .collect();
    for (field, want) in &w.data {
        let want = match want {
            Match::One(v) => v.clone(),
            Match::Any(vs) => vs.join(" or "),
        };
        said.push(format!("{field} {want}"));
    }
    said.join(", ")
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
/// its last run came to; before it has run, what its flow (`flow`, when loaded) says of
/// itself (`unfired`). Waiting for a yes, it says so first.
pub fn stands(f: &FlowRow, flow: Option<&Flow>, now: Timestamp) -> String {
    let said = match (f.firing, f.last()) {
        (Some(x), _) => match x.outcome.as_deref().filter(|o| !o.is_empty()) {
            Some(o) => format!("{} · {o}", state_words(x)),
            None => state_words(x),
        },
        (None, Some(i)) => i.summary.clone(),
        (None, None) => return flow.map(|x| unfired(x, now)).unwrap_or_default(),
    };
    match flow.and_then(needs_yes) {
        Some(yes) => format!("{yes} · {said}"),
        None => said,
    }
}

/// What a flow's row leads with while it waits for a yes: `needs a yes again`
/// once changed since it had one, `needs a yes` before it ever did.
fn needs_yes(f: &Flow) -> Option<&'static str> {
    match (f.status(), &f.approved_hash) {
        (FlowStatus::NeedsApproval, Some(_)) => Some("needs a yes again"),
        (FlowStatus::NeedsApproval, None) => Some("needs a yes"),
        _ => None,
    }
}

/// A flow that has not run, as its row says it: what starts it and, on,
/// when it fires next — `every 30m · next in 12m`, `on run.failed`;
/// `paused · every 30m`; changed since its yes, `needs a yes again · …`.
fn unfired(f: &Flow, now: Timestamp) -> String {
    let when = when_words(&f.when());
    if let Some(yes) = needs_yes(f) {
        return format!("{yes} · {when}");
    }
    match f.status() {
        FlowStatus::On => match f.next_at {
            Some(t) if t > now => format!("{when} · next in {}", fmt_age(now, t)),
            Some(_) => format!("{when} · due"),
            None => when,
        },
        FlowStatus::Paused => format!("paused · {when}"),
        FlowStatus::NeedsApproval | FlowStatus::Removed => when,
    }
}

/// A flow's row opened (`→`): its latest firing's steps, each with its mark, from column `from`
/// — `probe ✓ · triage ✓ · review ?`, dim, a failure red.
pub fn steps_line(f: &Firing, from: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(from))];
    for (n, s) in f.steps.iter().enumerate() {
        if n > 0 {
            spans.push(Span::raw(" · ").dim());
        }
        spans.push(Span::raw(format!("{} ", s.name)).dim());
        let mark = Span::raw(glyph(s.status));
        spans.push(if s.status == StepStatus::Failed {
            mark.style(theme::bad())
        } else {
            mark.dim()
        });
    }
    Line::from(spans)
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

/// A flow in the pane: where it runs, whether it has a yes, when it fires next and last did;
/// then its wiring — what starts it and the flows among `links` that do,
/// its steps drawn as the latest firing left them (or, before it has fired, as the flow writes
/// them), and the flows it starts. `at`: the step under the caret, while the pane is walked,
/// lit. Where the drawing is wider than `width`, the steps are listed instead.
#[allow(clippy::too_many_arguments)]
pub fn pane_lines(
    f: &Flow,
    def: &FlowDef,
    firing: Option<&Firing>,
    at: Option<usize>,
    links: &Links,
    names: &Names,
    width: usize,
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
    let mut by = trigger_words(f, def);
    if !links.by.is_empty() {
        by.push_str(&format!(" ← {}", links.by.join(", ")));
    }
    lines.push(link("starts on", by));
    match flow_graph::draw(def, firing, at).filter(|c| c.size().0 <= width) {
        Some(c) => {
            let (w, h) = c.size();
            lines.extend(c.lines(0, 0, w, h));
            lines.extend(step_lines(def, firing, None, false));
        }
        None => lines.extend(step_lines(def, firing, at, true)),
    }
    if !links.starts.is_empty() {
        lines.push(link("starts", flow_links::starts_words(&links.starts)));
    }
    lines
}

/// A line of what starts a flow or what it starts, its label wider than a fact's.
fn link(k: &str, v: String) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<11} ")).dim(), Span::raw(v)])
}

/// A line for each step, as the latest firing left it — or, before it has fired, as the flow
/// writes them: its name, how it stands and what it does. Listed in place of the drawing
/// (`listed`), each leads with its mark, `at`'s with the caret and lit, and ends with the
/// steps it comes after and its `if`.
fn step_lines(def: &FlowDef, firing: Option<&Firing>, at: Option<usize>, listed: bool) -> Vec<Line<'static>> {
    let steps: Vec<(&str, Option<StepStatus>)> = match firing {
        Some(x) => x.steps.iter().map(|s| (s.name.as_str(), Some(s.status))).collect(),
        None => def.steps.iter().map(|s| (s.name.as_str(), None)).collect(),
    };
    let w = steps.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
    let mut lines = Vec::new();
    for (n, (name, status)) in steps.into_iter().enumerate() {
        let lit = at == Some(n);
        let step = def.steps.iter().find(|s| s.name == name);
        let does = step.map(|s| match &s.does {
            Does::Run(a) => a.label(),
            Does::Ask(a) => format!("asks: {}", a.text),
        });
        let lead = if listed {
            format!(
                " {} {} {name:<w$}",
                if lit { "❯" } else { " " },
                glyph(status.unwrap_or(StepStatus::Pending))
            )
        } else {
            format!("   {name:<w$}")
        };
        let mut spans = vec![Span::raw(lead)];
        if let Some(s) = status {
            spans.push(Span::raw(format!("  {:<7}", s.as_str())).dim());
        }
        if let Some(d) = does.filter(|d| !d.is_empty()) {
            spans.push(Span::raw(format!("  {d}")).dim());
        }
        if let Some(s) = step.filter(|_| listed) {
            if !s.after.is_empty() {
                spans.push(Span::raw(format!("  after {}", s.after.join(", "))).dim());
            }
            if let Some(c) = &s.if_ {
                spans.push(Span::raw(format!("  if {c}")).dim());
            }
        }
        let line = Line::from(spans);
        lines.push(if lit { line.style(theme::selected()) } else { line });
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

    #[test]
    fn a_flows_row_opened_says_its_latest_firings_steps() {
        use StepStatus as S;
        let f = firing(
            FiringStatus::Waiting,
            &[("probe", S::Done), ("triage", S::Done), ("review", S::Waiting)],
        );
        assert_eq!(steps_line(&f, 4).to_string(), "    probe ✓ · triage ✓ · review ?");
        let failed = firing(FiringStatus::Failed, &[("gchat", S::Failed), ("brief", S::Skipped)]);
        let line = steps_line(&failed, 0);
        assert_eq!(line.to_string(), "gchat × · brief ○");
        assert!(line.spans.iter().any(|s| s.content == "×" && s.style == theme::bad()));
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
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let i = ran("exit 0 · 2s");
        let mut waiting = firing(
            F::Waiting,
            &[("probe", S::Done), ("triage", S::Done), ("review", S::Waiting)],
        );
        waiting.outcome = Some("page on-call?".into());
        assert_eq!(
            stands(&row(&i, Some(&waiting)), None, now),
            "waiting at review · page on-call?"
        );
        let mut quiet = firing(F::Done, &[("probe", S::Done)]);
        quiet.outcome = Some(String::new());
        assert_eq!(
            stands(&row(&i, Some(&quiet)), None, now),
            "done",
            "nothing printed: no dot with nothing after"
        );
        quiet.outcome = None;
        assert_eq!(stands(&row(&i, Some(&quiet)), None, now), "done");
        assert_eq!(
            stands(&row(&i, None), None, now),
            "exit 0 · 2s",
            "no firing known: its last run's own line"
        );
        // Changed since its yes: it says so first, whatever it last did.
        let changed = flow(Some("an older hash"), def(None));
        assert_eq!(
            stands(&row(&i, Some(&quiet)), Some(&changed), now),
            "needs a yes again · done"
        );
        // Never had one: no `again`.
        assert_eq!(
            stands(&row(&i, Some(&quiet)), Some(&flow(None, def(None))), now),
            "needs a yes · done"
        );
    }

    #[test]
    fn a_flow_that_has_not_run_says_what_starts_it() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let yes = Some("0123456789abcdef");
        let says = |f: &Flow| {
            stands(
                &FlowRow {
                    key: "f:prod-logs".into(),
                    name: "prod-logs".into(),
                    runs: vec![],
                    when: None,
                    firing: None,
                },
                Some(f),
                now,
            )
        };
        let on = flow(yes, def(None));
        assert_eq!(says(&on), "at 02:30 · next in 30m");
        let due = Flow {
            next_at: Some("2026-09-10T11:59:00Z".parse().unwrap()),
            ..flow(yes, def(None))
        };
        assert_eq!(says(&due), "at 02:30 · due");
        // On an event only: nothing is next, what starts it is all.
        let mut event = def(None);
        event["trigger"] = serde_json::json!({ "when": null, "on": ["run.failed"], "filter": null });
        let event = Flow {
            next_at: None,
            ..flow(yes, event)
        };
        assert_eq!(says(&event), "on run.failed");
        let paused = Flow {
            paused: true,
            ..flow(yes, def(None))
        };
        assert_eq!(says(&paused), "paused · at 02:30");
        assert_eq!(
            says(&flow(Some("an older hash"), def(None))),
            "needs a yes again · at 02:30"
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

    /// The pane's lines for `f` as text, `width` wide, wired as `links` say.
    fn pane_text(f: &Flow, firing: Option<&Firing>, at: Option<usize>, links: &Links, width: usize) -> String {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let names = Names::new([("/w/api".into(), "api".to_string())]);
        let d: FlowDef = serde_json::from_value(f.def.clone()).unwrap();
        pane_lines(f, &d, firing, at, links, &names, width, now)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_flow_in_words() {
        let text = |f: &Flow, firing: Option<&Firing>| pane_text(f, firing, None, &Links::default(), 80);
        let waiting = firing(
            FiringStatus::Waiting,
            &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
        );
        let s = text(&flow(Some("0123456789abcdef"), def(Some("/w/api"))), Some(&waiting));
        assert!(s.starts_with(" prod-logs  flow · api · on\n\n"), "{s}");
        assert!(
            s.contains(
                " prod-logs  flow · api · on\n\n in       api\n yes      approved 01234567\n next     in 30m00s\n"
            ),
            "{s}"
        );
        assert!(s.contains(" last     1h ago · waiting at review\n"), "{s}");
        assert!(
            s.ends_with(
                " steps\n \
                 starts on   at 02:30\n \
                 ╭─────────╮    ╭──────────╮\n \
                 │ ✓ probe │───▶│ ? review │\n \
                 ╰─────────╯    ╰──────────╯\n   \
                 probe   done     new-log-errors.sh\n   \
                 review  waiting  asks: fix it?"
            ),
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
            s.contains(" │ ○ probe │───▶│ ○ review │\n ╰─────────╯    ╰──────────╯\n   probe   new-log-errors.sh\n   review  asks: fix it?"),
            "{s}"
        );
    }

    #[test]
    fn a_pane_too_narrow_for_the_drawing_lists_the_steps() {
        let waiting = firing(
            FiringStatus::Waiting,
            &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
        );
        let mut d = def(None);
        d["steps"][1]["if_"] = serde_json::json!({ "Set": { "head": "probe", "rest": "found" } });
        let f = flow(Some("0123456789abcdef"), d);
        let s = pane_text(&f, Some(&waiting), None, &Links::default(), 29);
        assert!(
            s.ends_with(
                " steps\n \
                 starts on   at 02:30\n   \
                 ✓ probe   done     new-log-errors.sh\n   \
                 ? review  waiting  asks: fix it?  after probe  if probe.found"
            ),
            "{s}"
        );
        assert!(pane_text(&f, Some(&waiting), None, &Links::default(), 30).contains("│ ✓ probe │"));
    }

    #[test]
    fn a_step_under_the_caret_is_lit() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let names = Names::new([("/w/api".into(), "api".to_string())]);
        let f = flow(Some("0123456789abcdef"), def(Some("/w/api")));
        let d: FlowDef = serde_json::from_value(f.def.clone()).unwrap();
        let x = firing(
            FiringStatus::Waiting,
            &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
        );
        // Listed, its line.
        let lines = pane_lines(&f, &d, Some(&x), Some(1), &Links::default(), &names, 20, now);
        let review = lines.iter().find(|l| l.to_string().contains("? review")).unwrap();
        assert!(review.to_string().starts_with(" ❯ ? review"), "{review}");
        assert_eq!(review.style, theme::selected());
        // Drawn, its box.
        let lines = pane_lines(&f, &d, Some(&x), Some(1), &Links::default(), &names, 80, now);
        let mid = lines.iter().find(|l| l.to_string().contains("│ ? review │")).unwrap();
        let lit: Vec<&str> = mid
            .spans
            .iter()
            .filter(|s| s.style == theme::selected())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(lit, ["│", "│"], "{mid:?}");
        assert!(!lines.iter().any(|l| l.style == theme::selected()), "no line lit whole");
    }

    #[test]
    fn what_starts_a_flow_in_words() {
        let words = |trigger: serde_json::Value| {
            let mut d = def(None);
            d["trigger"] = trigger;
            let f = flow(None, d);
            let d: FlowDef = serde_json::from_value(f.def.clone()).unwrap();
            trigger_words(&f, &d)
        };
        assert_eq!(
            words(serde_json::json!({ "when": { "Every": 1800 }, "on": [], "filter": null })),
            "every 30m"
        );
        assert_eq!(
            words(
                serde_json::json!({ "when": { "Every": 1800 }, "on": ["run.failed", "run.stopped"],
                                      "filter": { "cause": "flow:nightly" } })
            ),
            "every 30m · on run.failed, run.stopped where cause flow:nightly"
        );
        assert_eq!(
            words(serde_json::json!({ "when": null, "on": ["logs.*"],
                                      "filter": { "tag": "prod", "data": { "severity": ["ERROR", "WARNING"] } } })),
            "on logs.* where tag prod, severity ERROR or WARNING"
        );
        assert_eq!(
            words(serde_json::json!({ "when": { "Cron": "30 7 * * 1-5" }, "on": [], "filter": null })),
            "at 07:30, Mon–Fri"
        );
        assert_eq!(
            words(serde_json::json!({ "when": null, "on": [], "filter": null })),
            "by hand"
        );
    }

    #[test]
    fn the_pane_says_which_flows_start_it_and_which_it_starts() {
        let links = Links {
            by: vec!["nightly".into(), "deploy".into()],
            starts: vec![
                ("triage".into(), vec!["decision.taken".into()]),
                ("page".into(), vec!["run.failed".into(), "run.lost".into()]),
            ],
        };
        let s = pane_text(&flow(None, def(None)), None, None, &links, 80);
        assert!(s.contains("\n starts on   at 02:30 ← nightly, deploy\n"), "{s}");
        assert!(
            s.ends_with("\n starts      triage (on decision.taken) · page (on run.failed, run.lost)"),
            "{s}"
        );
        let s = pane_text(&flow(None, def(None)), None, None, &Links::default(), 80);
        assert!(!s.contains(" starts  "), "nothing it starts, no line: {s}");
    }

    #[test]
    fn a_flow_heard_by_another_on_its_failures_says_it_starts_it() {
        let a = flow(Some("0123456789abcdef"), def(None));
        let mut d = def(None);
        d["name"] = "triage".into();
        d["trigger"] =
            serde_json::json!({ "when": null, "on": ["run.failed"], "filter": { "cause": "flow:prod-logs" } });
        let b = Flow {
            name: "triage".into(),
            def: d,
            ..flow(Some("0123456789abcdef"), def(None))
        };
        let all = flow_links::wired(&[a.clone(), b.clone()], &[]);
        let s = pane_text(&a, None, None, &flow_links::links("prod-logs", &all), 80);
        assert!(s.ends_with("\n starts      triage (on run.failed)"), "{s}");
        let s = pane_text(&b, None, None, &flow_links::links("triage", &all), 80);
        assert!(
            s.contains("\n starts on   on run.failed where cause flow:prod-logs ← prod-logs\n"),
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
