//! The panel seeing flows: a `catch_up: top` flow heading the catch-up, a flow's pane walked, a
//! flow's row opened to its steps. A real `rld` over a fresh home, each firing written into its
//! ledger as the engine leaves one — nothing runs, no agent is asked.

use super::tests::{draw_text, nine_hours_ago, panel, press};
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
/// what its run printed)` — a step that printed has a run, ended, in `folder`, its chain the
/// firing's — and the firing `status`, ended unless it still runs or waits.
fn fired(
    tmp: &tempfile::TempDir,
    folder: &Path,
    flow: &str,
    id: &str,
    steps: &[(&str, StepStatus, Option<&str>)],
    status: FiringStatus,
) {
    use rl_ledger::firings::{self, NewFiring};
    use rl_ledger::{Ledger, runs, session_io};
    use rl_proto::types::{Cause, Run, RunKind, RunState, Source};
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
