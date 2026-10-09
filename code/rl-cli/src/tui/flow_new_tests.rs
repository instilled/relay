//! The panel writing flows: `/flow new` from a template and from the bare one, and a drafted flow
//! shown, then written with its yes, opened first, or let go. A real `rld` over a fresh home; no
//! agent is asked — a draft is handed to the panel as the assistant's answer would hand it.

use super::tests::{draw_text, panel, press, type_in};
use crate::context::Want;
use crossterm::event::{KeyCode, KeyModifiers};

const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";

async fn run(app: &mut super::App, line: &str) {
    type_in(app, line).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

fn said(app: &super::App) -> String {
    app.flash.as_ref().map(|(m, _)| m.clone()).unwrap_or_default()
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
    let screen = draw_text(&mut app);
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
    let screen = draw_text(&mut app);
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
    draw_text(&mut app);
    // No daemon to load it and say yes: what was written goes again.
    token.cancel();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
    draw_text(&mut app);
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

#[tokio::test]
async fn a_flow_answered_is_shown_as_a_draft_named_by_whoever_asked() {
    use crate::interpret::{Answer, FlowDraft};
    let (_tmp, token, mut app, folder) = panel().await;
    let asked = [(Want::Flow { name: "mine".into() }, "mine"), (Want::Any, "theirs")];
    for (want, shown) in asked {
        app.asking = Some(super::Asking {
            words: "say hello".into(),
            folder: folder.clone(),
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
            name: Some("theirs".into()),
            text: HELLO.into(),
        };
        app.answered(Ok(Ok((Answer::Flow(draft), out)))).await;
        let super::Mode::Draft(d) = &app.mode else {
            panic!("the draft is shown: {}", said(&app));
        };
        assert_eq!(d.name, shown);
        assert_eq!(said(&app), format!("fakeai drafted {shown}"));
        app.mode = super::Mode::Normal;
    }
    token.cancel();
}
