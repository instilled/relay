//! The panel with Claude's own sessions on its list: their rows drawn, their
//! pane opened, one hidden. A real `rld` over a fresh home, which never asks
//! `claude` (`RELAY_CLAUDE_SESSIONS=off`): the sessions are put into the
//! digest the panel holds, as `rld` would carry them.

use super::outside::session;
use super::tests::{draw_text, panel, panel_with_profiles, press, screen, type_in};
use crossterm::event::{KeyCode, KeyModifiers};
use rl_proto::methods::Outside;

/// One of each: background and blocked on a prompt, in a terminal and at
/// work, both in the panel's project; and one in a folder that is no project.
fn sessions(folder: &std::path::Path) -> Vec<Outside> {
    let started = Some(jiff::Timestamp::now() - jiff::SignedDuration::from_mins(5));
    vec![
        Outside {
            cwd: folder.to_path_buf(),
            name: "tidy the docs".into(),
            started_at: started,
            ..session("bg", Some("blocked"), "waiting", Some("permission prompt"))
        },
        Outside {
            cwd: folder.to_path_buf(),
            name: "fix the parser".into(),
            started_at: started,
            ..session("term", None, "busy", None)
        },
        Outside {
            cwd: "/nowhere/scratch".into(),
            name: String::new(),
            state: Some("pondering".into()),
            status: "zoning".into(),
            started_at: None,
            ..session("odd", Some("x"), "", None)
        },
    ]
}

#[tokio::test]
async fn claudes_own_sessions_are_rows_with_a_pane() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = sessions(&folder);
    let screen = draw_text(&mut app);
    let row = |name: &str| {
        screen
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("no row for {name}:\n{screen}"))
            .to_string()
    };
    let bg = row("tidy the docs");
    assert!(bg.contains("needs you"), "{bg}");
    assert!(bg.contains("permission prompt · Claude's · background"), "{bg}");
    let term = row("fix the parser");
    assert!(term.contains("working"), "{term}");
    assert!(term.contains("Claude's · in another terminal"), "{term}");
    // A state this does not know is said as Claude wrote it; one with no
    // name goes by its session id.
    let odd = row("pondering");
    assert!(
        odd.contains("session sess-odd") && odd.contains("Claude's · background"),
        "{odd}"
    );
    // The headline counts them: one needs you.
    assert!(screen.contains("1 needs you"), "{screen}");

    // Each opens its pane, saying what enter does with it.
    for (key, enter) in [
        ("o:claude:sess-bg", "enter: open it here (claude attach)"),
        ("o:claude:sess-term", "busy in its terminal"),
        ("o:claude:sess-odd", "enter: open it here (claude attach)"),
    ] {
        app.select(Some(key.into()));
        // With the pane shut, enter opens it, and does nothing else.
        app.show_pane = false;
        assert!(app.hints().contains("enter its pane · /hide"), "{}", app.hints());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.show_pane && app.attach_outside.is_none());
        let screen = draw_text(&mut app);
        // The pane is narrow: the words may wrap after the verb.
        let words = enter.split(" (").next().unwrap_or(enter);
        assert!(screen.contains(words), "{key}: {screen}");
        assert!(screen.contains(&key["o:claude:".len()..]), "{key}: {screen}");
    }
    token.cancel();
}

#[tokio::test]
async fn a_narrow_list_still_ends_each_claude_row_in_its_age() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = sessions(&folder);
    app.show_pane = false;
    for w in [48, 64, 80] {
        let rows = screen(&mut app, w, 20);
        let row = rows
            .iter()
            .find(|l| l.contains("tidy the"))
            .unwrap_or_else(|| panic!("no row at {w}:\n{}", rows.join("\n")));
        assert!(row.trim_end().ends_with("5m"), "{w}: {row:?}");
    }
    token.cancel();
}

#[tokio::test]
async fn a_wide_glyph_wait_still_leaves_a_narrow_row_its_age() {
    let (_tmp, token, mut app, folder) = panel().await;
    let mut wide = sessions(&folder);
    wide[0].waiting_for = Some("権限の確認が必要です".into());
    app.digest.outside = wide;
    app.show_pane = false;
    for w in [48, 56, 64] {
        let rows = screen(&mut app, w, 20);
        let row = rows
            .iter()
            .find(|l| l.contains("tidy the"))
            .unwrap_or_else(|| panic!("no row at {w}:\n{}", rows.join("\n")));
        assert!(row.trim_end().ends_with("5m"), "{w}: {row:?}");
    }
    token.cancel();
}

