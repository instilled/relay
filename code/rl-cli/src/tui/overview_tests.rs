//! A project's overview: `enter` on its line opens its flows, under what was said of it, with
//! `+ new flow` last; `esc` goes back to the line. A real `rld` over a fresh home.

use super::flow_proposal::Aim;
use super::flow_proposal_tests::{answer_md, said, unasked};
use super::flow_tests::{fired, flow_in};
use super::overview::Scope;
use super::tests::{Setup, draw_text, panel, panel_set, panel_with_config, panel_with_fake_summariser, press, type_in};
use super::{NEW_FLOW, PARKED, Summarising, View, line};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};
use rl_proto::flow::{FiringStatus, FlowApproval, FlowApprove, FlowName, FlowPark, FlowStatus, StepStatus};
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
    // Nothing kept, and nothing written by itself: the top says how to have it.
    let (tmp, token, mut app, folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
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
    // A list search is set aside while the overview is up, and given back on esc.
    app.toggle_events().await;
    app.filter_aside = "zzzz".into();
    app.show_flow("zulu").await;
    assert!(app.view == View::Overview);
    assert!(app.filter.is_empty(), "the overview opens unsearched: {:?}", app.filter);
    assert_eq!(app.sel.as_deref(), Some("f:zulu"));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert_eq!(app.filter, "zzzz");
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
async fn opening_the_overview_on_a_flow_notes_where_its_unseen_began() {
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
    // An older note, say a catch-up's: opening on alpha writes over it.
    app.unseen_from.insert(
        "f:alpha".into(),
        jiff::Timestamp::now() - jiff::SignedDuration::from_hours(240),
    );
    enter_project(&mut app, &folder).await;
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    let began = app
        .digest
        .items
        .iter()
        .find(|i| matches!(&i.run.cause, rl_proto::types::Cause::Flow { flow, .. } if flow == "alpha"))
        .expect("alpha's run")
        .run
        .started_at;
    assert_eq!(app.unseen_from["f:alpha"], began);
    app.refresh().await;
    assert!(needs(&app), "noted, not marked seen");
    token.cancel();
}

#[tokio::test]
async fn a_list_search_stays_on_the_list() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    app.filter = "zzzz".into();
    app.select(Some(format!("g:{}", folder.display())));
    // `enter` cannot reach a line the search hides: opened directly.
    app.open_overview(Scope::Project(folder.clone()), None);
    assert!(app.filter.is_empty(), "the overview opens unsearched: {:?}", app.filter);
    assert_eq!(app.keys(), ["f:alpha", NEW_FLOW]);
    assert_eq!(app.sel.as_deref(), Some("f:alpha"));
    // One esc leaves, and the list has its search back.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert_eq!(app.filter, "zzzz");
    token.cancel();
}

#[tokio::test]
async fn a_summary_over_another_stretch_leaves_the_overviews_alone() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    let now = jiff::Timestamp::now();
    let away = now - jiff::SignedDuration::from_hours(1);
    app.away_since = Some(away);
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    let mut three = said_of(&target, "- three days");
    three.from = Some(now - jiff::SignedDuration::from_hours(72));
    app.made.insert(target.key(), three);
    enter_project(&mut app, &folder).await;
    let o = app.overviewing.as_ref().expect("a new one is asked");
    assert_eq!(o.from, Some(away), "the overview's own stretch, not /summary's");
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

/// The draft in the pane on screen a moment, to its end, and `enter`.
async fn saved(app: &mut super::App) {
    super::flow_proposal_tests::on_screen(app);
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

#[tokio::test]
async fn words_on_a_flow_answered_and_entered_change_it_and_the_cursor_stays_on_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    approve(&mut app, "hello").await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    type_in(&mut app, "say why").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    let changed = format!("{}\nSays why.\n", md_in(&folder));
    answer_md(&mut app, None, &changed).await;
    saved(&mut app).await;
    assert_eq!(said(&app), "hello approved");
    let md = app.home.flow_dir("hello").join("flow.md");
    assert_eq!(std::fs::read_to_string(md).unwrap(), changed);
    assert!(app.flows.iter().any(|f| f.name == "hello" && f.approved()));
    assert!(app.view == View::Overview && app.flow_proposal.is_none() && !app.show_pane);
    assert_eq!(app.sel.as_deref(), Some("f:hello"));
    app.settle();
    assert_eq!(app.sel.as_deref(), Some("f:hello"));
    token.cancel();
}

