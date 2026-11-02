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

/// The screen drawn, and the draft on it there long enough for an enter to be its yes, and an esc
/// its no — as if the person had looked a moment, without the test waiting.
pub(super) fn on_screen(app: &mut super::App) -> String {
    let screen = draw_text(app);
    if let Some(p) = app.flow_proposal.as_mut() {
        if let Some(d) = p.draft.as_ref() {
            d.drawn.set(Some(Instant::now() - Duration::from_secs(1)));
        }
        if p.landed.is_some() {
            p.landed = Some(Instant::now() - Duration::from_secs(1));
        }
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
    // What it is asked of is the file as it is now, and the draft against the old one is gone.
    let p = app.flow_proposal.as_ref().unwrap();
    assert!(p.draft.is_none());
    let now = std::fs::read_to_string(&md).unwrap();
    assert_eq!(p.aim, change(&now));
    // More words ask again, from it.
    run(&mut app, "every 2h").await;
    unasked(&mut app);
    let (sent, _, _, _) = app.flow_asking().unwrap();
    assert!(sent.contains("  Says hi.\n"), "{sent}");
    assert!(!sent.contains("every 2h\n  steps"), "not the old draft: {sent}");
    token.cancel();
}

#[tokio::test]
async fn another_subpage_over_the_proposal_takes_its_keys() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    on_screen(&mut app);
    assert!(app.flow_proposal_shown().is_some());
    // A run's output, over it: the keys are the output's.
    app.open_term_on("run00001".into(), super::tests::AREA).await;
    assert!(app.flow_proposal_shown().is_none());
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(!home.flow_dir("hello").exists(), "enter wrote nothing");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    assert!(app.editor.is_none(), "^e opened nothing");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(
        app.flow_proposal.as_ref().is_some_and(|p| p.draft.is_some()),
        "esc left the output, not the proposal"
    );
    run(&mut app, "/flow draft").await;
    assert!(draw_text(&mut app).contains("hello  drafted by fakeai"));
    token.cancel();
}

