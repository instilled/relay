//! A project's overview: `enter` on its line opens its flows, under what was said of it, with
//! `+ new flow` last; `esc` goes back to the line. A real `rld` over a fresh home.

use super::flow_tests::{fired, flow_in};
use super::tests::{draw_text, panel, press, type_in};
use super::{NEW_FLOW, View};
use crossterm::event::{KeyCode, KeyModifiers};
use rl_proto::flow::{FiringStatus, FlowApproval, FlowApprove, StepStatus};
use std::path::Path;

/// A flow `in` the folder: one step that runs nothing much.
fn md_in(folder: &Path) -> String {
    format!("---\nin: {}\nsteps:\n  probe:\n    run: x\n---\n", folder.display())
}

/// The yes `name` waits for, said.
async fn approve(app: &mut super::App, name: &str) {
    let hash = app.flows.iter().find(|f| f.name == name).unwrap().hash.clone();
    app.rpc
        .call::<FlowApprove>(FlowApproval {
            name: name.into(),
            hash,
        })
        .await
        .unwrap();
    app.refresh().await;
}

/// The cursor on the project's line, and `enter`.
async fn enter_project(app: &mut super::App, folder: &Path) {
    app.select(Some(format!("g:{}", folder.display())));
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

#[tokio::test]
async fn enter_on_a_project_line_opens_its_overview_with_its_flows_and_new_flow_last() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    // alpha has its yes; zulu waits for one, so it comes first.
    approve(&mut app, "alpha").await;
    enter_project(&mut app, &folder).await;
    assert!(app.view == View::Overview);
    assert_eq!(app.keys(), ["f:zulu", "f:alpha", NEW_FLOW]);
    assert!(!app.show_pane);
    token.cancel();
}

#[tokio::test]
async fn a_project_with_no_flows_opens_on_new_flow() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    assert!(app.view == View::Overview);
    assert_eq!(app.keys(), [NEW_FLOW]);
    assert_eq!(app.sel.as_deref(), Some(NEW_FLOW));
    assert!(app.on_new_flow());
    token.cancel();
}

#[tokio::test]
async fn everywheres_overview_has_the_flows_with_no_in() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    flow_in(&tmp, &mut app, "homed", &md_in(&folder)).await;
    app.refresh().await;
    // The list has no `everywhere` line with flows off: its overview is opened as a flow found
    // elsewhere opens it.
    app.open_overview(super::overview::Scope::Everywhere, None);
    assert!(app.view == View::Overview);
    assert_eq!(app.keys(), ["f:loose", NEW_FLOW]);
    token.cancel();
}

#[tokio::test]
async fn esc_leaves_the_overview_for_the_project_line() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    assert!(app.view == View::Overview);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert_eq!(app.sel, Some(format!("g:{}", folder.display())));
    assert!(app.overview.is_none());
    token.cancel();
}

#[tokio::test]
async fn the_overview_draws_its_top_its_flows_and_new_flow() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    let frame = draw_text(&mut app);
    for words in [
        "api",
        "no summary yet · /summary writes it",
        "alpha",
        "zulu",
        "+ new flow",
    ] {
        assert!(frame.contains(words), "{words}: {frame}");
    }
    // The headline is the project's, with its tally; the list's toggles stand down.
    assert!(frame.contains("api · 2 flows"), "{frame}");
    assert!(!frame.contains("◌h"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn a_flows_question_stands_under_it_and_the_line_answers_it() {
    use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    approve(&mut app, "alpha").await;
    approve(&mut app, "zulu").await;
    fired(
        &tmp,
        &folder,
        "alpha",
        "fire0001",
        &[("probe", StepStatus::Waiting, None)],
        FiringStatus::Waiting,
    );
    // The firing's question: its chain is the firing.
    let now = jiff::Timestamp::now();
    let asked = Decision {
        id: "dec00001".into(),
        workspace: folder.clone(),
        run: None,
        chain: "fire0001".into(),
        group: None,
        title: "ship it?".into(),
        summary: None,
        options: vec![DecisionOption {
            key: "ok".into(),
            ..Default::default()
        }],
        multi: false,
        status: DecisionStatus::Open,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        chosen: vec![],
        outcome_runs: vec![],
        note: None,
    };
    let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
    ledger
        .call_blocking(move |c| rl_ledger::decisions::insert(c, &asked))
        .unwrap();
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    let keys = app.keys();
    let at = keys.iter().position(|k| k == "f:alpha").expect("alpha is there");
    assert_eq!(keys.get(at + 1).map(String::as_str), Some("q:dec00001"), "{keys:?}");
    app.select(Some("q:dec00001".into()));
    assert_eq!(app.question_here().map(|q| q.id.as_str()), Some("dec00001"));
    type_in(&mut app, "1").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(
        !app.digest.decisions.iter().any(|d| d.id == "dec00001"),
        "answered from the line"
    );
    token.cancel();
}

#[tokio::test]
async fn the_flow_under_the_cursor_going_away_moves_the_cursor() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:zulu".into()));
    app.flows.retain(|f| f.name != "zulu");
    app.settle();
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    // And with none left, `+ new flow`.
    app.flows.clear();
    app.settle();
    assert_eq!(app.sel.as_deref(), Some(NEW_FLOW));
    token.cancel();
}
