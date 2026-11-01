//! The panel writing flows: `/flow new` from the bare template, and a drafted flow
//! shown in the flow proposal's pane, then written with its yes, opened first, or let go. A real
//! `rld` over a fresh home; no agent is asked — a draft is handed to the panel as the assistant's
//! answer would hand it.

use super::flow_proposal::Aim;
use super::flow_proposal_tests::{answer_md, ask_for, on_screen, proposed, run, said};
use super::tests::{draw_text, panel, press, type_in};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";

#[tokio::test]
async fn flow_new_opens_the_bare_template_and_its_save_is_the_yes() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    run(&mut app, "/flow new logs").await;
    let (editing, line) = app.editor.take().expect("the editor is next");
    assert_eq!(line, 1);
    // Back from the editor, saved changed: it loads, and it has its yes.
    std::fs::write(&editing.file, HELLO).unwrap();
    app.edited(editing).await;
    assert_eq!(said(&app), "logs approved");
    assert!(app.flows.iter().any(|f| f.name == "logs" && f.approved()));

    run(&mut app, "/flow new logs").await;
    assert_eq!(said(&app), "`logs` is a flow already; rl flow edit logs opens it");
    run(&mut app, "/flow new").await;
    assert_eq!(said(&app), super::new_flow::USAGE);

    // The bare template, left as it was, is not kept.
    run(&mut app, "/flow new pulse").await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    app.edited(editing).await;
    assert_eq!(said(&app), "pulse left as the template: not kept");
    assert!(!home.flow_dir("pulse").exists());
    token.cancel();
}

#[tokio::test]
async fn a_drafted_flow_is_shown_then_written_with_its_yes() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));

    // Words go to the assistant, asked for that flow by name.
    run(&mut app, "/flow new brief what my team did yesterday").await;
    let asking = app.asking.take().expect("asked");
    assert_eq!(asking.want, Want::Flow { name: "brief".into() });
    assert_eq!(asking.words, "what my team did yesterday");
    asking.task.abort();

    // Its answer, shown; nothing written yet.
    proposed(&mut app, "hello", HELLO).await;
    let screen = on_screen(&mut app);
    assert!(screen.contains("hello  drafted by fakeai"), "{screen}");
    assert!(screen.contains("run: echo hello"), "{screen}");
    assert!(!home.flow_dir("hello").exists());
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    assert_eq!(
        std::fs::read_to_string(home.flow_dir("hello").join("flow.md")).unwrap(),
        HELLO
    );
    assert!(app.flow_proposal.is_none());

    // One that would not load is not written on enter; esc lets it go.
    proposed(
        &mut app,
        "broken",
        "---\nsteps:\n  a:\n    after: nope\n    run: x\n---\n",
    )
    .await;
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        said(&app),
        "it would not load: flow.md:3 · `a` comes after `nope`, which is no step · ^e opens it in $EDITOR"
    );
    assert!(!home.flow_dir("broken").exists());
    assert!(app.flow_proposal_shown().is_some(), "still shown");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "dropped it · nothing written");
    assert!(app.flow_proposal.is_none());

    // `^e` writes it and opens it, for the yes on return.
    proposed(&mut app, "later", HELLO).await;
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    assert_eq!(editing.file, home.flow_dir("later").join("flow.md"));
    app.edited(editing).await;
    assert_eq!(said(&app), "later approved");

    // A name that is taken is a problem of the draft's, said before anything is written.
    proposed(&mut app, "hello", HELLO).await;
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(
        said(&app).starts_with("it would not load: `hello` is a flow already"),
        "{}",
        said(&app)
    );
    token.cancel();
}

#[tokio::test]
async fn a_draft_taller_than_its_pane_is_written_once_its_end_was_shown() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let mut md = HELLO.to_string();
    for i in 1..=80 {
        md.push_str(&format!("Line {i} of what it does.\n"));
    }
    proposed(&mut app, "tall", &md).await;
    let screen = on_screen(&mut app);
    assert!(!screen.contains("Line 80"), "{screen}");
    // Its rows are counted as it is drawn: the footer says so from the next frame on.
    let screen = draw_text(&mut app);
    assert!(screen.contains("↑↓ pgup/pgdn read it"), "the footer: {screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "the end is below · ↓ to read it");
    assert!(!home.flow_dir("tall").exists());
    assert!(app.flow_proposal_shown().is_some(), "still shown");

    // Read down to its end: then enter writes it.
    for _ in 0..80 {
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        draw_text(&mut app);
    }
    assert!(draw_text(&mut app).contains("Line 80 of what it does."));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "tall approved");
    assert_eq!(
        std::fs::read_to_string(home.flow_dir("tall").join("flow.md")).unwrap(),
        md
    );
    token.cancel();
}

