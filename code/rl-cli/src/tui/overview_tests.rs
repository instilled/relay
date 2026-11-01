//! A project's overview: `enter` on its line opens its flows, under what was said of it, with
//! `+ new flow` last; `esc` goes back to the line. A real `rld` over a fresh home.

use super::flow_proposal::Aim;
use super::flow_proposal_tests::{answer_md, said, unasked};
use super::flow_tests::{fired, flow_in};
use super::overview::Scope;
use super::tests::{draw_text, panel, press, type_in};
use super::{NEW_FLOW, Summarising, View, line};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};
use rl_proto::flow::{FiringStatus, FlowApproval, FlowApprove, StepStatus};
use rl_proto::types::{Summary, SummaryTarget};
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
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.overview, Some(Scope::Everywhere));
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

/// A question `dec00001`, open, asked by the firing `chain`: its one option `ok`.
fn asked_of(tmp: &tempfile::TempDir, folder: &Path, chain: &str) {
    use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
    let now = jiff::Timestamp::now();
    let asked = Decision {
        id: "dec00001".into(),
        workspace: folder.to_path_buf(),
        run: None,
        chain: chain.into(),
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
}

#[tokio::test]
async fn a_flows_question_stands_under_it_and_the_line_answers_it() {
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
    asked_of(&tmp, &folder, "fire0001");
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

#[tokio::test]
async fn enter_on_a_flow_opens_its_page_and_esc_comes_back_to_the_overview() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:alpha".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.show_pane, "its page");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(!app.show_pane && app.view == View::Overview, "back in the overview");
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert_eq!(app.sel, Some(format!("g:{}", folder.display())));
    token.cancel();
}

#[tokio::test]
async fn enter_on_new_flow_says_to_type() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    assert!(app.on_new_flow());
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "type what the new flow should do, then enter");
    assert!(app.line.is_empty());
    assert!(app.flow_proposal.is_none());
    token.cancel();
}

#[tokio::test]
async fn ctrl_e_on_a_flow_opens_its_file() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:alpha".into()));
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    let file = rl_core::Home::at(tmp.path().join("h"))
        .flow_dir("alpha")
        .join("flow.md");
    assert_eq!((editing.name.as_str(), &editing.file), ("alpha", &file));
    token.cancel();
}

#[tokio::test]
async fn show_flow_opens_its_projects_overview_on_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    app.toggle_events().await;
    assert!(app.view == View::Events);
    app.show_flow("zulu").await;
    assert!(app.view == View::Overview);
    assert_eq!(app.overview, Some(Scope::Project(folder.clone())));
    assert_eq!(app.sel.as_deref(), Some("f:zulu"));
    // esc: the list, on the project's line.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert_eq!(app.sel, Some(format!("g:{}", folder.display())));
    // A search that hides it there lets go.
    app.toggle_events().await;
    app.filter_aside = "zzzz".into();
    app.show_flow("zulu").await;
    assert!(app.view == View::Overview);
    assert!(app.filter.is_empty(), "the search let go: {:?}", app.filter);
    assert_eq!(app.sel.as_deref(), Some("f:zulu"));
    token.cancel();
}

#[tokio::test]
async fn show_flow_for_a_flow_with_no_in_opens_everywhere() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    flow_in(&tmp, &mut app, "homed", &md_in(&folder)).await;
    app.refresh().await;
    app.toggle_events().await;
    app.show_flow("loose").await;
    assert!(app.view == View::Overview);
    assert_eq!(app.overview, Some(Scope::Everywhere));
    assert_eq!(app.sel.as_deref(), Some("f:loose"));
    token.cancel();
}