#[tokio::test]
async fn an_answer_with_no_flow_proposal_to_land_in_is_let_be() {
    let (_tmp, token, mut app, _) = panel().await;
    app.asking = Some(super::Asking {
        words: "every 2h".into(),
        want: Want::Flow { name: "hello".into() },
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    answer(&mut app, ANSWER).await;
    assert!(app.flow_proposal.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_flow_answered_to_plain_words_lets_an_earlier_one_go_and_says_so() {
    let (_tmp, token, mut app, _) = panel().await;
    // One answered while it was left: it waits, unshown.
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
    // Plain words, answered with a flow: it takes the earlier one's place.
    app.propose("say bye every hour".into());
    app.asking = Some(super::Asking {
        words: "say bye every hour".into(),
        want: Want::Any,
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    answer(&mut app, &ANSWER.replace("name: hello", "name: bye")).await;
    assert_eq!(said(&app), "the earlier flow proposal was let go");
    let d = app.flow_proposal_shown().and_then(|p| p.draft.as_ref()).unwrap();
    assert_eq!(d.name, "bye");
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
async fn an_esc_just_as_the_answer_lands_is_let_be() {
    let (_tmp, token, mut app, _) = panel().await;
    proposed(&mut app, "hello", HELLO).await;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert!(app.flow_proposal.is_some(), "the answer kept");
    assert!(app.show_pane, "still up");
    assert!(!said(&app).starts_with("dropped it"), "{}", said(&app));
    // A moment on, esc is meant for what is there.
    if let Some(p) = app.flow_proposal.as_mut() {
        p.landed = Some(Instant::now() - Duration::from_secs(1));
    }
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "dropped it · nothing written");
    assert!(app.flow_proposal.is_none());
    token.cancel();
}

#[tokio::test]
async fn esc_once_answered_drops_it() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    on_screen(&mut app);
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
    assert!(app.editing_change.is_none());
    app.edited(editing).await;
    assert_eq!(said(&app), "later approved");

    // A change: its `flow.md` holds what was proposed, and the proposal is kept aside, with what
    // it was asked of, to put back.
    let (editing, base) = change_in_editor(&mut app, &home).await;
    assert_eq!(editing.name, "hello");
    let md = home.flow_dir("hello").join("flow.md");
    assert!(std::fs::read_to_string(&md).unwrap().contains("every 2h"));
    let kept = app.editing_change.as_ref().expect("kept aside");
    assert_eq!(kept.aim, change(&base));
    assert!(app.flow_proposal.is_none() && !app.show_pane);
    token.cancel();
}

/// `hello` there, a change to it proposed — `every 2h` — and `^e` on it: the editor next, the
/// file it opens, and `hello`'s `flow.md` as it was asked of.
async fn change_in_editor(app: &mut super::App, home: &rl_core::Home) -> (crate::flow::Editing, String) {
    let base = hello_there(app, home).await;
    ask_for(app, change(&base), "every 2h");
    answer_md(app, None, &HELLO.replace("every 1h", "every 2h")).await;
    on_screen(app);
    press(app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    (editing, base)
}

/// A client whose daemon hangs up on it: every call fails. Its home, kept while it is used.
async fn hung_up() -> (tempfile::TempDir, crate::client::Client) {
    let tmp = tempfile::tempdir().unwrap();
    let home = rl_core::Home::at(tmp.path());
    let sock = home.daemon_socket();
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            drop(s);
        }
    });
    let rpc = crate::client::Client::connect(&home).await.unwrap();
    (tmp, rpc)
}

/// `hello`, as the daemon has it now.
fn hello(app: &super::App) -> &rl_proto::flow::Flow {
    app.flows.iter().find(|f| f.name == "hello").expect("hello is there")
}

#[tokio::test]
async fn a_change_saved_changed_in_the_editor_is_said_yes_to() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let (editing, _) = change_in_editor(&mut app, &home).await;
    let mine = HELLO.replace("every 1h", "every 3h");
    std::fs::write(&editing.file, &mine).unwrap();
    app.edited(editing).await;
    assert_eq!(said(&app), "hello approved");
    assert_eq!(
        std::fs::read_to_string(home.flow_dir("hello").join("flow.md")).unwrap(),
        mine
    );
    assert!(hello(&app).approved());
    assert!(app.flow_proposal.is_none() && app.editing_change.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_change_left_as_proposed_in_the_editor_is_no_yes() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let (editing, base) = change_in_editor(&mut app, &home).await;
    let md = editing.file.clone();
    // The editor quit without saving: the file is as proposed.
    app.edited(editing).await;
    assert_eq!(said(&app), "nothing saved · back in the proposal: enter saves it");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base, "put back");
    assert!(hello(&app).approved(), "the flow as it was, with its yes");
    let p = app.flow_proposal_shown().expect("back in the pane");
    assert_eq!(p.aim, change(&base));
    let d = p.draft.as_ref().unwrap();
    assert!(d.text.contains("every 2h") && d.ready(), "the proposal as it was");
    // And `enter` is its yes.
    let screen = on_screen(&mut app);
    assert!(screen.contains(" + when: every 2h"), "{screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    assert!(std::fs::read_to_string(&md).unwrap().contains("every 2h"));
    token.cancel();
}

#[tokio::test]
async fn a_change_that_would_not_load_in_the_editor_puts_the_flow_back() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let (editing, base) = change_in_editor(&mut app, &home).await;
    let md = editing.file.clone();
    let broken = HELLO.replace("every 1h", "every now and then");
    std::fs::write(&md, &broken).unwrap();
    app.edited(editing).await;
    assert_eq!(said(&app), "hello kept as it was · what you wrote is in the proposal");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base, "put back");
    assert!(hello(&app).approved(), "loaded as it was, with its yes");
    assert!(!app.not_loaded.iter().any(|n| n.flow == "hello"), "read back as it was");
    let p = app.flow_proposal_shown().expect("back in the pane");
    assert_eq!(p.aim, change(&base));
    assert!(
        p.error.as_deref().is_some_and(|e| e.starts_with("not loaded: ")),
        "{:?}",
        p.error
    );
    let d = p.draft.as_ref().unwrap();
    assert_eq!(d.text, broken, "what was written is the draft");
    assert!(!d.ready(), "and it says why it would not load");
    let screen = on_screen(&mut app);
    assert!(screen.contains("✗ not loaded: "), "{screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(said(&app).starts_with("it would not load: "), "{}", said(&app));
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base);
    token.cancel();
}

#[tokio::test]
async fn a_change_the_daemon_cannot_take_puts_the_flow_back() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let (editing, base) = change_in_editor(&mut app, &home).await;
    let md = editing.file.clone();
    let mine = HELLO.replace("every 1h", "every 3h");
    std::fs::write(&md, &mine).unwrap();
    // No daemon to say yes.
    let (_gone, rpc) = hung_up().await;
    app.rpc = rpc;
    app.edited(editing).await;
    assert_eq!(said(&app), "hello kept as it was · what you wrote is in the proposal");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base, "put back");
    let p = app.flow_proposal_shown().expect("back in the pane");
    assert!(
        p.error.as_deref().is_some_and(|e| e.starts_with("edit hello: ")),
        "{:?}",
        p.error
    );
    assert_eq!(p.draft.as_ref().unwrap().text, mine);
    token.cancel();
}

