//! The list's actions on sessions a real `rl-session` holder runs: stopping
//! one with `^x ^x`, and starting one from the form.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rl::spawn::{self, Start};
use rl::store::sessions::{self, Status};
use rl::tui::App;
use rl_core::Home;
use std::path::{Path, PathBuf};
use std::sync::Once;

fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rl-session");
    p.is_file().then_some(p)
}

/// Points the list's starts at the holder this build made. Every test calls
/// it first, so nothing reads the environment while it is set.
fn holder() -> bool {
    static SET: Once = Once::new();
    let Some(bin) = session_bin() else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return false;
    };
    // SAFETY: set once, before any test of this binary goes on to read the
    // environment (each waits on the `Once` first).
    SET.call_once(|| unsafe { std::env::set_var(spawn::SESSION_BIN_ENV, bin) });
    true
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: Home,
    folder: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = Home::at(root.join("relay"));
    home.ensure_store().unwrap();
    std::fs::write(home.config_json(), r#"{"badges": []}"#).unwrap();
    let folder = root.join("shop");
    std::fs::create_dir_all(&folder).unwrap();
    Fixture {
        _tmp: tmp,
        home,
        folder,
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::from(code));
}

fn ctrl(app: &mut App, c: char) {
    app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn status(home: &Home, name: &str) -> Option<Status> {
    sessions::list(home)
        .into_iter()
        .find(|h| h.meta.name == name)
        .map(|h| h.status)
}

async fn sh(home: &Home, cwd: &Path) -> String {
    spawn::start(
        home,
        Start {
            harness: None,
            cwd: cwd.to_path_buf(),
            argv: vec!["sh".into()],
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap()
    .name
}

#[tokio::test]
async fn ctrl_x_twice_stops_a_live_held_shell() {
    if !holder() {
        return;
    }
    let f = fixture();
    let name = sh(&f.home, &f.folder).await;
    assert_eq!(status(&f.home, &name), Some(Status::Live));
    let mut app = App::new(f.home.clone()).unwrap();
    app.select(Some(format!("r:{name}")));
    ctrl(&mut app, 'x');
    assert!(
        app.flashing().is_some_and(|m| m.contains(&format!("stop {name}"))),
        "{:?}",
        app.flashing()
    );
    assert_eq!(status(&f.home, &name), Some(Status::Live), "not on the first press");
    ctrl(&mut app, 'x');
    app.finish().await;
    // The holder writes its exit once the shell has gone, which under load
    // takes a while.
    let ended = || matches!(status(&f.home, &name), Some(Status::Ended(_)));
    let by = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ended() && std::time::Instant::now() < by {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ended(), "{:?}", status(&f.home, &name));
    app.reload();
    assert!(
        app.flashing().is_some_and(|m| m.starts_with("stopped")),
        "{:?}",
        app.flashing()
    );
    // Its row stays, ended, until ^x twice again removes it.
    assert_eq!(app.sel.as_deref(), Some(format!("r:{name}").as_str()));
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    app.finish().await;
    assert_eq!(status(&f.home, &name), None);
}

#[tokio::test]
async fn the_form_starts_a_shell_in_the_folder_chosen_and_selects_it() {
    if !holder() {
        return;
    }
    let f = fixture();
    let other = f.folder.parent().unwrap().join("api");
    std::fs::create_dir_all(&other).unwrap();
    let mut app = App::new(f.home.clone()).unwrap();
    press(&mut app, KeyCode::Char('n'));
    // A shell is last of the harnesses: one back from the first, round.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Tab);
    ctrl(&mut app, 'u');
    for c in other.display().to_string().chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    let held = sessions::list(&f.home);
    assert_eq!(held.len(), 1, "{:?}", app.flashing());
    let meta = &held[0].meta;
    assert_eq!(meta.cwd, other);
    assert_eq!(meta.kind, rl_core::state::RunKind::Shell);
    assert_eq!(app.sel.as_deref(), Some(format!("r:{}", meta.name).as_str()));
    assert!(app.flashing().is_some_and(|m| m.starts_with("started")));
    let name = meta.name.clone();
    rl::cmd::stop(&f.home, &name).await.unwrap();
    rl::cmd::rm(&f.home, &name).await.unwrap();
}