#[tokio::test]
async fn the_everywhere_line_stands_when_a_flow_has_no_in_and_enter_opens_its_overview() {
    let (tmp, token, mut app, folder) = panel().await;
    let everywhere = "g:everywhere".to_string();
    flow_in(&tmp, &mut app, "homed", &md_in(&folder)).await;
    app.refresh().await;
    assert!(!app.keys().contains(&everywhere), "no flow with no in: no line");
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    app.refresh().await;
    assert!(app.keys().contains(&everywhere), "{:?}", app.keys());
    // Its tally at the edge: loose waits for its yes.
    let line = super::tests::screen(&mut app, 120, 40)
        .into_iter()
        .find(|l| l.starts_with("everywhere"))
        .expect("its line");
    assert!(line.contains("1 flow · 1 needs you"), "{line}");
    // The headline does not say them twice; with the line searched away, it says them.
    let named = "↻ 1 everywhere · loose needs you";
    assert!(!draw_text(&mut app).contains(named));
    app.filter = "zzzz".into();
    assert!(draw_text(&mut app).contains(named), "{}", draw_text(&mut app));
    app.filter.clear();
    app.select(Some(everywhere.clone()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.view == View::Overview);
    assert_eq!(app.overview, Some(Scope::Everywhere));
    assert_eq!(app.keys(), ["f:loose", NEW_FLOW]);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(app.sel, Some(everywhere));
    token.cancel();
}

/// A summary kept of `target`, saying `text`.
fn said_of(target: &SummaryTarget, text: &str) -> Summary {
    Summary {
        run: String::new(),
        agent: "fakeai".into(),
        by: "run09999".into(),
        at: jiff::Timestamp::now(),
        text: text.into(),
        since: None,
        source: "summarised".into(),
        reach: String::new(),
        stale: false,
        target: target.key(),
        deep: false,
        from: None,
        calls: 1,
        runs: 1,
    }
}

#[tokio::test]
async fn summary_on_new_flow_in_the_overview_tells_the_project() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    assert!(app.on_new_flow());
    assert_eq!(app.summary_target(), Some(SummaryTarget::Project { workspace: folder }));
    token.cancel();
}

#[tokio::test]
async fn a_project_summary_made_shows_at_the_overviews_top() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    let target = SummaryTarget::Project { workspace: folder };
    app.made
        .insert(target.key(), said_of(&target, "- the build went green"));
    let frame = draw_text(&mut app);
    assert!(frame.contains("the build went green"), "{frame}");
    assert!(!frame.contains("no summary yet"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn summarising_the_project_says_so_at_the_top() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    let target = SummaryTarget::Project { workspace: folder };
    app.summarising = Some(Summarising {
        target: target.clone(),
        title: "proj".into(),
        deep: false,
        from: None,
        since: std::time::Instant::now(),
        task: tokio::spawn(std::future::pending()),
    });
    let frame = draw_text(&mut app);
    assert!(frame.contains("summarising"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn starting_a_project_summary_in_the_overview_does_not_open_the_pane() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    app.summary_start(rl_proto::methods::SummaryParams {
        target: SummaryTarget::Project { workspace: folder },
        deep: false,
        from: None,
        again: false,
        dry: false,
        timeout: None,
    });
    assert!(app.summarising.is_some());
    assert!(!app.show_pane);
    token.cancel();
}

#[tokio::test]
async fn everywheres_top_has_no_summary() {
    let (tmp, token, mut app, _folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    app.refresh().await;
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    app.made.insert(
        SummaryTarget::All.key(),
        said_of(&SummaryTarget::All, "- every project's news"),
    );
    let frame = draw_text(&mut app);
    assert!(!frame.contains("every project's news"), "{frame}");
    assert!(!frame.contains("/summary writes it"), "{frame}");
    assert!(frame.contains("flows with no in"), "{frame}");
    // And /summary on nothing here targets nothing.
    app.select(Some(NEW_FLOW.into()));
    assert_eq!(app.summary_target(), None);
    token.cancel();
}

#[tokio::test]
async fn opening_the_overview_does_not_mark_its_first_flow_seen() {
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
        &[("probe", StepStatus::Failed, Some("boom\r\n"))],
        FiringStatus::Failed,
    );
    app.refresh().await;
    let needs = |app: &super::App| {
        let f = app.flows.iter().find(|f| f.name == "alpha").unwrap();
        super::rows::flow_needs(f, &app.digest)
    };
    assert!(needs(&app), "a failed run unseen: it needs you");
    enter_project(&mut app, &folder).await;
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    app.refresh().await;
    assert!(needs(&app), "opening the overview alone does not clear it");
    // Moving off it and back onto it is looking at it.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    app.refresh().await;
    assert!(!needs(&app), "landed on, it is seen");
    token.cancel();
}

#[tokio::test]
async fn a_question_from_a_live_flow_run_stands_under_its_flow() {
    use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
    use rl_proto::methods::DigestItem;
    use rl_proto::types::{Cause, Run, RunKind};
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "zulu", &md_in(&folder)).await;
    app.refresh().await;
    approve(&mut app, "alpha").await;
    approve(&mut app, "zulu").await;
    enter_project(&mut app, &folder).await;
    // A step of alpha's, still going, asks.
    let now = jiff::Timestamp::now();
    let mut r = Run::new(
        "run07777",
        RunKind::Command,
        "alpha · probe",
        vec![],
        &folder,
        &folder,
        now,
    );
    r.cause = Cause::Flow {
        flow: "alpha".into(),
        step: "probe".into(),
    };
    app.digest.items.push(DigestItem {
        run: r,
        summary: String::new(),
        at: now,
        unseen: false,
        joined_by: None,
        emitted: vec![],
        cwd: None,
        recap: None,
    });
    app.digest.decisions.push(Decision {
        id: "dec00002".into(),
        workspace: folder.clone(),
        run: Some("run07777".into()),
        chain: "run07777".into(),
        group: None,
        title: "go on?".into(),
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
    });
    let keys = app.keys();
    let at = keys.iter().position(|k| k == "f:alpha").expect("alpha is there");
    assert_eq!(keys.get(at + 1).map(String::as_str), Some("q:dec00002"), "{keys:?}");
    assert_eq!(keys.iter().filter(|k| k.starts_with("r:")).count(), 0, "{keys:?}");
    token.cancel();
}

#[tokio::test]
async fn the_line_in_an_overview_on_new_flow_is_in_the_projects_folder() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    assert!(app.on_new_flow());
    assert_eq!(app.folder_here(), folder);
    token.cancel();
}

#[tokio::test]
async fn summarising_again_shows_the_progress_above_the_old_text() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    let target = SummaryTarget::Project { workspace: folder };
    app.made
        .insert(target.key(), said_of(&target, "- the build went green"));
    app.summarising = Some(Summarising {
        target: target.clone(),
        title: "proj".into(),
        deep: false,
        from: None,
        since: std::time::Instant::now(),
        task: tokio::spawn(std::future::pending()),
    });
    let frame = draw_text(&mut app);
    let progress = frame.find("summarising… ").unwrap_or_else(|| panic!("{frame}"));
    let old = frame.find("the build went green").unwrap_or_else(|| panic!("{frame}"));
    assert!(progress < old, "{frame}");
    token.cancel();
}

