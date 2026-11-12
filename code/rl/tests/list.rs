//! The list's actions on sessions a real `rl-session` holder runs: stopping
//! or removing one with `^x ^x` and its question, and starting one from the form.

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
            shell: false,
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
        app.flashing()
            .is_some_and(|m| m.contains(&format!("stop or remove {name}"))),
        "{:?}",
        app.flashing()
    );
    assert_eq!(status(&f.home, &name), Some(Status::Live), "not on the first press");
    ctrl(&mut app, 'x');
    assert!(app.work.is_empty(), "asked first");
    press(&mut app, KeyCode::Char('s'));
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
    // Its row stays, ended, until ^x twice again, then r, removes it.
    assert_eq!(app.sel.as_deref(), Some(format!("r:{name}").as_str()));
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    assert_eq!(status(&f.home, &name), None);
}

#[tokio::test]
async fn r_removes_a_live_held_shell_stopping_it_first() {
    if !holder() {
        return;
    }
    let f = fixture();
    let name = sh(&f.home, &f.folder).await;
    let mut app = App::new(f.home.clone()).unwrap();
    app.select(Some(format!("r:{name}")));
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    assert_eq!(status(&f.home, &name), None, "{:?}", app.flashing());
    assert!(
        app.flashing().is_some_and(|m| m.starts_with("removed")),
        "{:?}",
        app.flashing()
    );
}

fn run(app: &mut App, line: &str) {
    for c in line.chars() {
        press(app, KeyCode::Char(c));
    }
    press(app, KeyCode::Enter);
}

#[tokio::test]
async fn slash_rm_asks_on_a_live_held_shell_and_slash_stop_stops_it_at_once() {
    if !holder() {
        return;
    }
    let f = fixture();
    let name = sh(&f.home, &f.folder).await;
    let mut app = App::new(f.home.clone()).unwrap();
    app.select(Some(format!("r:{name}")));
    run(&mut app, "/rm");
    assert!(app.work.is_empty(), "asked first");
    press(&mut app, KeyCode::Esc);
    assert_eq!(status(&f.home, &name), Some(Status::Live), "left as it was");
    // Typing the command is the confirmation: no second press.
    run(&mut app, "/stop");
    assert!(app.cmd.is_empty());
    app.finish().await;
    let ended = || matches!(status(&f.home, &name), Some(Status::Ended(_)));
    let by = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ended() && std::time::Instant::now() < by {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ended(), "{:?}", status(&f.home, &name));
    app.reload();
    run(&mut app, "/rm");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(status(&f.home, &name), None);
}

#[tokio::test]
async fn enter_on_a_shell_line_runs_it_in_a_held_shell_that_stays() {
    if !holder() {
        return;
    }
    let f = fixture();
    let mut app = App::new(f.home.clone()).unwrap();
    // Nothing on the list: the line starts where `rl` was started.
    app.started_in = f.folder.clone();
    for c in "echo made > made.txt".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.cmd.is_empty(), "cleared once it is on its way");
    app.finish().await;
    let held = sessions::list(&f.home);
    assert_eq!(held.len(), 1, "{:?}", app.flashing());
    let meta = &held[0].meta;
    assert_eq!(meta.cwd, f.folder);
    assert_eq!(meta.kind, rl_core::state::RunKind::Shell, "it reads as a shell");
    assert_eq!(meta.harness, "sh");
    assert_eq!(meta.argv[1], "-ic");
    assert!(
        meta.argv[2].starts_with("echo made > made.txt\nexec "),
        "{:?}",
        meta.argv
    );
    assert_eq!(app.sel.as_deref(), Some(format!("r:{}", meta.name).as_str()));
    assert!(app.flashing().is_some_and(|m| m.starts_with("started")));
    // The line ran, and the shell is still there after it.
    let made = f.folder.join("made.txt");
    let by = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !made.exists() && std::time::Instant::now() < by {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(std::fs::read_to_string(&made).unwrap_or_default(), "made\n");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = meta.name.clone();
    assert_eq!(status(&f.home, &name), Some(Status::Live), "the shell stays");
    rl::cmd::stop(&f.home, &name).await.unwrap();
    rl::cmd::rm(&f.home, &name).await.unwrap();
}

#[tokio::test]
async fn a_bang_line_of_several_lines_runs_them_in_order_in_a_shell_whatever_the_default() {
    if !holder() {
        return;
    }
    let f = fixture();
    std::fs::write(f.home.config_json(), r#"{"badges": [], "list": {"default": "claude"}}"#).unwrap();
    let mut app = App::new(f.home.clone()).unwrap();
    app.started_in = f.folder.clone();
    for c in "! echo one > made.txt".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    for c in "echo two >> made.txt".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    let held = sessions::list(&f.home);
    assert_eq!(held.len(), 1, "{:?}", app.flashing());
    let meta = &held[0].meta;
    assert_eq!(meta.harness, "sh", "a shell, not the default claude");
    assert!(
        meta.argv[2].starts_with("echo one > made.txt\necho two >> made.txt\nexec "),
        "{:?}",
        meta.argv
    );
    let made = f.folder.join("made.txt");
    let by = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::fs::read_to_string(&made).unwrap_or_default() != "one\ntwo\n" && std::time::Instant::now() < by {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(std::fs::read_to_string(&made).unwrap_or_default(), "one\ntwo\n");
    let name = meta.name.clone();
    rl::cmd::stop(&f.home, &name).await.unwrap();
    rl::cmd::rm(&f.home, &name).await.unwrap();
}