#[tokio::test]
async fn a_change_needing_a_yes_after_the_editor_keeps_its_text() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let base = hello_there(&mut app, &home).await;
    // Something in its folder since its yes: no yes comes with an edit.
    std::fs::write(home.flow_dir("hello").join("notes.txt"), "mine\n").unwrap();
    ask_for(&mut app, change(&base), "every 2h");
    answer_md(&mut app, None, &HELLO.replace("every 1h", "every 2h")).await;
    on_screen(&mut app);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    let mine = HELLO.replace("every 1h", "every 3h");
    std::fs::write(&editing.file, &mine).unwrap();
    app.edited(editing).await;
    assert_eq!(
        said(&app),
        "hello needs a yes: its folder changed since its last yes; look, then rl flow approve hello"
    );
    assert_eq!(
        std::fs::read_to_string(home.flow_dir("hello").join("flow.md")).unwrap(),
        mine
    );
    assert!(app.flow_proposal.is_none());
    token.cancel();
}

#[tokio::test]
async fn a_refine_that_comes_to_nothing_leaves_the_draft_older() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    proposed(&mut app, "hello", HELLO).await;
    run(&mut app, "only weekdays").await;
    unasked(&mut app);
    app.answered(Ok(Err(anyhow::anyhow!("fakeai took too long")))).await;
    let d = app.flow_proposal.as_ref().unwrap().draft.as_ref().unwrap();
    assert!(d.older, "the words did not reach it");
    let screen = on_screen(&mut app);
    assert!(screen.contains("✗ fakeai took too long"), "{screen}");
    assert!(
        screen.contains("the last words were not taken · type again, or esc"),
        "the footer: {screen}"
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "the last words were not taken · type again, or esc");
    assert!(!home.flow_dir("hello").exists(), "no yes to the older one");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
    assert!(app.editor.is_none(), "nor opened for one");
    // An answer that takes the words: a draft again, and its yes.
    run(&mut app, "only weekdays").await;
    unasked(&mut app);
    answer_md(&mut app, Some("hello"), HELLO).await;
    assert!(!app.flow_proposal.as_ref().unwrap().draft.as_ref().unwrap().older);
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    token.cancel();
}

#[tokio::test]
async fn new_words_for_a_flow_say_an_earlier_draft_goes() {
    let (_tmp, token, mut app, _) = panel().await;
    proposed(&mut app, "hello", HELLO).await;
    // A run's output over it, then words for another flow.
    app.open_term_on("run00001".into(), super::tests::AREA).await;
    run(&mut app, "/flow new bye say bye").await;
    unasked(&mut app);
    assert_eq!(said(&app), "the earlier flow proposal was let go");
    assert_eq!(app.flow_proposal_shown().unwrap().asked, ["say bye"]);
    token.cancel();
}

#[tokio::test]
async fn while_asked_the_only_key_is_esc() {
    let (_tmp, token, mut app, _) = panel().await;
    ask_for(
        &mut app,
        Aim::New {
            named: None,
            folder: None,
        },
        "say hello",
    );
    assert_eq!(
        app.shortcuts_place(),
        Some(super::shortcuts::Place::FlowProposal { asking: true })
    );
    answer(&mut app, ANSWER).await;
    assert_eq!(
        app.shortcuts_place(),
        Some(super::shortcuts::Place::FlowProposal { asking: false })
    );
    let screen = on_screen(&mut app);
    assert!(screen.contains("type changes it · enter writes it"), "{screen}");
    token.cancel();
}
