//! The panel asking for a flow: the pane up at once while it is asked, then the flow proposed
//! there — new, or a change drawn against the flow it changes — written with `enter`, changed with
//! more words, opened with `^e`, let go with `esc`. A real `rld` over a fresh home; no agent is
//! asked — the answer is handed to the panel as the assistant's would be.

use super::flow_proposal::Aim;
use super::tests::{draw_text, panel, press, type_in};
use crate::context::Want;
use crate::interpret::{Answer, FlowDraft};
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";
const ANSWER: &str = "kind: flow\nname: hello\n---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n";

pub(super) fn said(app: &super::App) -> String {
    app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default()
}

pub(super) async fn run(app: &mut super::App, line: &str) {
    type_in(app, line).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

/// The assistant's run: what the answer came with.
fn out() -> rl_proto::methods::AssistantOutput {
    rl_proto::methods::AssistantOutput {
        run: "run00009".into(),
        agent: "fakeai".into(),
        output: String::new(),
        exit: Some(0),
        timed_out: false,
    }
}

/// Whatever ask is under way, let go before it reaches the daemon: its answer is the test's to
/// hand in.
pub(super) fn unasked(app: &mut super::App) {
    if let Some(a) = app.asking.as_mut() {
        a.task.abort();
        a.task = tokio::spawn(async { anyhow::bail!("not asked") });
    }
}

/// Asked for a flow: the pane up on the words, as words on a project's line will open it.
pub(super) fn ask_for(app: &mut super::App, aim: Aim, words: &str) {
    app.propose_flow(aim, words.into());
    unasked(app);
}

/// The assistant answering `raw`, as it writes it.
pub(super) async fn answer(app: &mut super::App, raw: &str) {
    let a = crate::interpret::answer(raw).unwrap();
    app.answered(Ok(Ok((a, out())))).await;
}

/// The assistant answering with `text` as a flow's `flow.md`, named `name`.
pub(super) async fn answer_md(app: &mut super::App, name: Option<&str>, text: &str) {
    let draft = FlowDraft {
        name: name.map(str::to_string),
        text: text.into(),
    };
    app.answered(Ok(Ok((Answer::Flow(draft), out())))).await;
}

/// A new flow `name`, proposed as `text` and up in the pane.
pub(super) async fn proposed(app: &mut super::App, name: &str, text: &str) {
    if let Some(a) = app.asking.take() {
        a.task.abort();
    }
    ask_for(
        app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    answer_md(app, Some(name), text).await;
}

/// The screen drawn, and the draft on it there long enough for an enter to be its yes — as if the
/// person had looked a moment, without the test waiting.
pub(super) fn on_screen(app: &mut super::App) -> String {
    let screen = draw_text(app);
    if let Some(d) = app.flow_proposal.as_ref().and_then(|p| p.draft.as_ref()) {
        d.drawn.set(Some(Instant::now() - Duration::from_secs(1)));
    }
    screen
}

/// `hello`, written from `HELLO` and said yes to, as it is on disk: what a change is asked of.
async fn hello_there(app: &mut super::App, home: &rl_core::Home) -> String {
    let name = crate::flow::check_name(home, "hello").unwrap();
    let files = vec![crate::flow::NewFile::flow_md(HELLO)];
    crate::flow::write_new(&mut app.rpc, home, &name, files, &[])
        .await
        .unwrap();
    app.refresh().await;
    assert!(app.flows.iter().any(|f| f.name == "hello" && f.approved()));
    std::fs::read_to_string(home.flow_dir("hello").join("flow.md")).unwrap()
}

fn change(base: &str) -> Aim {
    Aim::Change {
        name: "hello".into(),
        base: base.into(),
    }
}

#[tokio::test]
async fn asking_for_a_new_flow_shows_the_wait() {
    let (_tmp, token, mut app, ws) = panel().await;
    let aim = Aim::New {
        named: None,
        folder: Some(ws.clone()),
    };
    ask_for(&mut app, aim, "say hello hourly");
    assert!(app.show_pane, "up at once");
    let screen = draw_text(&mut app);
    assert!(screen.contains("asked    say hello hourly"), "{screen}");
    assert!(screen.contains("asking the assistant"), "{screen}");
    assert!(screen.contains("flow proposal"), "its title: {screen}");
    assert!(screen.contains("esc leaves it asking"), "the footer: {screen}");
    let asking = app.asking.as_ref().expect("asked");
    assert_eq!(
        asking.want,
        Want::NewFlow {
            folder: Some(ws.clone())
        }
    );
    let (sent, folder, _, _) = app.flow_asking().unwrap();
    assert_eq!(sent, "say hello hourly", "the words alone");
    assert_eq!(folder, ws);
    // One ask at a time.
    app.propose_flow(
        Aim::New {
            named: None,
            folder: None,
        },
        "another".into(),
    );
    assert_eq!(said(&app), "still asking; /stop on its run");
    assert_eq!(app.flow_proposal.as_ref().unwrap().asked, ["say hello hourly"]);
    token.cancel();
}

#[tokio::test]
async fn a_new_flow_answered_shows_its_text_and_saves() {
    let (tmp, token, mut app, ws) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let aim = Aim::New {
        named: None,
        folder: Some(ws),
    };
    ask_for(&mut app, aim, "say hello hourly");
    answer(&mut app, ANSWER).await;
    let screen = on_screen(&mut app);
    assert!(screen.contains(" hello  drafted by fakeai"), "{screen}");
    assert!(screen.contains("run: echo hello"), "{screen}");
    assert!(
        screen.contains("enter writes it and says yes · ^e opens it"),
        "{screen}"
    );
    assert_eq!(said(&app), "", "up in the pane: nothing to say");
    assert!(!home.flow_dir("hello").exists());

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    assert!(app.flows.iter().any(|f| f.name == "hello" && f.approved()));
    assert!(app.flow_proposal.is_none() && !app.show_pane, "the pane is gone");
    token.cancel();
}

#[tokio::test]
async fn more_words_refine_it() {
    let (_tmp, token, mut app, ws) = panel().await;
    let aim = Aim::New {
        named: None,
        folder: Some(ws.clone()),
    };
    ask_for(&mut app, aim, "say hello hourly");
    answer(&mut app, ANSWER).await;
    type_in(&mut app, "only weekdays").await;
    assert!(draw_text(&mut app).contains("changes the proposal"));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    unasked(&mut app);
    let p = app.flow_proposal.as_ref().unwrap();
    assert_eq!(p.asked, ["say hello hourly", "only weekdays"]);
    let (sent, _, want, words) = app.flow_asking().unwrap();
    assert!(sent.contains("Its flow.md now:"), "{sent}");
    assert!(sent.contains("  when: every 1h\n"), "the draft it changes: {sent}");
    assert!(sent.contains("Change it: only weekdays"), "{sent}");
    assert_eq!(want, Want::NewFlow { folder: Some(ws) });
    assert_eq!(words, "only weekdays");
    let screen = draw_text(&mut app);
    assert!(screen.contains("then     only weekdays"), "{screen}");
    assert!(screen.contains("changing it"), "{screen}");
    assert!(
        screen.contains("hello  drafted by fakeai"),
        "the last draft stays: {screen}"
    );
    // Still asked: enter is not its yes.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "still asking; a moment");
    token.cancel();
}

#[tokio::test]
async fn a_change_shows_a_diff_and_saves() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let base = hello_there(&mut app, &home).await;
    ask_for(&mut app, change(&base), "every 2h");
    assert_eq!(app.asking.as_ref().unwrap().want, Want::Flow { name: "hello".into() });
    let (sent, _, _, _) = app.flow_asking().unwrap();
    assert!(sent.contains("Its flow.md now:\n\n  ---\n  when: every 1h\n"), "{sent}");
    assert!(sent.contains("Change it: every 2h"), "{sent}");
    answer(
        &mut app,
        &ANSWER
            .replace("every 1h", "every 2h")
            .replace("name: hello", "name: other"),
    )
    .await;
    let screen = on_screen(&mut app);
    assert!(screen.contains(" - when: every 1h"), "{screen}");
    assert!(screen.contains(" + when: every 2h"), "{screen}");
    assert!(screen.contains("   steps:"), "what stays: {screen}");

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    let now = std::fs::read_to_string(home.flow_dir("hello").join("flow.md")).unwrap();
    assert!(now.contains("every 2h"), "{now}");
    assert!(app.flow_proposal.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_change_keeps_its_flows_name() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let base = hello_there(&mut app, &home).await;
    ask_for(&mut app, change(&base), "every 2h");
    answer(
        &mut app,
        &ANSWER
            .replace("every 1h", "every 2h")
            .replace("name: hello", "name: other"),
    )
    .await;
    let d = app.flow_proposal.as_ref().unwrap().draft.as_ref().unwrap();
    assert_eq!(d.name, "hello", "the overview's flow, not the answer's name");
    assert!(d.ready(), "a flow there is not a name taken: {:?}", d.problems);
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(!home.flow_dir("other").exists());
    assert!(!app.flows.iter().any(|f| f.name == "other"));
    token.cancel();
}

#[tokio::test]
async fn a_change_whose_flow_moved_is_not_saved() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let base = hello_there(&mut app, &home).await;
    ask_for(&mut app, change(&base), "every 2h");
    answer(&mut app, &ANSWER.replace("every 1h", "every 2h")).await;
    let md = home.flow_dir("hello").join("flow.md");
    std::fs::write(&md, HELLO.replace("Says hello.", "Says hi.")).unwrap();
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        said(&app),
        "hello's flow.md changed since it was asked · type to ask again"
    );
    assert!(
        std::fs::read_to_string(&md).unwrap().contains("Says hi."),
        "nothing written"
    );
    let screen = draw_text(&mut app);
    assert!(
        screen.contains("✗ hello's flow.md changed since it was asked"),
        "{screen}"
    );
    token.cancel();
}