#[tokio::test]
async fn hide_takes_a_claude_session_off_the_list() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = sessions(&folder);
    app.select(Some("o:claude:sess-bg".into()));
    type_in(&mut app, "/hide").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    // The daemon keeps it by its key: the next digest has it hidden.
    assert!(
        app.digest.hidden.iter().any(|h| h.key == "o:claude:sess-bg"),
        "{:?} {:?}",
        app.digest.hidden,
        app.flash
    );
    app.digest.outside = sessions(&folder);
    let on_the_list = |app: &super::App| {
        app.rows()
            .iter()
            .any(|r| r.key().as_deref() == Some("o:claude:sess-bg"))
    };
    assert!(!on_the_list(&app), "hidden");
    app.show_hidden = true;
    assert!(on_the_list(&app), "back with hidden on");

    // It came to need you before it was hidden: it stays hidden. It comes
    // to need you after: it comes through, just after the questions.
    app.show_hidden = false;
    let hid = app
        .digest
        .hidden
        .iter()
        .find(|h| h.key == "o:claude:sess-bg")
        .unwrap()
        .at;
    let needing = |at: jiff::Timestamp| {
        let mut s = sessions(&folder);
        s[0].state_at = Some(at);
        s
    };
    app.digest.outside = needing(hid - jiff::SignedDuration::from_secs(1));
    assert!(!on_the_list(&app), "needed you before it was hidden");
    app.digest.outside = needing(hid + jiff::SignedDuration::from_secs(1));
    assert!(on_the_list(&app), "comes through");
    let rows = app.rows();
    assert_eq!(
        rows[0].key().as_deref(),
        Some("o:claude:sess-bg"),
        "above every project"
    );
    token.cancel();
}

#[tokio::test]
async fn another_agents_session_wears_its_agents_glyph_and_name() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = vec![Outside {
        agent: "codex".into(),
        cwd: folder.clone(),
        name: "port the parser".into(),
        started_at: Some(jiff::Timestamp::now() - jiff::SignedDuration::from_mins(5)),
        ..session("cx", None, "busy", None)
    }];
    let screen = draw_text(&mut app);
    let row = screen
        .lines()
        .find(|l| l.contains("port the parser"))
        .unwrap_or_else(|| panic!("no row:\n{screen}"));
    assert!(row.contains('⬡'), "codex's glyph: {row}");
    assert!(!row.contains('✻'), "not claude's: {row}");
    assert!(row.contains("Codex's · in another terminal"), "{row}");
    assert!(
        app.rows().iter().any(|r| r.key().as_deref() == Some("o:codex:sess-cx")),
        "keyed by its agent"
    );
    // A search finds it by whose it is, and only it.
    app.filter = "kind:codex".into();
    let keys: Vec<_> = app.rows().iter().filter_map(|r| r.key()).collect();
    assert!(keys.contains(&"o:codex:sess-cx".to_string()), "{keys:?}");
    app.filter = "kind:claude".into();
    let keys: Vec<_> = app.rows().iter().filter_map(|r| r.key()).collect();
    assert!(!keys.contains(&"o:codex:sess-cx".to_string()), "{keys:?}");
    token.cancel();
}

/// The flash line's words.
fn said(app: &super::App) -> String {
    app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default()
}

/// A `claude` at a place of the test's choosing, never one on `PATH`.
fn fake_claude(_: &str) -> Option<std::path::PathBuf> {
    Some("/opt/fake/claude".into())
}

/// With its pane open, enter on a background session hands the terminal to
/// `claude attach <id>` — only built here, run by the main loop — and
/// nothing reaches the ledger; `/join` does the same at once, pane or not.
/// Where there is no `claude`, it says so.
#[tokio::test]
async fn enter_or_join_on_a_background_session_attaches_to_it() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.agent_bin = fake_claude;
    app.digest.outside = sessions(&folder);
    app.select(Some("o:claude:sess-bg".into()));
    app.show_pane = true;
    assert!(app.hints().contains("enter opens it here · /hide"), "{}", app.hints());
    let runs = app.digest.items.len();
    let attach: Vec<std::ffi::OsString> = vec!["/opt/fake/claude".into(), "attach".into(), "bg".into()];
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.attach_outside.take(), Some(attach.clone()));
    assert_eq!(app.digest.items.len(), runs, "nothing made of it in Relay");

    app.show_pane = false;
    type_in(&mut app, "/join").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.attach_outside.take(), Some(attach), "{}", said(&app));

    app.agent_bin = |_| None;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.attach_outside.is_none());
    assert_eq!(said(&app), "claude attach: `claude` is not on this machine");
    token.cancel();
}

/// Enter on one carried on here already, at work again in the terminal it
/// was left open in, goes to its run.
#[tokio::test]
async fn enter_on_one_carried_on_already_goes_to_its_run() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = vec![Outside {
        cwd: folder.clone(),
        run: Some("run00001".into()),
        ..session("left", None, "busy", None)
    }];
    app.select(Some("o:claude:sess-left".into()));
    app.show_pane = true;
    assert!(app.hints().contains("enter goes to its run"), "{}", app.hints());
    let screen = draw_text(&mut app);
    assert!(screen.contains("still open in its"), "{screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.sel.as_deref(), Some("r:run00001"));
    assert!(app.carrying.is_none() && app.attach_outside.is_none());
    token.cancel();
}