#[tokio::test]
async fn words_on_new_flow_answered_and_entered_make_it_and_the_cursor_lands_on_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some(NEW_FLOW.into()));
    type_in(&mut app, "probe it").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    answer_md(&mut app, Some("zeta"), &md_in(&folder)).await;
    saved(&mut app).await;
    assert_eq!(said(&app), "zeta approved");
    assert!(app.home.flow_dir("zeta").join("flow.md").exists());
    assert!(app.view == View::Overview && app.flow_proposal.is_none());
    assert_eq!(app.sel.as_deref(), Some("f:zeta"));
    // The next tick's fix-up leaves it there.
    app.settle();
    assert_eq!(app.sel.as_deref(), Some("f:zeta"));
    token.cancel();
}

#[tokio::test]
async fn a_new_flow_in_another_place_says_where_it_went() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some(NEW_FLOW.into()));
    type_in(&mut app, "probe it").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    // The answer has no `in`: the flow is everywhere's, not this project's.
    answer_md(&mut app, Some("loose"), "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    saved(&mut app).await;
    assert_eq!(said(&app), "loose approved · it is in everywhere");
    assert!(app.view == View::Overview);
    assert!(app.sel.as_ref().is_some_and(|k| app.keys().contains(k)));
    token.cancel();
}

#[tokio::test]
async fn words_on_a_flow_whose_file_cant_be_read_stay_on_the_line() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    std::fs::remove_file(app.home.flow_dir("hello").join("flow.md")).unwrap();
    type_in(&mut app, "every 2h").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(
        said(&app).starts_with("hello's flow.md can't be read: "),
        "{}",
        said(&app)
    );
    assert_eq!(app.line.text(), "every 2h", "the words are kept");
    assert!(app.flow_proposal.is_none() && app.asking.is_none());
    token.cancel();
}

#[tokio::test]
async fn typing_is_a_flows_only_on_a_flow_or_new_flow() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    let place = |words| Some(super::shortcuts::Place::Overview { words });
    app.select(Some("f:hello".into()));
    assert_eq!(app.shortcuts_place(), place(true));
    app.select(Some(NEW_FLOW.into()));
    assert_eq!(app.shortcuts_place(), place(true));
    // On no flow, words are words as anywhere.
    app.select(None);
    assert_eq!(app.shortcuts_place(), place(false));
    token.cancel();
}

/// `name` parked by the daemon, and the panel told, as `^x` twice leaves it.
async fn parked(app: &mut super::App, name: &str) {
    app.rpc.call::<FlowPark>(FlowName { name: name.into() }).await.unwrap();
    app.refresh().await;
    app.fetch_parked().await;
}

