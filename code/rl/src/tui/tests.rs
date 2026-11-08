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
        color: None,
    }
}

async fn list(f: &Fixture) -> App {
    let mut app = App::new(f.home.clone()).unwrap();
    app.set_native(vec![waiting(&f.repo)]);
    // A PATH of its own: claude and codex are here, agy is not.
    let bin = f.repo.parent().unwrap().join("bin");
    app.commands = std::cell::OnceCell::from(super::cmdline::bin_dir(&bin, &["claude", "codex", "cat", "make"]));
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
async fn a_row_says_its_harness_before_its_state_at_80_columns() {
    let f = fixture();
    let app = list(&f).await;
    let s = screen(&app, 80, 24).join("\n");
    let native = row_of(&s, "fix the login");
    let held = row_of(&s, "shop-sh");
    let col = |line: &str, t: &str| line[..line.find(t).unwrap()].chars().count();
    assert!(native.contains(" claude needs input"), "{native}");
    assert!(held.contains(" sh "), "{held}");
    // The harness ends the title's column; the state starts where it did.
    assert_eq!(col(native, "needs input"), col(held, "done"), "{s}");
    assert_eq!(col(native, "claude") + 7, col(native, "needs input"), "{native}");
    assert!(native.contains("asks which"), "the summary still has room: {native}");
}

#[tokio::test]
async fn ctrl_a_hides_the_agents_hints_on_the_header_and_is_kept() {
    let f = fixture();
    let mut app = list(&f).await;
    ctrl(&mut app, 'a');
    let s = shown(&app);
    assert!(!s.contains("fix the login"), "{s}");
    assert!(row_of(&s, "⎇ trunk").contains("! 1 needs you"), "{s}");
    assert!(s.contains("shop-sh"), "a shell is not an agent: {s}");
    assert!(!Config::load(&f.home).unwrap().list.show_agents, "kept in config.json");
    // A list opened again opens the same way.
    let again = list(&f).await;
    assert!(!shown(&again).contains("fix the login"));
    ctrl(&mut app, 'a');
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
        "the search is typed where the command line was: {s}"
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
    assert_eq!(app.sel.as_ref(), Some(&keys[1]), "opens on a session, not the header");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.sel.as_ref(), Some(&keys[0]), "the header is there to move to");
    app.reload();
    assert_eq!(app.sel.as_ref(), Some(&keys[0]), "and stays while it exists");
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
async fn the_cursor_lands_on_a_session_when_the_list_opens_and_when_its_line_is_gone() {
    let f = fixture();
    let mut app = list(&f).await;
    let first_row = |app: &App| app.lines().iter().find_map(|l| l.row().map(|r| r.key.clone()));
    assert_eq!(app.sel, first_row(&app));
    assert!(app.selected().unwrap().row().is_some());
    app.select(Some("gone".into()));
    app.reload();
    assert_eq!(app.sel, first_row(&app), "after a refresh that lost it");
    let drawn = shown(&app);
    assert!(drawn.contains("shop-sh"), "{drawn}");
}

#[tokio::test]
async fn a_finished_rows_age_is_frozen_from_its_start_to_its_end() {
    let f = fixture();
    let app = list(&f).await;
    let s = shown(&app);
    // Started 5 minutes ago, ended a minute in: 1m, not the 4m since it ended.
    assert!(row_of(&s, "shop-sh").trim_end().ends_with("1m"), "{s}");
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
async fn ctrl_h_and_f1_show_the_shortcuts_esc_puts_them_away_and_a_question_mark_types() {
    let f = fixture();
    let mut app = list(&f).await;
    assert!(shown(&app).contains("^h/F1 shortcuts"));
    ctrl(&mut app, 'h');
    assert!(shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::F(1));
    assert!(shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::F(1));
    assert!(!shown(&app).contains("by project/state"));
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(app.cmd.line.text(), "?", "a ? is typed");
    assert!(!app.shortcuts);
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
async fn ctrl_x_twice_stops_a_harnesss_own_session_through_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    assert!(shown(&app).contains("^x stop"), "the hint says what ^x does here");
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ^x again to stop fix the login"));
    assert!(ran_in(&f.repo).is_empty(), "not on the first press");
    ctrl(&mut app, 'x');
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["stop", "ab12cd34"], "claude stop <id>, in its folder");
    assert!(shown(&app).contains("stopped fix the login"));
    assert!(app.ask_native, "the list asks claude again at once");
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty(), "not hidden");
}