#[tokio::test]
async fn a_proposal_that_would_not_load_is_not_saved() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    answer(&mut app, &ANSWER.replace("every 1h", "whenever")).await;
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(said(&app).starts_with("it would not load: "), "{}", said(&app));
    assert!(said(&app).ends_with(" · ^e opens it in $EDITOR"), "{}", said(&app));
    assert!(!home.flow_dir("hello").exists());
    assert!(app.flow_proposal_shown().is_some(), "still up");
    token.cancel();
}

#[tokio::test]
async fn esc_while_asking_leaves_it_asking() {
    let (_tmp, token, mut app, _) = panel().await;
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "still asking · /flow draft shows it");
    assert!(!app.show_pane);
    assert!(app.flow_proposal.is_some(), "still asked");
    run(&mut app, "/flow draft").await;
    let screen = draw_text(&mut app);
    assert!(screen.contains("asked    say hello"), "{screen}");
    token.cancel();
}

#[tokio::test]
async fn esc_once_answered_drops_it() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "dropped it · nothing written");
    assert!(app.flow_proposal.is_none() && !app.show_pane);
    assert!(!home.flow_dir("hello").exists());
    run(&mut app, "/flow draft").await;
    assert_eq!(
        said(&app),
        "no flow proposal · type on + new flow in a project, or /flow new <name> <words>"
    );
    token.cancel();
}

