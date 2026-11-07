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

fn shift(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::SHIFT));
}

/// The cursor on the line with this key.
fn on(app: &mut App, key: &str) {
    assert!(app.keys().iter().any(|k| k == key), "{key} not in {:?}", app.keys());
    app.select(Some(key.to_string()));
}

const NATIVE: &str = "n:claude:ab12cd34";
const HELD: &str = "r:shop-sh";

#[tokio::test]
async fn ctrl_r_writes_a_name_and_an_empty_one_goes_back() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    assert!(shown(&app).contains("enter keeps it"), "the footer says how");
    ctrl(&mut app, 'u');
    typed(&mut app, "build box");
    assert!(shown(&app).contains("build box"), "typed where it stands");
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(HELD).map(String::as_str), Some("build box"));
    assert!(row_of(&shown(&app), "build box").contains("done"));
    assert_eq!(app.sel.as_deref(), Some(HELD));
    // Empty goes back to its own.
    ctrl(&mut app, 'r');
    ctrl(&mut app, 'u');
    press(&mut app, KeyCode::Enter);
    assert!(!Config::load(&f.home).unwrap().list.names.contains_key(HELD));
    assert!(shown(&app).contains("shop-sh"));
    // Esc keeps what was.
    ctrl(&mut app, 'r');
    typed(&mut app, "zzz");
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains("shop-shzzz"));
    assert!(Config::load(&f.home).unwrap().list.names.is_empty());
}

#[tokio::test]
async fn ctrl_r_renames_a_project_but_not_a_harnesss_own_session() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'r');
    assert!(matches!(app.mode, super::app::Mode::List));
    assert!(shown(&app).contains("claude names its own sessions"));
    let head = format!("g:{}", f.repo.display());
    on(&mut app, &head);
    ctrl(&mut app, 'r');
    ctrl(&mut app, 'u');
    typed(&mut app, "the shop");
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(&head).map(String::as_str), Some("the shop"));
    assert!(row_of(&shown(&app), "⎇ trunk").contains("the shop"));
}

#[tokio::test]
async fn ctrl_t_pins_to_the_top_band_and_again_lets_it_go() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 't');
    assert_eq!(Config::load(&f.home).unwrap().list.pins, [NATIVE]);
    let s = shown(&app);
    assert!(s.find("fix the login").unwrap() < s.find("⎇ trunk").unwrap(), "{s}");
    assert_eq!(app.keys()[0], NATIVE, "first on the list");
    assert_eq!(app.sel.as_deref(), Some(NATIVE), "the cursor goes with it");
    ctrl(&mut app, 't');
    assert!(Config::load(&f.home).unwrap().list.pins.is_empty());
    let s = shown(&app);
    assert!(s.find("⎇ trunk").unwrap() < s.find("fix the login").unwrap(), "{s}");
}

#[tokio::test]
async fn shift_down_swaps_two_rows_and_keeps_it() {
    let f = fixture();
    let mut app = list(&f).await;
    let head = format!("g:{}", f.repo.display());
    assert_eq!(app.keys(), [head.as_str(), HELD, NATIVE]);
    on(&mut app, HELD);
    shift(&mut app, KeyCode::Down);
    assert_eq!(app.keys(), [head.as_str(), NATIVE, HELD]);
    assert_eq!(app.sel.as_deref(), Some(HELD), "the cursor stays on it");
    let order = Config::load(&f.home).unwrap().list.order;
    assert_eq!(order.len(), 1);
    assert_eq!(order[0].group, head);
    assert_eq!(order[0].rows, [NATIVE, HELD]);
    // A list opened again has it so.
    assert_eq!(list(&f).await.keys(), [head.as_str(), NATIVE, HELD]);
    shift(&mut app, KeyCode::Down);
    assert!(shown(&app).contains("already at the bottom of its project"));
    shift(&mut app, KeyCode::Up);
    assert_eq!(app.keys(), [head.as_str(), HELD, NATIVE]);
}

#[tokio::test]
async fn ctrl_x_twice_hides_a_harnesss_own_session_and_again_brings_it_back() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ^x again to hide fix the login"));
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty(), "not yet");
    ctrl(&mut app, 'x');
    assert!(app.keys().iter().all(|k| k != NATIVE));
    assert!(shown(&app).contains("hid fix the login"));
    let hidden = Config::load(&f.home).unwrap().list.hidden;
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].key, NATIVE);
    assert_eq!(app.sel.as_deref(), Some(HELD), "the cursor on the line before");
    // With hidden on it is there, and ^x twice brings it back.
    press(&mut app, KeyCode::Char('h'));
    on(&mut app, NATIVE);
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ^x again to bring back fix the login"));
    ctrl(&mut app, 'x');
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty());
}

#[tokio::test]
async fn ctrl_x_is_disarmed_by_any_other_key() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Down);
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ^x again"), "armed afresh, not fired");
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty());
}

#[tokio::test]
async fn ctrl_x_twice_removes_an_ended_held_session_and_what_was_kept_for_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 't');
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ^x again to remove shop-sh"));
    ctrl(&mut app, 'x');
    app.finish().await;
    assert!(!f.home.session_dir("shop-sh").exists());
    let s = shown(&app);
    assert!(s.contains("removed shop-sh"), "{s}");
    assert!(app.keys().iter().all(|k| k != HELD), "{s}");
    assert!(
        Config::load(&f.home).unwrap().list.pins.is_empty(),
        "its pin goes with it"
    );
}

#[tokio::test]
async fn enter_hands_a_held_session_over_and_says_a_native_one_cannot_yet() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    press(&mut app, KeyCode::Enter);
    let hand = app.hand.take().expect("handed over");
    assert_eq!(hand.key, HELD);
    assert!(matches!(&hand.to, super::app::To::Held(m) if m.name == "shop-sh"));
    on(&mut app, NATIVE);
    press(&mut app, KeyCode::Enter);
    assert!(app.hand.is_none());
    assert!(shown(&app).contains("can't be attached from the list yet"));
}

#[tokio::test]
async fn n_opens_the_form_in_the_project_and_esc_puts_it_away() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    press(&mut app, KeyCode::Char('n'));
    let s = shown(&app);
    assert!(s.contains("new session"), "{s}");
    assert!(row_of(&s, "  folder  ").contains("shop"), "{s}");
    assert!(row_of(&s, "harness").contains("shell"), "{s}");
    // Letters are the form's now.
    press(&mut app, KeyCode::Char('a'));
    assert!(Config::load(&f.home).unwrap().list.show_agents);
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains("new session"));
}

#[test]
fn coming_back_from_an_attach_says_how_it_went() {
    use crate::attach::Ended;
    assert_eq!(
        super::back_words("shop-sh", &Ended::Detached),
        "let go of shop-sh · it keeps going"
    );
    let exit = Ended::Exit {
        code: 1,
        how: "exit 1".into(),
    };
    assert_eq!(super::back_words("shop-sh", &exit), "shop-sh ended (exit 1)");
    assert!(super::back_words("shop-sh", &Ended::Closed).starts_with("lost the connection"));
}