#[tokio::test]
async fn ctrl_x_twice_hides_a_harnesss_own_session_and_again_brings_it_back() {
    let f = fixture();
    let mut app = list(&f).await;
    // Done: nothing to stop.
    let mut done = waiting(&f.repo);
    done.state = NativeState {
        words: "completed".into(),
        state: RunState::Finished,
    };
    app.set_native(vec![done]);
    on(&mut app, NATIVE);
    assert!(shown(&app).contains("^x hide"));
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
    ctrl(&mut app, 'o');
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
async fn enter_hands_a_held_session_over_and_a_native_one_to_its_harness() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    press(&mut app, KeyCode::Enter);
    let hand = app.hand.take().expect("handed over");
    assert_eq!(hand.key, HELD);
    assert!(matches!(&hand.to, super::app::To::Held(m) if m.name == "shop-sh"));
    on(&mut app, NATIVE);
    press(&mut app, KeyCode::Enter);
    let hand = app.hand.take().expect("handed over");
    assert_eq!(hand.key, NATIVE);
    let super::app::To::Argv(argv) = &hand.to else {
        panic!("{:?}", hand.to);
    };
    assert_eq!(argv[1..], ["attach", "ab12cd34"], "claude attach <id>");
}

/// What the harness's stand-in was last run with in `dir` (`sources`).
fn ran_in(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("argv"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn the_line_starts_claude_in_its_own_background_and_the_cursor_waits_for_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    typed(&mut app, "claude fix the login");
    assert!(row_of(&shown(&app), "shop ❯").contains("agent · claude"));
    press(&mut app, KeyCode::Enter);
    assert!(app.cmd.is_empty(), "the line is cleared once it is on its way");
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["--bg", "fix the login"], "run in the row's folder");
    assert!(
        sessions::list(&f.home).iter().all(|h| h.meta.harness != "claude"),
        "not one Relay holds"
    );
    assert!(shown(&app).contains("started claude 0a1b2c3d in its background"));
    // Not listed yet: the harness is asked now, and the cursor goes to it
    // once it is.
    let new = "n:claude:0a1b2c3d";
    assert_eq!(app.awaiting.as_deref(), Some(new));
    assert!(app.ask_native);
    let mut started = waiting(&f.repo);
    started.id = "0a1b2c3d".into();
    started.session_id = "s-new".into();
    app.set_native(vec![waiting(&f.repo), started]);
    assert_eq!(app.sel.as_deref(), Some(new));
    assert_eq!(app.awaiting, None);
}

/// A Claude session open in another terminal, as its hooks told of it.
fn hooked(f: &Fixture, sid: &str, state: crate::store::state::Said) {
    use crate::store::state::{self, HookState};
    let s = HookState {
        agent: "claude".into(),
        session_id: sid.into(),
        cwd: f.repo.clone(),
        state,
        detail: None,
        pid: None,
        transcript_path: None,
        started: Timestamp::now(),
        at: Timestamp::now(),
    };
    state::write(&state::path(&f.home, "claude", sid).unwrap(), &s).unwrap();
}

#[tokio::test]
async fn enter_carries_an_idle_claude_on_in_its_background_and_attaches() {
    use crate::store::state::Said;
    let f = fixture();
    hooked(&f, "c-idle", Said::Idle);
    hooked(&f, "c-busy", Said::Busy);
    let mut app = list(&f).await;
    on(&mut app, "o:claude:c-busy");
    press(&mut app, KeyCode::Enter);
    assert!(app.work.is_empty() && app.hand.is_none());
    assert!(shown(&app).contains("busy in its own terminal"));

    on(&mut app, "o:claude:c-idle");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["--resume", "c-idle", "--bg"], "in its folder");
    let hand = app.hand.take().expect("handed over");
    assert_eq!(hand.key, "n:claude:0a1b2c3d");
    let super::app::To::Argv(argv) = &hand.to else {
        panic!("{:?}", hand.to);
    };
    assert_eq!(argv[1..], ["attach", "0a1b2c3d"], "attached by the new id");
}