/// With its pane open, enter on one in another terminal asks `rld` to carry
/// it on, in the background; what `rld` says back is said. This `rld` was
/// never told of it.
#[tokio::test]
async fn enter_on_one_in_another_terminal_asks_to_carry_it_on() {
    let (_tmp, token, mut app, folder) = panel().await;
    app.digest.outside = vec![Outside {
        agent: "codex".into(),
        cwd: folder.clone(),
        ..session("cx", None, "idle", None)
    }];
    app.select(Some("o:codex:sess-cx".into()));
    app.show_pane = true;
    assert!(app.hints().contains("enter carries it on here"), "{}", app.hints());
    let screen = draw_text(&mut app);
    assert!(screen.contains("enter: carry it on here"), "{screen}");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.attach_outside.is_none());
    // Said at once, in the footer while it lasts, and the panel goes on
    // meanwhile.
    assert!(
        app.footer(200).starts_with("carrying name cx on… · "),
        "{}",
        app.footer(200)
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(said(&app), "carrying name cx on already");
    let done = (&mut app.carrying.as_mut().expect("on its way").task).await;
    app.carried(done).await;
    assert!(app.carrying.is_none());
    assert!(
        said(&app).contains("codex has no session sess-cx now"),
        "{}",
        said(&app)
    );
    token.cancel();
}

/// One `rld` was told of, idle a while in its terminal, carried on: the run
/// that carries it becomes the selection, and the panel says to leave the
/// other terminal be. The codex here is a script that waits.
#[tokio::test]
async fn an_idle_one_in_another_terminal_carries_on_as_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexec sleep 30\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let profile = format!("bins: [\"{}\"]\n", codex.display());
    let Some((_home, token, mut app, folder)) = panel_with_profiles(vec![("codex", profile)]).await else {
        return;
    };
    for sid in ["sess-cx", "sess-cy"] {
        let hook = rl_proto::methods::OutsideHookParams {
            agent: "codex".into(),
            session_id: sid.into(),
            cwd: folder.clone(),
            event: "Stop".into(),
            detail: None,
            pid: None,
            transcript_path: None,
            run: None,
        };
        app.rpc.call::<rl_proto::methods::OutsideHook>(hook).await.unwrap();
    }
    // Quiet a moment, as `rld` wants before it carries one on.
    tokio::time::sleep(std::time::Duration::from_millis(3200)).await;
    let told = |id: &str| Outside {
        agent: "codex".into(),
        cwd: folder.clone(),
        ..session(id, None, "idle", None)
    };
    app.digest.outside = vec![told("cx"), told("cy")];
    app.select(Some("o:codex:sess-cx".into()));
    app.show_pane = true;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    // Said for as long as it takes, not as long as a flash lasts.
    app.flash = None;
    assert!(
        app.footer(200).starts_with("carrying name cx on… · "),
        "{}",
        app.footer(200)
    );
    let done = (&mut app.carrying.as_mut().expect("on its way").task).await;
    app.carried(done).await;
    assert_eq!(
        said(&app),
        "name cx carries on here; it is still open in its own terminal — leave that one"
    );
    let sel = app.sel.clone().unwrap_or_default();
    let id = sel
        .strip_prefix("r:")
        .unwrap_or_else(|| panic!("a run selected: {sel}"));
    let run = app
        .rpc
        .call::<super::RunGet>(rl_proto::methods::RunId { id: id.into() })
        .await
        .unwrap();
    assert!(run.tags.iter().any(|t| t == "codex-session:sess-cx"), "{:?}", run.tags);
    assert_eq!(run.argv, [codex.to_string_lossy().as_ref(), "resume", "sess-cx"]);

    // The cursor moved on while the next was being carried on: it stays
    // where it went, and the run is named.
    app.digest.outside = vec![told("cy")];
    app.select(Some("o:codex:sess-cy".into()));
    app.show_pane = true;
    app.pane = super::Pane::Facts;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let moved_to = format!("r:{}", run.id);
    app.select(Some(moved_to.clone()));
    let done = (&mut app.carrying.as_mut().expect("on its way").task).await;
    let other = done
        .as_ref()
        .ok()
        .and_then(|r| r.as_ref().ok())
        .cloned()
        .expect("carried on");
    app.carried(done).await;
    assert_eq!(app.sel.as_deref(), Some(moved_to.as_str()), "the cursor stays");
    assert_eq!(said(&app), format!("name cy carries on here · run {}", other.title));
    assert!(app.carrying.is_none() && !app.footer(200).contains("carrying"));
    let _ = app
        .rpc
        .call::<super::RunStop>(rl_proto::methods::RunStopParams {
            id: other.id.clone(),
            force: false,
            archive: false,
        })
        .await;
    let _ = app
        .rpc
        .call::<super::RunStop>(rl_proto::methods::RunStopParams {
            id: run.id.clone(),
            force: false,
            archive: false,
        })
        .await;
    token.cancel();
}