#[tokio::test]
async fn a_draft_written_without_its_yes_is_taken_away_again() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    on_screen(&mut app);
    // No daemon to load it and say yes: what was written goes again.
    token.cancel();
    let deadline = Instant::now() + Duration::from_secs(10);
    while crate::client::Client::connect(&home).await.is_ok() {
        assert!(Instant::now() < deadline, "the daemon never went");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(said(&app).starts_with("write hello: "), "{}", said(&app));
    assert!(said(&app).ends_with(" · nothing kept"), "{}", said(&app));
    assert!(!home.flow_dir("hello").exists());
    assert!(app.flow_proposal_shown().is_some(), "still shown");
}

#[tokio::test]
async fn a_draft_with_no_yes_in_a_folder_of_yours_leaves_only_what_was_yours() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let dir = home.flow_dir("hello");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    proposed(&mut app, "hello", HELLO).await;
    on_screen(&mut app);
    // The folder never had a yes: none is given, and what was written goes again — each time.
    for _ in 0..2 {
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(
            said(&app),
            "hello was not said yes to: its folder changed since its last yes · nothing kept"
        );
        assert!(!dir.join("flow.md").exists());
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "mine");
        assert!(app.flow_proposal_shown().is_some(), "still shown");
    }
    token.cancel();
}

#[tokio::test]
async fn a_flow_answered_is_shown_as_a_draft_named_by_whoever_asked() {
    let (_tmp, token, mut app, _) = panel().await;
    // Asked for by name: the person's name wins the draft's.
    let named = Aim::New {
        named: Some("mine".into()),
        folder: None,
    };
    ask_for(&mut app, named, "say hello");
    answer_md(&mut app, Some("theirs"), HELLO).await;
    let d = app.flow_proposal_shown().and_then(|p| p.draft.as_ref()).expect("shown");
    assert_eq!(d.name, "mine");
    // Asked for in plain words: the draft's.
    app.flow_proposal = None;
    app.propose("say hello".into());
    app.asking = Some(super::Asking {
        words: "say hello".into(),
        want: Want::Any,
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    answer_md(&mut app, Some("theirs"), HELLO).await;
    let d = app.flow_proposal_shown().and_then(|p| p.draft.as_ref()).expect("shown");
    assert_eq!(d.name, "theirs");
    token.cancel();
}

#[tokio::test]
async fn a_draft_that_comes_while_the_keys_are_anothers_leaves_them_be() {
    let (_tmp, token, mut app, _) = panel().await;
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    run(&mut app, "/new").await;
    type_in(&mut app, "make").await;
    assert!(matches!(app.mode, super::Mode::Form(_)));
    // It comes mid-form: the form stays, its keys its own, and the draft is in the pane.
    answer_md(&mut app, Some("first"), HELLO).await;
    let super::Mode::Form(f) = &app.mode else {
        panic!("the form is left as it was");
    };
    assert_eq!(f.get("run"), "make");
    assert!(app.flow_proposal.as_ref().is_some_and(|p| p.draft.is_some()));
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(matches!(app.mode, super::Mode::Normal));

    // Words half typed when it comes are words still: enter would ask with them, not write it.
    app.propose_flow(
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello again".into(),
    );
    super::flow_proposal_tests::unasked(&mut app);
    type_in(&mut app, "half typed").await;
    answer_md(&mut app, Some("second"), HELLO).await;
    on_screen(&mut app);
    assert_eq!(app.line.text(), "half typed");
    assert_eq!(app.line_what(), super::line::What::Ask("half typed".into()));
    let d = app.flow_proposal.as_ref().and_then(|p| p.draft.as_ref()).unwrap();
    assert_eq!(d.name, "second", "the newer draft replaces the one before");
    token.cancel();
}

#[tokio::test]
async fn an_enter_as_a_draft_comes_on_screen_is_not_its_yes() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    draw_text(&mut app);
    // Pressed as it was drawn: ignored, and nothing said.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.flow_proposal_shown().is_some(), "still shown");
    assert_eq!(said(&app), "");
    assert!(!home.flow_dir("hello").exists());
    // A moment later, it is the yes.
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    token.cancel();
}