#[tokio::test]
async fn a_pull_request_reads_as_its_number() {
    let f = fixture();
    let mut app = list(&f).await;
    let mut pr = waiting(&f.repo);
    pr.pr = Some(crate::sources::Pr {
        number: 1358,
        href: "https://x/pull/1358".into(),
    });
    app.set_native(vec![pr]);
    let s = shown(&app);
    assert!(row_of(&s, "fix the login").contains("#1358"), "{s}");
    assert!(app.prs.is_empty(), "the badges off: gh is not asked");
}

#[tokio::test]
async fn an_idle_claude_is_not_carried_on_before_claude_has_listed_its_own() {
    use crate::store::state::Said;
    let f = fixture();
    hooked(&f, "c-idle", Said::Idle);
    let mut app = App::new(f.home.clone()).unwrap();
    on(&mut app, "o:claude:c-idle");
    press(&mut app, KeyCode::Enter);
    assert!(app.work.is_empty() && app.hand.is_none());
    assert!(shown(&app).contains("waiting for claude to list its own sessions"));
    assert!(ran_in(&f.repo).is_empty(), "nothing started");
    // It has answered, without it: now it is carried on.
    app.set_native(vec![]);
    on(&mut app, "o:claude:c-idle");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["--resume", "c-idle", "--bg"]);
}

#[tokio::test]
async fn a_lister_that_does_not_answer_says_so_and_its_rows_stay() {
    let f = fixture();
    let mut app = list(&f).await;
    let since = Timestamp::now();
    let mut said = app.native.clone();
    said.failing = vec![super::live::Failing {
        harness: "claude".into(),
        error: "`claude agents` did not answer in 10s".into(),
        since: Some(since),
    }];
    app.heard(said);
    let at = since
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%H:%M")
        .to_string();
    let s = shown(&app);
    assert!(
        s.contains(&format!("claude agents did not answer in 10s · showing {at}")),
        "{s}"
    );
    assert!(app.keys().iter().any(|k| k == NATIVE), "what it said last stands");
    // It answers again: the footer is the hints again.
    let mut said = app.native.clone();
    said.failing.clear();
    app.heard(said);
    assert!(!shown(&app).contains("did not answer"));
}

#[tokio::test]
async fn a_start_that_came_to_nothing_waits_for_no_line() {
    let f = fixture();
    let mut app = list(&f).await;
    app.awaiting = Some("n:claude:0a1b2c3d".into());
    app.done(super::app::Done::gave_up("start claude: not logged in"));
    assert_eq!(app.awaiting, None);
    assert!(shown(&app).contains("start claude: not logged in"));
}

#[tokio::test]
async fn a_carry_on_done_while_a_name_is_typed_does_not_take_the_terminal() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    app.done(super::app::Done {
        hand: Some(super::app::Hand {
            to: super::app::To::Argv(vec!["claude".into(), "attach".into(), "ab12cd34".into()]),
            title: "fix the login".into(),
            key: NATIVE.into(),
        }),
        ..super::app::Done::said("fix the login carries on in claude's background")
    });
    assert!(app.hand.is_none(), "the name keeps the keys");
    assert!(shown(&app).contains("fix the login carries on · enter on it attaches"));
    assert!(matches!(app.mode, super::app::Mode::Rename(_)));
}