#[tokio::test]
async fn the_kept_project_summary_stays_at_the_top_with_a_flow_selected() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    let away = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    let mut kept = said_of(&target, "- the deploy went out");
    kept.from = Some(away);
    let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
    ledger.call_blocking(move |c| rl_ledger::kept::put(c, &kept)).unwrap();
    app.away_since = Some(away);
    enter_project(&mut app, &folder).await;
    app.select(Some(NEW_FLOW.into()));
    app.moved().await;
    assert!(
        draw_text(&mut app).contains("the deploy went out"),
        "{}",
        draw_text(&mut app)
    );
    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    let frame = draw_text(&mut app);
    assert!(frame.contains("the deploy went out"), "{frame}");
    assert!(!frame.contains("no summary yet"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn a_flows_page_in_the_overview_says_esc_back() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    assert!(app.hints().ends_with("esc the list"), "{}", app.hints());
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.show_pane);
    let hints = app.hints();
    assert!(hints.contains("^o hides details"), "{hints}");
    assert!(hints.ends_with("esc back"), "{hints}");
    token.cancel();
}

#[tokio::test]
async fn ctrl_backslash_in_the_overview_goes_to_the_list() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    assert!(!app.show_pane);
    press(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL).await;
    assert!(app.view == View::Work);
    assert!(app.overview.is_none());
    assert_eq!(app.sel, Some(format!("g:{}", folder.display())));
    token.cancel();
}

#[tokio::test]
async fn the_events_view_from_the_overview_comes_back_to_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.toggle_events().await;
    assert!(app.view == View::Events);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Overview);
    assert_eq!(app.overview, Some(Scope::Project(folder.clone())));
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    token.cancel();
}

#[tokio::test]
async fn a_catch_up_over_the_overview_leaves_for_the_list() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.open_catch_up(super::tests::nine_hours_ago(), false).await;
    assert!(app.view == View::CatchUp);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert!(app.overview.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_kept_project_summary_shows_at_the_top_when_the_overview_opens_on_a_flow() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    let away = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    let mut kept = said_of(&target, "- the deploy went out");
    kept.from = Some(away);
    let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
    ledger.call_blocking(move |c| rl_ledger::kept::put(c, &kept)).unwrap();
    app.away_since = Some(away);
    enter_project(&mut app, &folder).await;
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    let frame = draw_text(&mut app);
    assert!(frame.contains("the deploy went out"), "{frame}");
    assert!(!frame.contains("no summary yet"), "{frame}");
    token.cancel();
}

/// The overview of `folder` open on its flow `hello`, and the words typed and entered there: the
/// ask let go before it reaches the daemon.
async fn words_on_hello(tmp: &tempfile::TempDir, app: &mut super::App, folder: &Path, words: &str) {
    flow_in(tmp, app, "hello", &md_in(folder)).await;
    app.refresh().await;
    enter_project(app, folder).await;
    app.select(Some("f:hello".into()));
    type_in(app, words).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(app);
}

