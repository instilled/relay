//! The list as drawn, on a test terminal, over a temp home.

use super::App;
use crate::config::{Config, Grouping};
use crate::sources::{NativeRow, NativeState};
use crate::store::sessions::{self, Meta};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_core::state::{RunKind, RunState};
use rl_session::exit::{Exit, write_exit_file};
use std::path::{Path, PathBuf};

struct Fixture {
    _tmp: tempfile::TempDir,
    home: Home,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success());
}

/// A home with no badges to run, and a repo on `trunk` with one of Relay's
/// shells in it that has ended.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = Home::at(root.join("relay"));
    home.ensure_store().unwrap();
    std::fs::write(home.config_json(), r#"{"badges": []}"#).unwrap();
    let repo = root.join("shop");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "trunk"]);
    let dir = home.session_dir("shop-sh");
    std::fs::create_dir_all(&dir).unwrap();
    let started = Timestamp::now() - SignedDuration::from_mins(5);
    sessions::write_meta(
        &dir,
        &Meta {
            name: "shop-sh".into(),
            harness: "sh".into(),
            kind: RunKind::Shell,
            cwd: repo.clone(),
            argv: vec!["sh".into()],
            started,
            socket: dir.join("sock"),
            holder_pid: None,
            native_id: None,
        },
    )
    .unwrap();
    write_exit_file(
        &dir.join("exit"),
        &Exit {
            code: 0,
            signal: None,
            at: started + SignedDuration::from_mins(1),
        },
    )
    .unwrap();
    Fixture { _tmp: tmp, home, repo }
}

/// One of Claude's own sessions, in `cwd`, waiting on you.
fn waiting(cwd: &Path) -> NativeRow {
    let at = Timestamp::now() - SignedDuration::from_mins(2);
    NativeRow {
        harness: "claude".into(),
        id: "ab12cd34".into(),
        session_id: "s-1".into(),
        name: "fix the login".into(),
        cwd: cwd.to_path_buf(),
        state: NativeState {
            words: "needs input".into(),
            state: RunState::NeedsInput,
        },
        summary: Some("asks which port".into()),
        started: Some(at),
        ended: None,
        at: Some(at),
        pr: None,
    }
}

async fn list(f: &Fixture) -> App {
    let mut app = App::new(f.home.clone()).unwrap();
    app.set_native(vec![waiting(&f.repo)]);
    app
}

/// The screen as rows of text, drawn at this size.
fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

fn shown(app: &App) -> String {
    screen(app, 100, 30).join("\n")
}

fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::from(code));
}

fn ctrl(app: &mut App, c: char) {
    app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn row_of<'a>(screen: &'a str, text: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(text))
        .unwrap_or_else(|| panic!("{text:?} is not on the screen:\n{screen}"))
}

#[tokio::test]
async fn a_project_stands_with_its_git_line_over_a_held_and_a_native_row() {
    let f = fixture();
    let app = list(&f).await;
    let s = shown(&app);
    let head = row_of(&s, "⎇ trunk");
    assert!(head.contains("shop"), "{head}");
    let held = row_of(&s, "shop-sh");
    assert!(held.contains("done"), "{held}");
    let native = row_of(&s, "fix the login");
    assert!(
        native.contains("needs input") && native.contains("asks which port"),
        "{native}"
    );
    // Header, then its rows.
    let at = |t: &str| s.find(t).unwrap();
    assert!(
        at("⎇ trunk") < at("shop-sh") && at("shop-sh") < at("fix the login"),
        "{s}"
    );
    assert!(s.contains("relay"), "{s}");
    assert!(s.contains("1 needs you"), "the header's tally: {s}");
}

#[tokio::test]
async fn a_hides_the_agents_hints_on_the_header_and_is_kept() {
    let f = fixture();
    let mut app = list(&f).await;
    press(&mut app, KeyCode::Char('a'));
    let s = shown(&app);
    assert!(!s.contains("fix the login"), "{s}");
    assert!(row_of(&s, "⎇ trunk").contains("! 1 needs you"), "{s}");
    assert!(s.contains("shop-sh"), "a shell is not an agent: {s}");
    assert!(!Config::load(&f.home).unwrap().list.show_agents, "kept in config.json");
    // A list opened again opens the same way.
    let again = list(&f).await;
    assert!(!shown(&again).contains("fix the login"));
    press(&mut app, KeyCode::Char('a'));
    assert!(shown(&app).contains("fix the login"));
    assert!(Config::load(&f.home).unwrap().list.show_agents);
}