#[tokio::test]
async fn ctrl_x_twice_on_a_flow_parks_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "press ^x again to park hello");
    assert!(app.home.flow_dir("hello").exists(), "only armed");
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    // It never ran: nothing of it was archived, and nothing is said of it.
    assert_eq!(said(&app), "hello parked · /flow unpark hello brings it back");
    assert!(!app.keys().iter().any(|k| k == "f:hello"), "{:?}", app.keys());
    assert_eq!(app.keys(), [NEW_FLOW, PARKED]);
    let frame = draw_text(&mut app);
    assert!(frame.contains("parked: 1"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn the_parked_line_counts_only_this_scopes_flows() {
    let (tmp, token, mut app, folder) = panel().await;
    let other = std::fs::canonicalize(tmp.path()).unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    flow_in(&tmp, &mut app, "here", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "there", &md_in(&other)).await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    for name in ["here", "there", "loose"] {
        parked(&mut app, name).await;
    }
    // One left with no `in`, so the `everywhere` line stands.
    flow_in(&tmp, &mut app, "stays", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    let names = |app: &super::App| app.parked_in_scope().iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&app), ["here"]);
    let frame = draw_text(&mut app);
    assert!(frame.contains("parked: 1"), "{frame}");
    // Everywhere's is the one with no `in`.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.overview, Some(Scope::Everywhere));
    assert_eq!(names(&app), ["loose"]);
    let frame = draw_text(&mut app);
    assert!(frame.contains("parked: 1"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn no_parked_line_with_none_parked() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    assert_eq!(app.keys(), ["f:hello", NEW_FLOW]);
    let frame = draw_text(&mut app);
    assert!(!frame.contains("parked:"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn enter_on_parked_folds_them_out_and_in() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    parked(&mut app, "hello").await;
    enter_project(&mut app, &folder).await;
    app.select(Some(PARKED.into()));
    // Neither the line nor its flows take words, nor `^x`.
    assert!(!app.words_only());
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "already parked");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.keys(), [NEW_FLOW, PARKED, "parked:_hello"]);
    let frame = draw_text(&mut app);
    assert!(frame.contains("parked: 1 ▾"), "{frame}");
    let row = frame.lines().find(|l| l.contains("hello  parked ")).expect("its row");
    assert!(row.contains("_hello"), "{row}");
    app.select(Some("parked:_hello".into()));
    assert!(!app.words_only());
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "already parked");
    app.select(Some(PARKED.into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.keys(), [NEW_FLOW, PARKED]);
    let frame = draw_text(&mut app);
    assert!(!frame.contains("hello  parked "), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn enter_on_a_parked_flow_unparks_it() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    parked(&mut app, "hello").await;
    enter_project(&mut app, &folder).await;
    app.select(Some(PARKED.into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    app.select(Some("parked:_hello".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello back · it needs a yes: enter on it");
    assert_eq!(app.sel.as_deref(), Some("f:hello"));
    let hello = app.flows.iter().find(|f| f.name == "hello").expect("a flow again");
    assert_eq!(hello.status(), FlowStatus::NeedsApproval);
    assert!(!app.keys().iter().any(|k| k == PARKED), "{:?}", app.keys());
    app.settle();
    assert_eq!(app.sel.as_deref(), Some("f:hello"));
    token.cancel();
}

#[tokio::test]
async fn a_flow_unparked_into_another_place_says_where_it_went() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    parked(&mut app, "loose").await;
    enter_project(&mut app, &folder).await;
    type_in(&mut app, "/flow unpark loose").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        said(&app),
        "loose back · it needs a yes: enter on it · it is in everywhere"
    );
    assert!(app.view == View::Overview);
    token.cancel();
}

#[tokio::test]
async fn flow_park_and_unpark_commands() {
    let (tmp, token, mut app, folder) = panel().await;
    let flows = app.home.flows_dir();
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    // On the flow, `/flow park` parks it.
    app.select(Some("f:hello".into()));
    type_in(&mut app, "/flow park").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    // It never ran: nothing of it was archived, and nothing is said of it.
    assert_eq!(said(&app), "hello parked · /flow unpark hello brings it back");
    assert!(flows.join("_hello").is_dir());
    // By name, twice more: each to a folder of its own.
    for n in [2, 3] {
        flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
        app.refresh().await;
        type_in(&mut app, "/flow park hello").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(flows.join(format!("_hello.{n}")).is_dir(), "{}", said(&app));
    }
    // A name brings back its newest.
    type_in(&mut app, "/flow unpark hello").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello back · it needs a yes: enter on it");
    assert!(!flows.join("_hello.3").exists() && flows.join("hello").is_dir());
    assert!(flows.join("_hello").is_dir() && flows.join("_hello.2").is_dir());
    // A folder brings back that one.
    type_in(&mut app, "/flow park hello").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    type_in(&mut app, "/flow unpark _hello.2").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello back · it needs a yes: enter on it");
    assert!(!flows.join("_hello.2").exists() && flows.join("hello").is_dir());
    assert!(flows.join("_hello").is_dir() && flows.join("_hello.3").is_dir());
    // Nothing parked by a name says so.
    type_in(&mut app, "/flow unpark nope").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "nothing parked as nope · rl flow parked lists them");
    token.cancel();
}

#[tokio::test]
async fn slash_archive_on_a_flow_still_archives_its_runs() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    approve(&mut app, "hello").await;
    fired(
        &tmp,
        &folder,
        "hello",
        "fire0001",
        &[("probe", StepStatus::Done, Some("found\r\n"))],
        FiringStatus::Done,
    );
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    assert!(app.selected_flow().is_some_and(|f| !f.runs.is_empty()));
    type_in(&mut app, "/archive").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "archived hello · 1 runs");
    assert!(app.home.flow_dir("hello").is_dir(), "not parked");
    assert!(app.parked.is_empty());
    assert!(app.keys().iter().any(|k| k == "f:hello"));
    token.cancel();
}

#[tokio::test]
async fn parking_a_flow_from_its_page_closes_it_and_moves_on() {
    let (tmp, token, mut app, folder) = panel().await;
    for name in ["hello", "zeta"] {
        flow_in(&tmp, &mut app, name, &md_in(&folder)).await;
        app.refresh().await;
        approve(&mut app, name).await;
    }
    fired(
        &tmp,
        &folder,
        "hello",
        "fire0001",
        &[("probe", StepStatus::Done, Some("found\r\n"))],
        FiringStatus::Done,
    );
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    assert_eq!(app.keys(), ["f:hello", "f:zeta", NEW_FLOW]);
    app.select(Some("f:hello".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.show_pane, "its page");
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(
        said(&app),
        "hello parked · its runs archived · /flow unpark hello brings it back"
    );
    assert!(!app.show_pane, "back to the overview");
    assert_eq!(app.sel.as_deref(), Some("f:zeta"), "the next flow down");
    // The last flow parked: `+ new flow`.
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "zeta parked · /flow unpark zeta brings it back");
    assert_eq!(app.sel.as_deref(), Some(NEW_FLOW));
    token.cancel();
}

#[tokio::test]
async fn ctrl_x_armed_on_one_line_does_nothing_once_the_cursor_moved() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "alpha", &md_in(&folder)).await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    app.refresh().await;
    enter_project(&mut app, &folder).await;
    app.select(Some("f:hello".into()));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "press ^x again to park hello");
    // A refresh moves the cursor between the presses.
    app.select(Some("f:alpha".into()));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "the line moved · ^x again");
    assert!(app.arm.is_none());
    assert!(app.home.flow_dir("alpha").is_dir() && app.home.flow_dir("hello").is_dir());
    // A run's arm the same: on the list, armed on the run, then off it.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    app.select(Some("r:run00001".into()));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert!(app.arm.is_some(), "{}", said(&app));
    app.select(Some(format!("g:{}", folder.display())));
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).await;
    assert_eq!(said(&app), "the line moved · ^x again");
    assert!(app.digest.items.iter().any(|i| i.run.id == "run00001"), "not archived");
    token.cancel();
}