/// What the flow proposal is for, if there is one.
fn aim(app: &super::App) -> Option<Aim> {
    app.flow_proposal.as_ref().map(|p| p.aim.clone())
}

#[tokio::test]
async fn words_on_a_flow_change_it() {
    let (tmp, token, mut app, folder) = panel().await;
    words_on_hello(&tmp, &mut app, &folder, "every 2h").await;
    let base = std::fs::read_to_string(app.home.flow_dir("hello").join("flow.md")).unwrap();
    assert_eq!(
        aim(&app),
        Some(Aim::Change {
            name: "hello".into(),
            base
        })
    );
    assert_eq!(app.asking.as_ref().unwrap().want, Want::Flow { name: "hello".into() });
    assert!(app.flow_proposal_shown().is_some(), "up in the pane");
    assert!(app.line.is_empty());
    token.cancel();
}

#[tokio::test]
async fn words_on_new_flow_make_one_in_the_project() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    assert!(app.on_new_flow());
    type_in(&mut app, "say hello hourly").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    assert_eq!(
        aim(&app),
        Some(Aim::New {
            named: None,
            folder: Some(folder.clone())
        })
    );
    assert_eq!(
        app.asking.as_ref().unwrap().want,
        Want::NewFlow {
            folder: Some(folder.clone())
        }
    );
    assert!(app.line.is_empty());
    token.cancel();
}

#[tokio::test]
async fn words_on_new_flow_everywhere_have_no_folder() {
    let (tmp, token, mut app, _folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    app.refresh().await;
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    app.select(Some(NEW_FLOW.into()));
    type_in(&mut app, "say hello hourly").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    assert_eq!(
        aim(&app),
        Some(Aim::New {
            named: None,
            folder: None
        })
    );
    assert_eq!(app.asking.as_ref().unwrap().want, Want::NewFlow { folder: None });
    token.cancel();
}

#[tokio::test]
async fn a_runnable_first_word_is_still_words() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    // A bang still runs it.
    type_in(&mut app, "!ls").await;
    assert!(matches!(app.line_what(), line::What::Shell(_)), "{:?}", app.line_what());
    app.line.clear();
    type_in(&mut app, "ls only on weekdays").await;
    assert_eq!(app.line_what(), line::What::Ask("ls only on weekdays".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    assert!(matches!(aim(&app), Some(Aim::Change { name, .. }) if name == "hello"));
    token.cancel();
}

#[tokio::test]
async fn a_question_line_still_answers() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    approve(&mut app, "alpha").await;
    fired(
        &tmp,
        &folder,
        "alpha",
        "fire0001",
        &[("probe", StepStatus::Waiting, None)],
        FiringStatus::Waiting,
    );
    asked_of(&tmp, &folder, "fire0001");
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("q:dec00001".into()));
    type_in(&mut app, "1 ok").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(
        !app.digest.decisions.iter().any(|d| d.id == "dec00001"),
        "answered from the line"
    );
    assert!(app.flow_proposal.is_none(), "and nothing proposed");
    assert!(app.asking.is_none());
    token.cancel();
}

#[tokio::test]
async fn the_line_hint_says_what_words_do() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    type_in(&mut app, "every 2h").await;
    let screen = draw_text(&mut app);
    assert!(screen.contains("… changes hello"), "{screen}");
    app.select(Some(NEW_FLOW.into()));
    let screen = draw_text(&mut app);
    assert!(screen.contains("… makes a new flow"), "{screen}");
    token.cancel();
}

#[tokio::test]
async fn a_change_proposal_does_not_move_on_cursor_moves() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    words_on_hello(&tmp, &mut app, &folder, "every 2h").await;
    assert_eq!(app.keys(), ["f:alpha", "f:hello", NEW_FLOW]);
    for code in [KeyCode::Down, KeyCode::Up, KeyCode::Up] {
        press(&mut app, code, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some("f:hello"), "while it is asked");
    }
    answer_md(&mut app, None, &format!("{}\nEvery two hours.\n", md_in(&folder))).await;
    assert!(app.flow_proposal_shown().is_some());
    for code in [KeyCode::Down, KeyCode::Up, KeyCode::Up] {
        press(&mut app, code, KeyModifiers::NONE).await;
        assert_eq!(app.sel.as_deref(), Some("f:hello"), "once it is proposed");
    }
    assert!(app.flow_proposal_shown().is_some(), "still up");
    token.cancel();
}
