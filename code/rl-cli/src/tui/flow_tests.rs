//! The panel seeing flows: a `catch_up: top` flow heading the catch-up, a flow's pane walked, a
//! flow's row opened to its steps. A real `rld` over a fresh home, each firing written into its
//! ledger as the engine leaves one — nothing runs, no agent is asked.

use super::tests::{draw_text, nine_hours_ago, panel, press, type_in};
use crossterm::event::{KeyCode, KeyModifiers};
use rl_proto::flow::{FiringStatus, FlowReload, StepStatus};
use rl_proto::types::SummaryTarget;
use std::path::Path;

/// The flow `name` written into the panel's home as `md`, and read.
async fn flow_in(tmp: &tempfile::TempDir, app: &mut super::App, name: &str, md: &str) {
    let dir = rl_core::Home::at(tmp.path().join("h")).flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flow.md"), md).unwrap();
    app.rpc.call::<FlowReload>(()).await.unwrap();
}

/// A firing `id` of `flow` from an hour ago, as the engine leaves one: each step `(name, status,
/// what its run printed)` — a step that printed has a run, started and ended, in `folder`, its
/// chain the firing's — and the firing `status`, ended unless it still runs or waits.
fn fired(
    tmp: &tempfile::TempDir,
    folder: &Path,
    flow: &str,
    id: &str,
    steps: &[(&str, StepStatus, Option<&str>)],
    status: FiringStatus,
) {
    use rl_ledger::events::{self, NewEvent};
    use rl_ledger::firings::{self, NewFiring};
    use rl_ledger::{Ledger, runs, session_io};
    use rl_proto::types::{Cause, Run, RunKind, RunState, Source, event_kind};
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    let steps: Vec<(String, StepStatus, Option<String>)> = steps
        .iter()
        .map(|(n, s, o)| (n.to_string(), *s, o.map(String::from)))
        .collect();
    let (flow, id, folder) = (flow.to_string(), id.to_string(), folder.to_path_buf());
    let ended = !matches!(status, FiringStatus::Running | FiringStatus::Waiting);
    let ledger = Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
    ledger
        .call_blocking(move |c| {
            let names: Vec<String> = steps.iter().map(|(n, ..)| n.clone()).collect();
            firings::insert(
                c,
                &NewFiring {
                    id: &id,
                    flow: &flow,
                    def_json: "{}",
                    hash: "h",
                    trigger: "when",
                    event: None,
                    at,
                    steps: &names,
                },
            )?;
            let mut status_of = std::collections::BTreeMap::new();
            for (n, s, out) in &steps {
                status_of.insert(n.clone(), *s);
                let Some(out) = out else { continue };
                let run = format!("{id}-{n}");
                let mut r = Run::new(
                    run.clone(),
                    RunKind::Command,
                    format!("{flow} · {n}"),
                    vec![],
                    &folder,
                    &folder,
                    at,
                );
                r.cause = Cause::Flow {
                    flow: flow.clone(),
                    step: n.clone(),
                };
                r.chain = id.clone();
                runs::insert(c, &r)?;
                // The list shows a run with news: the step's run, started.
                events::append(c, &NewEvent::for_run(&r, at, event_kind::RUN_STARTED, "started"))?;
                session_io::append(c, &run, session_io::Direction::Out, out.as_bytes(), at)?;
                let failed = *s == StepStatus::Failed;
                let state = if failed { RunState::Failed } else { RunState::Finished };
                runs::finish(c, &run, state, Source::Pty, Some(i32::from(failed)), at)?;
                firings::set_step_run(c, &id, n, &run)?;
            }
            firings::save(c, &id, &status_of, status, ended.then_some(at))?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn a_catch_up_top_flows_firing_heads_the_catch_up_above_relays_own() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(
        &tmp,
        &mut app,
        "morning-brief",
        "---\ncatch_up: top\nsteps:\n  brief:\n    run: echo brief\n---\n",
    )
    .await;
    fired(
        &tmp,
        &folder,
        "morning-brief",
        "fire0001",
        &[(
            "brief",
            StepStatus::Done,
            Some("*Morning brief*\r\n\r\n*Anna*\r\n• shipped the parser\r\n"),
        )],
        FiringStatus::Done,
    );
    app.refresh().await;
    app.open_catch_up(nine_hours_ago(), false).await;
    let frame = draw_text(&mut app);
    let brief = frame.find("↻ morning-brief").unwrap_or_else(|| panic!("{frame}"));
    let anna = frame.find("• shipped the parser").unwrap_or_else(|| panic!("{frame}"));
    let ours = frame
        .find("• api:")
        .unwrap_or_else(|| panic!("Relay's own bullets: {frame}"));
    assert!(brief < anna && anna < ours, "the brief heads the top: {frame}");

    // The cursor starts on the rows; up is Relay's top, then the brief.
    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.sel.as_deref(), Some(super::TOP));
    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.sel.as_deref(), Some("h:morning-brief"));
    assert_eq!(
        app.summary_target(),
        Some(SummaryTarget::Firing { id: "fire0001".into() })
    );

    // enter shows all of it in the pane.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.show_pane);
    let frame = draw_text(&mut app);
    assert!(frame.contains("what brief printed"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn several_leads_never_push_relays_own_top_off_screen() {
    let (tmp, token, mut app, folder) = panel().await;
    let long: String = (1..=12).map(|i| format!("line {i}\n")).collect();
    for (n, flow) in ["brief-a", "brief-b", "brief-c"].into_iter().enumerate() {
        flow_in(
            &tmp,
            &mut app,
            flow,
            "---\ncatch_up: top\nsteps:\n  brief:\n    run: echo brief\n---\n",
        )
        .await;
        fired(
            &tmp,
            &folder,
            flow,
            &format!("fire000{n}"),
            &[("brief", StepStatus::Done, Some(&long))],
            FiringStatus::Done,
        );
    }
    app.refresh().await;
    app.open_catch_up(nine_hours_ago(), false).await;
    let frame = draw_text(&mut app);
    assert!(frame.contains("↻ brief-"), "{frame}");
    let ours = frame
        .find("• api:")
        .unwrap_or_else(|| panic!("Relay's own top shows: {frame}"));
    let cut = frame
        .rfind("… ^o on it shows the rest")
        .unwrap_or_else(|| panic!("{frame}"));
    assert!(cut < ours, "the leads are cut above it: {frame}");
    for flow in ["brief-a", "brief-b", "brief-c"] {
        assert!(
            frame.contains(&format!("↻ {flow}")),
            "every lead's title shows: {frame}"
        );
    }
    // The cursor stops only on a lead that has a line on screen.
    for key in app.keys() {
        if let Some(flow) = super::heads::flow_of(&key) {
            assert!(frame.contains(&format!("↻ {flow}")), "{key} has no line: {frame}");
        }
    }
    token.cancel();
}

#[tokio::test]
async fn leads_past_the_room_are_said_by_count_and_the_cursor_skips_them() {
    let (tmp, token, mut app, folder) = panel().await;
    let flows: Vec<String> = (0..30).map(|n| format!("brief-{n:02}")).collect();
    for (n, flow) in flows.iter().enumerate() {
        flow_in(
            &tmp,
            &mut app,
            flow,
            "---\ncatch_up: top\nsteps:\n  brief:\n    run: echo brief\n---\n",
        )
        .await;
        fired(
            &tmp,
            &folder,
            flow,
            &format!("fire{n:04}"),
            &[("brief", StepStatus::Done, Some("a line\n"))],
            FiringStatus::Done,
        );
    }
    app.refresh().await;
    app.open_catch_up(nine_hours_ago(), false).await;
    let frame = draw_text(&mut app);
    assert!(frame.contains(" more"), "{frame}");
    assert!(frame.contains("• api:"), "Relay's own top shows: {frame}");
    let leads: Vec<String> = app
        .keys()
        .into_iter()
        .filter(|k| super::heads::flow_of(k).is_some())
        .collect();
    assert!(!leads.is_empty() && leads.len() < flows.len(), "{leads:?}");
    for key in leads {
        let flow = super::heads::flow_of(&key).unwrap();
        assert!(frame.contains(&format!("↻ {flow}")), "{key} has no line: {frame}");
    }
    token.cancel();
}

#[tokio::test]
async fn enter_twice_on_a_flow_walks_its_steps_and_enter_on_one_reads_its_run() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(
        &tmp,
        &mut app,
        "prod-logs",
        "---\nsteps:\n  probe:\n    run: x\n  triage:\n    after: probe\n    run: y\n---\n",
    )
    .await;
    fired(
        &tmp,
        &folder,
        "prod-logs",
        "fire0001",
        &[
            ("probe", StepStatus::Done, Some("3x connection reset\r\n")),
            ("triage", StepStatus::Done, Some("restart the api pod\r\n")),
        ],
        FiringStatus::Done,
    );
    app.refresh().await;
    app.select(Some("f:prod-logs".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.show_pane, "enter shows the flow");
    let frame = draw_text(&mut app);
    // The firings in the pane, each a row: the one shown marked, when, where it stands, what it
    // came to (cut to the pane's width).
    let row = frame.lines().find(|l| {
        let Some((_, pane)) = l.rsplit_once('│') else {
            return false;
        };
        let words: Vec<&str> = pane.split_whitespace().collect();
        matches!(words.as_slice(), ["▸", _when, "done", came_to] if came_to.starts_with("res"))
    });
    assert!(row.is_some(), "a firing's row: {frame}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(matches!(app.mode, super::Mode::Walk), "again: its steps, walked");
    press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        app.term.as_ref().map(|t| t.id.as_str()),
        Some("fire0001-triage"),
        "the step's run, read in the tile"
    );
    token.cancel();
}

#[tokio::test]
async fn summary_on_a_flow_showing_an_older_firing_tells_that_firing() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "prod-logs", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    for id in ["fire0001", "fire0002"] {
        fired(
            &tmp,
            &folder,
            "prod-logs",
            id,
            &[("probe", StepStatus::Done, Some("found\r\n"))],
            FiringStatus::Done,
        );
    }
    app.refresh().await;
    app.select(Some("f:prod-logs".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        app.summary_target(),
        Some(SummaryTarget::Flow {
            name: "prod-logs".into()
        })
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    // probe, then the firings, newest first: the older is the third stop.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    let older = app.flow_pane.shown().map(|f| f.id.clone()).unwrap();
    assert_eq!(app.summary_target(), Some(SummaryTarget::Firing { id: older }));
    token.cancel();
}

/// On `prod-logs`, walked to its older firing and shown it: enter (the pane, unless shown),
/// enter (the walk), down past probe and the latest, enter.
async fn pick_older(app: &mut super::App) {
    app.select(Some("f:prod-logs".into()));
    if !app.show_pane {
        press(app, KeyCode::Enter, KeyModifiers::NONE).await;
    }
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(matches!(app.mode, super::Mode::Walk));
    press(app, KeyCode::Down, KeyModifiers::NONE).await;
    press(app, KeyCode::Down, KeyModifiers::NONE).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.flow_pane.picked.is_some(), "the older one shown");
}

#[tokio::test]
async fn the_firing_picked_goes_when_the_cursor_leaves_the_row() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "prod-logs", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    for id in ["fire0001", "fire0002"] {
        fired(
            &tmp,
            &folder,
            "prod-logs",
            id,
            &[("probe", StepStatus::Done, Some("found\r\n"))],
            FiringStatus::Done,
        );
    }
    app.refresh().await;
    let flow = || SummaryTarget::Flow {
        name: "prod-logs".into(),
    };
    // esc ends the walk; the pick stays while the cursor is on the row.
    pick_older(&mut app).await;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(!matches!(app.mode, super::Mode::Walk));
    assert!(app.flow_pane.picked.is_some());

    // Off the flow and back: the latest is the flow's again.
    app.select(Some(super::TOP.into()));
    app.select(Some("f:prod-logs".into()));
    assert_eq!(app.flow_pane.picked, None);
    assert_eq!(app.summary_target(), Some(flow()));
    token.cancel();
}

#[tokio::test]
async fn right_opens_a_flows_row_to_its_latest_firings_steps_and_left_folds_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(
        &tmp,
        &mut app,
        "prod-logs",
        "---\nsteps:\n  probe:\n    run: x\n  triage:\n    after: probe\n    run: y\n  review:\n    after: triage\n    ask: ok?\n    options: { ok: {} }\n---\n",
    )
    .await;
    fired(
        &tmp,
        &folder,
        "prod-logs",
        "fire0001",
        &[
            ("probe", StepStatus::Done, Some("3x connection reset\r\n")),
            ("triage", StepStatus::Done, Some("restart the api pod\r\n")),
            ("review", StepStatus::Waiting, None),
        ],
        FiringStatus::Waiting,
    );
    app.refresh().await;
    app.select(Some("f:prod-logs".into()));
    let steps = "probe ✓ · triage ✓ · review ?";
    assert!(!draw_text(&mut app).contains(steps));
    press(&mut app, KeyCode::Right, KeyModifiers::NONE).await;
    assert!(draw_text(&mut app).contains(steps), "→ opens it");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE).await;
    assert!(!draw_text(&mut app).contains(steps), "← folds it");
    // On a row that is not a flow, → does nothing.
    app.select(Some("r:run00001".into()));
    assert!(app.selected_flow().is_none(), "a run row is selected");
    let before = draw_text(&mut app);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE).await;
    assert_eq!(draw_text(&mut app), before);
    assert!(app.open_flows.is_empty());
    app.select(Some("f:prod-logs".into()));
    // With words on the line, the arrows are the line's.
    type_in(&mut app, "ab").await;
    press(&mut app, KeyCode::Right, KeyModifiers::NONE).await;
    assert!(!draw_text(&mut app).contains(steps));
    token.cancel();
}

#[tokio::test]
async fn a_flow_opened_stays_open_while_a_search_hides_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "prod-logs", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    fired(
        &tmp,
        &folder,
        "prod-logs",
        "fire0001",
        &[("probe", StepStatus::Done, Some("found\r\n"))],
        FiringStatus::Done,
    );
    app.refresh().await;
    app.select(Some("f:prod-logs".into()));
    press(&mut app, KeyCode::Right, KeyModifiers::NONE).await;
    assert!(draw_text(&mut app).contains("probe ✓"), "→ opens it");
    app.filter = "nothing-like-it".into();
    app.refresh().await;
    assert!(!draw_text(&mut app).contains("prod-logs"), "the search hides it");
    app.filter.clear();
    app.refresh().await;
    assert!(draw_text(&mut app).contains("probe ✓"), "still open");
    token.cancel();
}