#[tokio::test]
async fn a_broken_config_json_is_never_written_over() {
    let f = fixture();
    let mut app = list(&f).await;
    std::fs::write(f.home.config_json(), "{ broken").unwrap();
    ctrl(&mut app, 'a');
    assert_eq!(std::fs::read_to_string(f.home.config_json()).unwrap(), "{ broken");
    assert!(shown(&app).contains("not kept: config.json does not read"));
    assert!(!app.prefs().show_agents, "the change stands on this list");
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

/// The command line's row on the screen: over the footer's separator.
fn line_row(app: &App) -> String {
    let rows = screen(app, 100, 30);
    rows[rows.len() - 3].clone()
}

#[tokio::test]
async fn the_header_carries_the_filters_and_the_grouping_and_the_foot_does_not() {
    use ratatui::style::Modifier;
    let f = fixture();
    let mut app = list(&f).await;
    let rows = screen(&app, 100, 30);
    let at = |rows: &[String], t: &str| rows.iter().position(|r| r.contains(t));
    let head = at(&rows, "✦^a").expect("the toggles are on screen");
    assert!(head < 6, "in the header, beside the mark: {}", rows.join("\n"));
    assert!(rows[head].contains("❯^l") && rows[head].contains("◌^o") && rows[head].contains("by project ^s"));
    assert!(at(&rows, "relay").unwrap() < head && at(&rows, "1 needs you").unwrap() < head);
    for r in &rows[6..] {
        assert!(
            !r.contains("✦^a") && !r.contains("by project"),
            "no toggles line at the foot: {r}"
        );
    }
    // `^a` dims the agents' mark, where it stands.
    let mark = |app: &App| {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        let y = head as u16;
        let x = (0..100).find(|&x| buf[(x, y)].symbol() == "✦").unwrap();
        buf[(x, y)].clone()
    };
    assert!(!mark(&app).modifier.contains(Modifier::DIM), "on");
    ctrl(&mut app, 'a');
    assert!(mark(&app).modifier.contains(Modifier::DIM), "off");
    // `^s`: by state, said in the header.
    ctrl(&mut app, 's');
    let rows = screen(&app, 100, 30);
    assert!(
        rows[..6].iter().any(|r| r.contains("by state ^s")),
        "{}",
        rows.join("\n")
    );
    // On a short screen the header is one line, and keeps them.
    let rows = screen(&app, 80, 20);
    assert!(rows[0].contains("✦^a") && rows[0].contains("by state"), "{}", rows[0]);
    let rows = screen(&app, 60, 20);
    assert!(rows[0].contains("✦^a") && rows[0].contains("by state"), "{}", rows[0]);
}

#[tokio::test]
async fn the_search_takes_the_command_lines_place_while_it_is_open() {
    let f = fixture();
    let mut app = list(&f).await;
    assert!(line_row(&app).contains(" ❯ "), "{}", line_row(&app));
    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "login");
    let row = line_row(&app);
    assert!(row.contains("⌕ login") && !row.contains(" ❯ "), "{row}");
    press(&mut app, KeyCode::Esc);
    assert!(line_row(&app).contains(" ❯ "));
}

#[tokio::test]
async fn typing_starts_the_line_in_the_rows_folder() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    assert!(line_row(&app).contains("shop ❯"), "where it starts: {}", line_row(&app));
    typed(&mut app, "make test");
    assert_eq!(app.cmd.line.text(), "make test");
    let row = line_row(&app);
    assert!(row.contains("shop ❯ make test"), "{row}");
    assert!(row.contains("shell"), "{row}");
    assert!(shown(&app).contains("enter starts"), "the footer says how");
    // A letter of the list's types once the line has text.
    assert!(Config::load(&f.home).unwrap().list.show_agents);
    // ←, then a letter, goes in where the cursor is; esc clears it all.
    press(&mut app, KeyCode::Left);
    typed(&mut app, "s");
    assert_eq!(app.cmd.line.text(), "make tesst");
    ctrl(&mut app, 'u');
    assert_eq!(app.cmd.line.text(), "t");
    press(&mut app, KeyCode::Delete);
    assert!(app.cmd.is_empty());
    typed(&mut app, "ls");
    press(&mut app, KeyCode::Esc);
    assert!(app.cmd.is_empty());
    // Moving on the list still works with text on the line.
    typed(&mut app, "ls");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.sel.as_deref(), Some(HELD));
    assert_eq!(app.cmd.line.text(), "ls");
}

#[tokio::test]
async fn the_filters_are_on_ctrl_keys_and_every_letter_types() {
    let f = fixture();
    let mut app = list(&f).await;
    // Plain letters start the line, a, s and h too.
    typed(&mut app, "ash");
    assert_eq!(app.cmd.line.text(), "ash");
    let p = Config::load(&f.home).unwrap().list;
    assert!(p.show_agents && p.show_shells && !p.show_hidden, "nothing flipped");
    // ^a ^l ^o flip them, with text on the line or none.
    for (c, what) in [('a', "agents"), ('l', "shells"), ('o', "hidden")] {
        ctrl(&mut app, c);
        assert_eq!(app.cmd.line.text(), "ash", "{c} leaves the line be");
        let p = Config::load(&f.home).unwrap().list;
        let now = match c {
            'a' => !p.show_agents,
            'l' => !p.show_shells,
            _ => p.show_hidden,
        };
        assert!(now, "{what} flipped");
    }
    press(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'a');
    assert!(Config::load(&f.home).unwrap().list.show_agents, "on an empty line too");
    // Home and end go to the line's ends; ^a is no longer one of them.
    typed(&mut app, "make test");
    press(&mut app, KeyCode::Home);
    typed(&mut app, "x");
    assert_eq!(app.cmd.line.text(), "xmake test");
    press(&mut app, KeyCode::End);
    typed(&mut app, "!");
    assert_eq!(app.cmd.line.text(), "xmake test!");
    assert!(
        !shown(&app).lines().any(|l| l.contains("✦a ")),
        "the header names the ctrl keys"
    );
}