#[tokio::test]
async fn a_scope_whose_flows_are_all_parked_keeps_its_line() {
    let (tmp, token, mut app, _folder) = panel().await;
    let other = std::fs::canonicalize(tmp.path()).unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    flow_in(&tmp, &mut app, "there", &md_in(&other)).await;
    app.refresh().await;
    for name in ["loose", "there"] {
        app.rpc.call::<FlowPark>(FlowName { name: name.into() }).await.unwrap();
    }
    // On the list, from a refresh alone: no overview fetched them.
    app.refresh().await;
    assert!(app.view == View::Work);
    let other_line = format!("g:{}", other.display());
    let keys = app.keys();
    assert!(keys.iter().any(|k| k == "g:everywhere"), "{keys:?}");
    assert!(keys.contains(&other_line), "an unregistered `in` too: {keys:?}");
    let frame = draw_text(&mut app);
    let line = frame.lines().find(|l| l.contains("everywhere")).expect("its line");
    assert!(line.contains("1 parked"), "{line}");
    // And its overview has them.
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.keys(), [NEW_FLOW, PARKED]);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    app.select(Some(other_line));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.overview, Some(Scope::Project(other.clone())));
    assert_eq!(app.keys(), [NEW_FLOW, PARKED]);
    token.cancel();
}