#[tokio::test]
async fn an_answer_after_esc_waits_unshown() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    answer(&mut app, ANSWER).await;
    assert_eq!(said(&app), "fakeai proposed hello · /flow draft shows it");
    assert!(!app.show_pane, "it does not pop up");
    let screen = on_screen(&mut app);
    assert!(!screen.contains("drafted by"), "{screen}");
    // An enter on the list is the list's.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(!home.flow_dir("hello").exists());
    assert!(
        app.flow_proposal.as_ref().is_some_and(|p| p.draft.is_some()),
        "it waits"
    );
    run(&mut app, "/flow draft").await;
    assert!(draw_text(&mut app).contains("hello  drafted by fakeai"));
    token.cancel();
}

#[tokio::test]
async fn a_run_where_a_flow_was_asked_is_said_in_the_pane() {
    let (_tmp, token, mut app, _) = panel().await;
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    answer(&mut app, "kind: run\nrun: echo hello\n").await;
    let screen = draw_text(&mut app);
    assert!(
        screen.contains("✗ asked for a flow; fakeai answered a run (run run00009)"),
        "{screen}"
    );
    assert!(screen.contains("type to ask again"), "{screen}");
    assert!(app.flow_proposal_shown().is_some(), "it stays up");
    // More words ask again.
    run(&mut app, "a flow, please").await;
    unasked(&mut app);
    let p = app.flow_proposal.as_ref().unwrap();
    assert_eq!(p.asked, ["say hello", "a flow, please"]);
    assert!(p.error.is_none() && p.since.is_some());
    token.cancel();
}