#[tokio::test]
async fn a_slash_on_an_empty_line_opens_the_search() {
    let f = fixture();
    let mut app = list(&f).await;
    press(&mut app, KeyCode::Char('/'));
    assert!(app.search.is_some());
    typed(&mut app, "login");
    assert_eq!(app.filter, "login");
    assert!(row_of(&shown(&app), "⌕").contains("login"));
    press(&mut app, KeyCode::Enter);
    // Mid-line it is a slash.
    typed(&mut app, "cat src/");
    assert!(app.search.is_none());
    assert_eq!(app.cmd.line.text(), "cat src/");
}

#[tokio::test]
async fn the_line_says_what_it_would_start() {
    let f = fixture();
    let mut app = list(&f).await;
    for (text, label) in [
        ("claude fix the login", "agent · claude"),
        ("codex", "agent · codex"),
        ("gemini write the docs", "agent · antigravity (agy not found)"),
        ("make test", "shell"),
    ] {
        typed(&mut app, text);
        let row = line_row(&app);
        assert!(row.contains(label), "{text}: {row}");
        press(&mut app, KeyCode::Esc);
    }
    // A harness that is not here is said, and the line kept.
    typed(&mut app, "agy hi");
    assert!(line_row(&app).contains("agent · antigravity (agy not found)"));
    press(&mut app, KeyCode::Enter);
    assert!(app.work.is_empty(), "{:?} {:?}", app.flashing(), app.cmd.line.text());
    assert_eq!(app.cmd.line.text(), "agy hi");
    assert!(shown(&app).contains("agy is not on PATH"), "{}", shown(&app));
}

#[tokio::test]
async fn enter_on_an_empty_line_still_attaches() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.hand.take().expect("handed over").key, HELD);
    assert!(app.work.is_empty(), "nothing started");
}

#[tokio::test]
async fn tab_completes_a_harness_word_and_a_path_and_the_ghost_shows_it() {
    let f = fixture();
    std::fs::create_dir_all(f.repo.join("src")).unwrap();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    typed(&mut app, "cod");
    assert_eq!(app.cmd.ghost.as_deref(), Some("ex"));
    assert!(line_row(&app).contains("shop ❯ codex"), "the ghost: {}", line_row(&app));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "codex ");
    typed(&mut app, "look at sr");
    assert!(line_row(&app).contains("look at src/"), "{}", line_row(&app));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "codex look at src/", "under the row's folder");
}

/// The cells of the row holding `text`, drawn at 100×30: where `text`
/// starts, and the buffer.
fn cells_of(app: &App, text: &str) -> (ratatui::buffer::Buffer, u16, u16) {
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    t.draw(|f| app.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..30 {
        let row: String = (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect();
        if let Some(at) = row.find(text) {
            let x = row[..at].chars().count() as u16;
            return (buf, x, y);
        }
    }
    panic!("{text:?} is not on the screen");
}

#[tokio::test]
async fn a_claude_session_keeps_its_colour_on_the_list_but_not_under_the_cursor() {
    use ratatui::style::Color;
    let f = fixture();
    let mut app = list(&f).await;
    let mut pink = waiting(&f.repo);
    pink.color = Some("pink".into());
    app.set_native(vec![pink]);
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].fg, Color::Indexed(213), "the name, pink");
    assert_eq!(buf[(1, y)].fg, Color::Indexed(213), "and its icon");
    // Without one, the name is the foreground's, as ever.
    app.set_native(vec![waiting(&f.repo)]);
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].fg, Color::Reset);
    app.set_native(vec![{
        let mut p = waiting(&f.repo);
        p.color = Some("pink".into());
        p
    }]);
    on(&mut app, NATIVE);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(
        buf[(x, y)].fg,
        crate::tui::theme::selected().fg.unwrap(),
        "the cursor's band wins"
    );
    on(&mut app, HELD);
    ctrl(&mut app, 'f');
    typed(&mut app, "login");
    press(&mut app, KeyCode::Enter);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert!(
        buf[(x + 8, y)].modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "the search's mark still shows"
    );
}