#[tokio::test]
async fn ctrl_s_groups_by_state_and_back_and_is_kept() {
    let f = fixture();
    let mut app = list(&f).await;
    ctrl(&mut app, 's');
    let s = shown(&app);
    let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t:?} not in\n{s}"));
    assert!(at("Needs input") < at("fix the login"), "{s}");
    assert!(at("fix the login") < at("Done") && at("Done") < at("shop-sh"), "{s}");
    assert!(!s.contains("⎇ trunk"), "no project headers by state: {s}");
    assert!(row_of(&s, "fix the login").contains("shop › fix the login"), "{s}");
    assert_eq!(Config::load(&f.home).unwrap().list.grouping, Grouping::State);
    ctrl(&mut app, 's');
    assert!(shown(&app).contains("⎇ trunk"));
    assert_eq!(Config::load(&f.home).unwrap().list.grouping, Grouping::Project);
}

#[tokio::test]
async fn a_search_narrows_the_list_and_esc_clears_it() {
    let f = fixture();
    let mut app = list(&f).await;
    ctrl(&mut app, 'f');
    typed(&mut app, "login");
    let s = shown(&app);
    assert!(s.contains("fix the login") && !s.contains("shop-sh"), "{s}");
    assert!(
        row_of(&s, "⌕").contains("login"),
        "the search is typed on the toggles line: {s}"
    );
    assert_eq!(
        app.sel.as_deref(),
        Some("n:claude:ab12cd34"),
        "the cursor on the best match"
    );
    // Enter keeps it; a letter is the list's again.
    press(&mut app, KeyCode::Enter);
    assert!(app.search.is_none());
    assert!(!shown(&app).contains("shop-sh"));
    press(&mut app, KeyCode::Esc);
    let s = shown(&app);
    assert!(s.contains("fix the login") && s.contains("shop-sh"), "{s}");
}

#[tokio::test]
async fn the_cursor_walks_the_lines_and_stops_at_the_ends() {
    let f = fixture();
    let mut app = list(&f).await;
    let keys = app.keys();
    assert_eq!(keys.len(), 3, "{keys:?}");
    assert_eq!(app.sel.as_ref(), Some(&keys[0]));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.sel.as_ref(), Some(&keys[1]));
    ctrl(&mut app, 'j');
    assert_eq!(app.sel.as_ref(), Some(&keys[2]));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.sel.as_ref(), Some(&keys[2]), "the end");
    ctrl(&mut app, 'k');
    assert_eq!(app.sel.as_ref(), Some(&keys[1]));
    press(&mut app, KeyCode::Home);
    assert_eq!(app.sel.as_ref(), Some(&keys[0]));
    press(&mut app, KeyCode::End);
    assert_eq!(app.sel.as_ref(), Some(&keys[2]));
    // A page is the list's height, as last drawn.
    shown(&app);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.sel.as_ref(), Some(&keys[0]));
}

#[tokio::test]
async fn ctrl_c_twice_quits_and_once_only_says_so() {
    let f = fixture();
    let mut app = list(&f).await;
    ctrl(&mut app, 'c');
    assert!(!app.quit);
    assert!(shown(&app).contains("press ^c again to quit"));
    press(&mut app, KeyCode::Down);
    ctrl(&mut app, 'c');
    assert!(!app.quit, "another key between disarms it");
    ctrl(&mut app, 'c');
    assert!(app.quit);
}

#[tokio::test]
async fn ctrl_h_and_a_question_mark_show_the_shortcuts_and_esc_puts_them_away() {
    let f = fixture();
    let mut app = list(&f).await;
    assert!(shown(&app).contains("^h shortcuts"));
    ctrl(&mut app, 'h');
    assert!(shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::Char('?'));
    assert!(shown(&app).contains("by project/state"));
}

#[tokio::test]
async fn a_change_in_the_files_is_on_the_list_once_it_is_read_again() {
    let f = fixture();
    let mut app = list(&f).await;
    sessions::remove(&f.home, "shop-sh").unwrap();
    assert!(shown(&app).contains("shop-sh"), "until it reads again");
    app.reload();
    assert!(!shown(&app).contains("shop-sh"));
    // The native rows last said stand across a reload.
    assert!(shown(&app).contains("fix the login"));
}

#[tokio::test]
async fn a_click_puts_the_cursor_on_its_line() {
    let f = fixture();
    let mut app = list(&f).await;
    let rows = screen(&app, 100, 30);
    let y = rows.iter().position(|r| r.contains("fix the login")).unwrap() as u16;
    app.mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 10,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sel.as_deref(), Some("n:claude:ab12cd34"));
}
