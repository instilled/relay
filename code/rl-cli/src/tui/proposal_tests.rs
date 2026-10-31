//! The panel asking for a run: the pane up at once while it is asked, then what the assistant
//! proposed there — run with `enter`, changed with more words, opened as the form with `^e`, let
//! go with `esc`. A real `rld` over a fresh home; no agent is asked — the answer is handed to the
//! panel as the assistant's would be.

use super::tests::{draw_text, panel, panel_with_profiles, press, type_in};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::Duration;

fn said(app: &super::App) -> String {
    app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default()
}

/// Asked: the pane up on the words, as `enter` on them does.
fn ask(app: &mut super::App, words: &str) -> String {
    let sent = app.propose(words.into());
    app.asking = Some(super::Asking {
        words: words.into(),
        want: Want::Any,
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    sent
}

/// The assistant proposing `run`, a moment ago — an `enter` now is meant for it.
async fn answer(app: &mut super::App, run: &str) {
    let crate::interpret::Answer::Run(d) = crate::interpret::answer(&format!("kind: run\nrun: {run}\n")).unwrap()
    else {
        panic!("a run");
    };
    let out = rl_proto::methods::AssistantOutput {
        run: "run00009".into(),
        agent: "fakeai".into(),
        output: String::new(),
        exit: Some(0),
        timed_out: false,
    };
    app.answered(Ok(Ok((crate::interpret::Answer::Run(d), out)))).await;
    if let Some((_, _, at)) = app.proposal.as_mut().and_then(|p| p.draft.as_mut()) {
        *at -= Duration::from_secs(1);
    }
}

#[tokio::test]
async fn the_pane_shows_the_wait_then_the_run_proposed_and_enter_runs_it() {
    // A daemon that runs what it starts.
    let Some((_tmp, token, mut app, _)) = panel_with_profiles(vec![]).await else {
        return;
    };
    ask(&mut app, "say hi");
    assert!(app.show_pane, "up at once");
    let screen = draw_text(&mut app);
    assert!(screen.contains("asking the assistant"), "{screen}");
    assert!(screen.contains("asked    say hi"), "{screen}");

    answer(&mut app, "echo hi").await;
    let screen = draw_text(&mut app);
    assert!(screen.contains("run      echo hi"), "{screen}");
    assert!(screen.contains("enter runs it"), "{screen}");
    assert!(!matches!(app.mode, super::Mode::Form(_)), "not the form at the foot");

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.proposal.is_none(), "{}\n{}", said(&app), draw_text(&mut app));
    assert!(app.sel.as_deref().is_some_and(|s| s.starts_with("r:")), "on its run");
    assert!(said(&app).starts_with("started"), "{}", said(&app));
    token.cancel();
}

#[tokio::test]
async fn words_typed_with_it_up_change_it_even_when_they_start_with_a_command() {
    let (_tmp, token, mut app, _) = panel().await;
    ask(&mut app, "say hi");
    answer(&mut app, "echo hi").await;
    type_in(&mut app, "ls first, then say hi").await;
    assert_eq!(
        app.line_what(),
        super::line::What::Ask("ls first, then say hi".into()),
        "a change, not a shell line"
    );
    assert!(draw_text(&mut app).contains("changes the proposal"));
    app.asking = None;
    app.line.clear();
    let sent = ask(&mut app, "ls first, then say hi");
    assert!(sent.contains("- say hi\n"), "{sent}");
    assert!(sent.contains("  run: echo hi\n"), "{sent}");
    assert!(sent.contains("Change it: ls first, then say hi"), "{sent}");
    let screen = draw_text(&mut app);
    assert!(screen.contains("then     ls first, then say hi"), "{screen}");
    assert!(screen.contains("changing it"), "{screen}");
    token.cancel();
}

#[tokio::test]
async fn esc_leaves_it_asking_and_ask_brings_it_back_then_esc_drops_it() {
    let (_tmp, token, mut app, _) = panel().await;
    ask(&mut app, "say hi");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(!app.show_pane);
    assert!(app.proposal.is_some(), "still asked");
    answer(&mut app, "echo hi").await;
    assert_eq!(said(&app), "fakeai proposed a run · /ask shows it");
    type_in(&mut app, "/ask").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(draw_text(&mut app).contains("run      echo hi"));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.proposal.is_none() && !app.show_pane);
    assert_eq!(said(&app), "dropped it");
    token.cancel();
}

#[tokio::test]
async fn ctrl_e_opens_it_as_the_run_form() {
    let (_tmp, token, mut app, _) = panel().await;
    ask(&mut app, "say hi");
    answer(&mut app, "echo hi").await;
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let super::Mode::Form(f) = &app.mode else {
        panic!("the form: {}", said(&app));
    };
    assert_eq!(f.get("run"), "echo hi");
    assert!(app.proposal.is_none() && !app.show_pane);
    token.cancel();
}

#[tokio::test]
async fn an_ask_that_fails_says_why_in_the_pane() {
    let (_tmp, token, mut app, _) = panel().await;
    ask(&mut app, "say hi");
    app.answered(Ok(Err(anyhow::anyhow!("fakeai took too long")))).await;
    let screen = draw_text(&mut app);
    assert!(screen.contains("✗ fakeai took too long"), "{screen}");
    assert!(screen.contains("type to ask again"), "{screen}");
    token.cancel();
}
