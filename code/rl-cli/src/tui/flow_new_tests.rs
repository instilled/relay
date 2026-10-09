//! The panel writing flows: `/flow new` from a template and from the bare one, and a drafted flow
//! shown, then written with its yes, opened first, or let go. A real `rld` over a fresh home; no
//! agent is asked — a draft is handed to the panel as the assistant's answer would hand it.

use super::tests::{draw_text, panel, press, type_in};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";

async fn run(app: &mut super::App, line: &str) {
    type_in(app, line).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

fn said(app: &super::App) -> String {
    app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default()
}

/// The screen drawn, and a draft on it there long enough for an enter to be its yes — as if the
/// person had looked a moment, without the test waiting.
fn on_screen(app: &mut super::App) -> String {
    let screen = draw_text(app);
    if let super::Mode::Draft(d) = &app.mode {
        d.drawn.set(Some(Instant::now() - Duration::from_secs(1)));
    }
    screen
}

#[tokio::test]
async fn flow_new_fills_a_template_and_its_save_is_the_yes() {
    let (tmp, token, mut app, folder) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    // On the project's run: `in` is its folder.
    app.select(Some("r:run00001".into()));
    run(&mut app, "/flow new logs --from prod-logs").await;
    assert_eq!(
        said(&app),
        "prod-logs needs project=… (the GCP project whose logs are read, like my-prod-project)"
    );
    assert!(
        app.editor.is_none() && !home.flow_dir("logs").exists(),
        "nothing written"
    );

    run(&mut app, "/flow new logs --from prod-logs project=my-prod").await;
    let (editing, line) = app.editor.take().expect("the editor is next");
    assert_eq!(line, 1);
    let md = std::fs::read_to_string(&editing.file).unwrap();
    assert!(
        md.contains(&format!("in: \"{}\"", rl_core::display_path(&folder))),
        "{md}"
    );
    // Back from the editor, left as written: it is the person's, it loads, and it has its yes.
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
    app.show_draft(Some("hello".into()), HELLO.into(), "fakeai".into());
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
    assert!(matches!(app.mode, super::Mode::Normal));

    // One that would not load is not written on enter; esc lets it go.
    app.show_draft(
        Some("broken".into()),
        "---\nsteps:\n  a:\n    after: nope\n    run: x\n---\n".into(),
        "fakeai".into(),
    );
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        said(&app),
        "it would not load: flow.md:3 · `a` comes after `nope`, which is no step · e opens it in $EDITOR"
    );
    assert!(!home.flow_dir("broken").exists());
    assert!(matches!(app.mode, super::Mode::Draft(_)), "still shown");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "draft let go: nothing written");
    assert!(matches!(app.mode, super::Mode::Normal));

    // `e` writes it and opens it, for the yes on return.
    app.show_draft(Some("later".into()), HELLO.into(), "fakeai".into());
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE).await;
    let (editing, _) = app.editor.take().expect("the editor is next");
    assert_eq!(editing.file, home.flow_dir("later").join("flow.md"));
    app.edited(editing).await;
    assert_eq!(said(&app), "later approved");

    // A name that is taken is a problem of the draft's, said before anything is written.
    app.show_draft(Some("hello".into()), HELLO.into(), "fakeai".into());
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
    app.show_draft(Some("tall".into()), md.clone(), "fakeai".into());
    let screen = on_screen(&mut app);
    assert!(!screen.contains("Line 80"), "{screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "the end is below · ↓ to read it");
    assert!(!home.flow_dir("tall").exists());
    assert!(matches!(app.mode, super::Mode::Draft(_)), "still shown");

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
    app.show_draft(Some("hello".into()), HELLO.into(), "fakeai".into());
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
    assert!(matches!(app.mode, super::Mode::Draft(_)), "still shown");
}