#[tokio::test]
async fn an_ask_that_fails_says_why_in_the_pane() {
    let (_tmp, token, mut app, _) = panel().await;
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    app.answered(Ok(Err(anyhow::anyhow!("fakeai took too long")))).await;
    let screen = draw_text(&mut app);
    assert!(screen.contains("✗ fakeai took too long"), "{screen}");
    assert!(app.flow_proposal.as_ref().unwrap().since.is_none());
    token.cancel();
}

#[tokio::test]
async fn flow_new_with_words_lands_in_the_pane() {
    let (_tmp, token, mut app, _) = panel().await;
    run(&mut app, "/flow new brief what my team did").await;
    unasked(&mut app);
    let p = app.flow_proposal.as_ref().expect("the pane");
    assert_eq!(
        p.aim,
        Aim::New {
            named: Some("brief".into()),
            folder: None
        }
    );
    let asking = app.asking.as_ref().expect("asked");
    assert_eq!(asking.want, Want::Flow { name: "brief".into() });
    assert_eq!(asking.words, "what my team did");
    // Whatever name the answer gives, the person's wins.
    answer(&mut app, ANSWER).await;
    let d = app.flow_proposal.as_ref().unwrap().draft.as_ref().unwrap();
    assert_eq!(d.name, "brief");
    token.cancel();
}

#[tokio::test]
async fn a_flow_answered_to_plain_words_moves_into_the_flow_pane() {
    let (_tmp, token, mut app, _) = panel().await;
    app.propose("say hello every hour".into());
    let folder = app.proposal.as_ref().unwrap().folder.clone();
    app.asking = Some(super::Asking {
        words: "say hello every hour".into(),
        want: Want::Any,
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    answer(&mut app, ANSWER).await;
    assert!(app.proposal.is_none(), "the run proposal is let go");
    let p = app.flow_proposal.as_ref().expect("the flow's");
    assert_eq!(
        p.aim,
        Aim::New {
            named: None,
            folder: Some(folder)
        }
    );
    assert_eq!(p.asked, ["say hello every hour"]);
    assert!(p.shown);
    let screen = draw_text(&mut app);
    assert!(screen.contains("asked    say hello every hour"), "{screen}");
    assert!(screen.contains("hello  drafted by fakeai"), "{screen}");
    token.cancel();
}

#[tokio::test]
async fn ctrl_e_opens_the_proposal_in_the_editor() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));

    // A new flow: written, opened, and said yes to on return, even saved as it was.
    proposed(&mut app, "later", HELLO).await;
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    assert_eq!(editing.file, home.flow_dir("later").join("flow.md"));
    assert!(app.flow_proposal.is_none() && !app.show_pane);
    assert!(app.editing_base.is_none());
    app.edited(editing).await;
    assert_eq!(said(&app), "later approved");

    // A change: its `flow.md` holds what was proposed, and what it was is kept to put back.
    let base = hello_there(&mut app, &home).await;
    ask_for(&mut app, change(&base), "every 2h");
    answer(&mut app, &ANSWER.replace("every 1h", "every 2h")).await;
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    assert_eq!(editing.name, "hello");
    let md = home.flow_dir("hello").join("flow.md");
    assert!(std::fs::read_to_string(&md).unwrap().contains("every 2h"));
    assert_eq!(app.editing_base.as_deref(), Some(base.as_str()));
    // The editor never opened: put back as it was asked of.
    crate::flow::restore(&editing, &base).unwrap();
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base);
    token.cancel();
}