/// Hands the overview's summary, once it is written, to the panel as the main loop would.
async fn wait_overviewed(app: &mut super::App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while let Some(o) = app.overviewing.as_mut() {
        if o.task.is_finished() {
            let done = (&mut o.task).await;
            app.overviewed(done).await;
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the summary never came");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A summary of the project kept from `from`, saying `text`, reaching `reach`: kept as it is, or
/// moved on since when the runs from `from` reach further.
fn keep(tmp: &tempfile::TempDir, folder: &Path, from: jiff::Timestamp, reach: &str, text: &str) {
    keep_at(tmp, folder, from, reach, text, jiff::Timestamp::now());
}

/// `keep`, as written at `at`.
fn keep_at(
    tmp: &tempfile::TempDir,
    folder: &Path,
    from: jiff::Timestamp,
    reach: &str,
    text: &str,
    at: jiff::Timestamp,
) {
    let target = SummaryTarget::Project {
        workspace: folder.to_path_buf(),
    };
    let mut kept = said_of(&target, text);
    kept.from = Some(from);
    kept.at = at;
    kept.reach = reach.into();
    let ledger = rl_ledger::Ledger::open(&rl_core::Home::at(tmp.path().join("h")).ledger()).unwrap();
    ledger.call_blocking(move |c| rl_ledger::kept::put(c, &kept)).unwrap();
}

#[tokio::test]
async fn opening_writes_the_summary_when_none_is_kept() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    enter_project(&mut app, &folder).await;
    let o = app.overviewing.as_ref().expect("auto, and named: it is written");
    assert_eq!(
        o.target,
        SummaryTarget::Project {
            workspace: folder.clone()
        }
    );
    assert_eq!((o.title.as_str(), o.deep), ("api", false));
    let frame = draw_text(&mut app);
    assert!(frame.contains("summarising… "), "{frame}");
    assert!(!frame.contains("no summary yet"), "{frame}");
    wait_overviewed(&mut app).await;
    let frame = draw_text(&mut app);
    assert!(frame.contains("the build went green"), "{frame}");
    assert!(!frame.contains("summarising… "), "{frame}");
    let said = app.overview_said.as_ref().expect("its top");
    assert!(said.text.contains("the build went green"), "{said:?}");
    token.cancel();
}

#[tokio::test]
async fn a_fresh_kept_summary_is_shown_not_rewritten() {
    let (tmp, token, mut app, folder) = panel().await;
    // From after the project's one run: nothing since, so the one kept reaches all there is.
    let from = jiff::Timestamp::now();
    keep(&tmp, &folder, from, "n:0:0:0", "- nothing moved");
    app.away_since = Some(from);
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "kept and current: no one is asked");
    let frame = draw_text(&mut app);
    assert!(frame.contains("nothing moved"), "{frame}");
    assert!(!frame.contains("summarising"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn a_stale_kept_summary_is_shown_while_a_new_one_is_written() {
    let (tmp, token, mut app, folder) = panel().await;
    let from = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    // Kept before the project's run: moved on since, and long enough ago to be written again.
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(16);
    keep_at(&tmp, &folder, from, "", "- the deploy went out", at);
    app.away_since = Some(from);
    enter_project(&mut app, &folder).await;
    let o = app.overviewing.as_ref().expect("moved on: a new one is asked");
    assert_eq!(o.from, Some(from));
    let frame = draw_text(&mut app);
    let progress = frame.find("summarising… ").unwrap_or_else(|| panic!("{frame}"));
    let old = frame.find("the deploy went out").unwrap_or_else(|| panic!("{frame}"));
    assert!(progress < old, "{frame}");
    token.cancel();
}

#[tokio::test]
async fn a_stale_summary_kept_a_minute_ago_is_shown_not_rewritten() {
    let (tmp, token, mut app, folder) = panel().await;
    let from = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(1);
    keep_at(&tmp, &folder, from, "", "- the deploy went out", at);
    app.away_since = Some(from);
    enter_project(&mut app, &folder).await;
    assert!(
        app.overviewing.is_none(),
        "moved on, but kept a minute ago: no one is asked"
    );
    let frame = draw_text(&mut app);
    assert!(frame.contains("the deploy went out"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn no_auto_no_write() {
    let (_tmp, token, mut app, folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "summary: ask");
    let frame = draw_text(&mut app);
    assert!(frame.contains(" no summary yet · /summary writes it"), "{frame}");
    token.cancel();
    let setup = Setup {
        unnamed: true,
        ..Default::default()
    };
    let (_tmp, token, mut app, folder) = panel_set(setup, |_, _| vec![]).await;
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "no assistant named for summaries");
    let frame = draw_text(&mut app);
    assert!(frame.contains(" no summary yet · /summary writes it"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn reopening_while_writing_starts_no_second() {
    let (_tmp, token, mut app, folder) = panel().await;
    enter_project(&mut app, &folder).await;
    let asked = app.overviewing.as_ref().expect("written on open").since;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.view == View::Work);
    assert!(app.overviewing.is_some(), "leaving lets it go on");
    enter_project(&mut app, &folder).await;
    assert_eq!(app.overviewing.as_ref().map(|o| o.since), Some(asked), "the same one");
    // And `/summary` of the project waits for it.
    assert!(app.on_new_flow());
    type_in(&mut app, "/summary").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.summarising.is_none());
    let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
    assert_eq!(said, "still summarising api");
    token.cancel();
}

#[tokio::test]
async fn a_summary_landing_after_leaving_is_kept_not_shown() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_some());
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    wait_overviewed(&mut app).await;
    let key = SummaryTarget::Project {
        workspace: folder.clone(),
    }
    .key();
    assert!(
        app.made
            .get(&key)
            .is_some_and(|s| s.text.contains("the build went green"))
    );
    assert!(app.overview_said.is_none(), "not the overview's top: it was left");
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "kept: no one is asked again");
    let frame = draw_text(&mut app);
    assert!(frame.contains("the build went green"), "{frame}");
    token.cancel();
}

#[tokio::test]
async fn everywhere_writes_nothing() {
    let (tmp, token, mut app, _folder) = panel().await;
    flow_in(&tmp, &mut app, "loose", "---\nsteps:\n  probe:\n    run: x\n---\n").await;
    app.refresh().await;
    app.select(Some("g:everywhere".into()));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.overview, Some(Scope::Everywhere));
    assert!(app.overviewing.is_none());
    let frame = draw_text(&mut app);
    assert!(frame.contains("flows with no in"), "{frame}");
    assert!(!frame.contains("summarising"), "{frame}");
    token.cancel();
}

/// A second project, `zed`, registered beside `api`: its folder.
async fn zed(tmp: &tempfile::TempDir, app: &mut super::App) -> std::path::PathBuf {
    let zed = std::fs::canonicalize(tmp.path()).unwrap().join("zed");
    std::fs::create_dir_all(&zed).unwrap();
    app.rpc
        .call::<super::WorkspaceUp>(super::WorkspaceUpParams { path: zed.clone() })
        .await
        .unwrap();
    app.refresh().await;
    zed
}

#[tokio::test]
async fn another_projects_waits_for_the_one_being_written_then_starts() {
    let Some((tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    let other = zed(&tmp, &mut app).await;
    enter_project(&mut app, &folder).await;
    let asked = app.overviewing.as_ref().expect("api's is written").since;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    enter_project(&mut app, &other).await;
    let o = app.overviewing.as_ref().expect("api's goes on");
    assert_eq!((o.since, o.title.as_str()), (asked, "api"), "not aborted, not replaced");
    let zed_target = SummaryTarget::Project {
        workspace: other.clone(),
    };
    assert_eq!(app.overview_next.as_ref(), Some(&zed_target), "zed's waits");
    let frame = draw_text(&mut app);
    assert!(frame.contains(" summarising api… · this one next"), "{frame}");
    wait_overviewed(&mut app).await;
    let api = SummaryTarget::Project { workspace: folder }.key();
    assert!(app.made.contains_key(&api), "api's landed into made");
    let o = app.overviewing.as_ref().expect("zed's starts once api's lands");
    assert_eq!(o.target, zed_target);
    assert!(app.overview_next.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_failed_write_is_not_asked_again_until_a_summary_there() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("").await else {
        return;
    };
    enter_project(&mut app, &folder).await;
    wait_overviewed(&mut app).await;
    let said = app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
    assert!(said.starts_with("the summary: "), "{said}");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    app.flash = None;
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "not asked again by itself");
    assert!(app.flash.is_none(), "and said once");
    // A `/summary` there lets it be asked again.
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    app.summary_start(rl_proto::methods::SummaryParams {
        target: target.clone(),
        deep: false,
        from: Some(jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24)),
        again: false,
        dry: false,
        timeout: None,
    });
    assert!(!app.overview_failed.contains(&target.key()));
    token.cancel();
}

#[tokio::test]
async fn a_second_open_finds_the_one_made_and_writes_nothing() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    enter_project(&mut app, &folder).await;
    wait_overviewed(&mut app).await;
    assert!(app.overviewing.is_none());
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "made this session: nothing written");
    assert!(draw_text(&mut app).contains("the build went green"));
    token.cancel();
}

#[tokio::test]
async fn a_current_one_kept_over_another_stretch_stands_for_a_day() {
    let (tmp, token, mut app, folder) = panel().await;
    // Kept from after the project's run, by an earlier session: nothing since.
    let earlier = jiff::Timestamp::now();
    keep(&tmp, &folder, earlier, "n:0:0:0", "- nothing moved");
    // This session starts from elsewhere: none kept from there.
    app.away_since = Some(earlier + jiff::SignedDuration::from_mins(1));
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_none(), "the newest kept is current: no one is asked");
    assert!(draw_text(&mut app).contains("nothing moved"));
    token.cancel();

    // Written more than a day ago, it is shown while a new one is written.
    let (tmp, token, mut app, folder) = panel().await;
    let earlier = jiff::Timestamp::now();
    let old = earlier - jiff::SignedDuration::from_hours(25);
    keep_at(&tmp, &folder, earlier, "n:0:0:0", "- nothing moved", old);
    app.away_since = Some(earlier + jiff::SignedDuration::from_mins(1));
    enter_project(&mut app, &folder).await;
    assert!(app.overviewing.is_some(), "over a day old: written again");
    assert!(draw_text(&mut app).contains("nothing moved"));
    token.cancel();
}

#[tokio::test]
async fn landing_leaves_a_deep_summary_in_the_subpage() {
    let (_tmp, token, mut app, folder) = panel_with_config("catch_up:\n  summary: ask\n").await;
    enter_project(&mut app, &folder).await;
    assert!(app.on_new_flow());
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    let mut deep = said_of(&target, "- read closely");
    deep.deep = true;
    app.summary = Some(deep);
    let brief = said_of(&target, "- the build went green");
    let landed = brief.clone();
    app.overviewing = Some(Summarising {
        target: target.clone(),
        title: "api".into(),
        deep: false,
        from: None,
        since: std::time::Instant::now(),
        task: tokio::spawn(async move { Ok(landed) }),
    });
    wait_overviewed(&mut app).await;
    assert!(app.summary.as_ref().is_some_and(|s| s.deep), "the deep one stays");
    assert_eq!(
        app.made.get(&target.key()).map(|s| s.text.as_str()),
        Some(brief.text.as_str())
    );
    assert_eq!(
        app.overview_said.as_ref().map(|s| s.text.as_str()),
        Some(brief.text.as_str())
    );
    token.cancel();
}

#[tokio::test]
async fn a_deep_summary_made_of_the_project_is_not_replaced_by_the_brief() {
    let Some((_tmp, token, mut app, folder)) = panel_with_fake_summariser("- the build went green").await else {
        return;
    };
    let target = SummaryTarget::Project {
        workspace: folder.clone(),
    };
    let mut deep = said_of(&target, "- every detail");
    deep.deep = true;
    app.made.insert(target.key(), deep);
    enter_project(&mut app, &folder).await;
    wait_overviewed(&mut app).await;
    let kept = app.made.get(&target.key()).expect("still made");
    assert!(kept.deep && kept.text.contains("every detail"), "{kept:?}");
    let said = app.overview_said.as_ref().expect("the brief is its top");
    assert!(said.text.contains("the build went green"), "{said:?}");
    token.cancel();
}

#[tokio::test]
async fn an_unpark_whose_folder_came_back_but_did_not_load_says_so_and_reads_again() {
    let (tmp, token, mut app, folder) = panel().await;
    flow_in(&tmp, &mut app, "hello", &md_in(&folder)).await;
    parked(&mut app, "hello").await;
    std::fs::write(
        app.home.flows_dir().join("_hello").join("flow.md"),
        "---\nsteps: [\n---\n",
    )
    .unwrap();
    enter_project(&mut app, &folder).await;
    type_in(&mut app, "/flow unpark hello").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let said = said(&app);
    assert!(
        said.starts_with("hello is back in flows/hello but did not load: "),
        "{said}"
    );
    assert!(said.ends_with(" · ^e on it, or /flow reload"), "{said}");
    assert!(app.parked.is_empty(), "no longer listed as parked: {:?}", app.parked);
    token.cancel();
}