#[tokio::test]
async fn a_draft_with_no_yes_in_a_folder_of_yours_leaves_only_what_was_yours() {
    let (tmp, token, mut app, _) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    let dir = home.flow_dir("hello");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    app.show_draft(Some("hello".into()), HELLO.into(), "fakeai".into());
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
        assert!(matches!(app.mode, super::Mode::Draft(_)), "still shown");
    }
    token.cancel();
}

/// The assistant answering what `want` asked with `HELLO`, named `name` by the draft.
async fn answer_flow(app: &mut super::App, folder: &std::path::Path, want: Want, name: &str) {
    use crate::interpret::{Answer, FlowDraft};
    app.asking = Some(super::Asking {
        words: "say hello".into(),
        folder: folder.to_path_buf(),
        want,
        task: tokio::spawn(async { anyhow::bail!("not asked") }),
    });
    let out = rl_proto::methods::AssistantOutput {
        run: "run00009".into(),
        agent: "fakeai".into(),
        output: String::new(),
        exit: Some(0),
        timed_out: false,
    };
    let draft = FlowDraft {
        name: Some(name.into()),
        text: HELLO.into(),
    };
    app.answered(Ok(Ok((Answer::Flow(draft), out)))).await;
}

#[tokio::test]
async fn a_flow_answered_is_shown_as_a_draft_named_by_whoever_asked() {
    let (_tmp, token, mut app, folder) = panel().await;
    let asked = [(Want::Flow { name: "mine".into() }, "mine"), (Want::Any, "theirs")];
    for (want, shown) in asked {
        answer_flow(&mut app, &folder, want, "theirs").await;
        let super::Mode::Draft(d) = &app.mode else {
            panic!("the draft is shown: {}", said(&app));
        };
        assert_eq!(d.name, shown);
        assert_eq!(said(&app), format!("fakeai drafted {shown}"));
        app.mode = super::Mode::Normal;
    }
    token.cancel();
}

#[tokio::test]
async fn a_draft_that_comes_while_the_keys_are_anothers_waits_for_flow_draft() {
    let (_tmp, token, mut app, folder) = panel().await;
    run(&mut app, "/new").await;
    type_in(&mut app, "make").await;
    assert!(matches!(app.mode, super::Mode::Form(_)));
    // It comes mid-form: the form stays, its keys its own, and the draft waits.
    answer_flow(&mut app, &folder, Want::Any, "first").await;
    let super::Mode::Form(f) = &app.mode else {
        panic!("the form is left as it was");
    };
    assert_eq!(f.get("run"), "make");
    assert_eq!(said(&app), "first drafted · /flow draft shows it");
    // Another replaces it; so does one that comes while words are typed on the line.
    answer_flow(&mut app, &folder, Want::Any, "second").await;
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    type_in(&mut app, "half typed").await;
    answer_flow(&mut app, &folder, Want::Any, "third").await;
    assert!(matches!(app.mode, super::Mode::Normal));
    assert_eq!(said(&app), "third drafted · /flow draft shows it");
    while !app.line.is_empty() {
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE).await;
    }

    run(&mut app, "/flow draft").await;
    let super::Mode::Draft(d) = &app.mode else {
        panic!("shown now: {}", said(&app));
    };
    assert_eq!(d.name, "third");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    run(&mut app, "/flow draft").await;
    assert_eq!(said(&app), "no draft waiting · /flow new <name> <words> asks for one");
    token.cancel();
}

#[tokio::test]
async fn an_enter_as_a_draft_comes_on_screen_is_not_its_yes() {
    let (tmp, token, mut app, folder) = panel().await;
    let home = rl_core::Home::at(tmp.path().join("h"));
    answer_flow(&mut app, &folder, Want::Any, "hello").await;
    draw_text(&mut app);
    // Pressed as it was drawn: ignored, and nothing said.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(matches!(app.mode, super::Mode::Draft(_)), "still shown");
    assert_eq!(said(&app), "");
    assert!(!home.flow_dir("hello").exists());
    // A moment later, it is the yes.
    on_screen(&mut app);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "hello approved");
    token.cancel();
}
