//! The list as drawn, on a test terminal, over a temp home.

use super::{App, help};
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
        elsewhere: false,
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
    assert!(shown(&app).contains("press ctrl+c again to quit"));
    press(&mut app, KeyCode::Down);
    ctrl(&mut app, 'c');
    assert!(!app.quit, "another key between disarms it");
    ctrl(&mut app, 'c');
    assert!(app.quit);
}

#[tokio::test]
async fn ctrl_h_and_f1_toggle_the_grid_esc_closes_it_and_a_question_mark_types() {
    let f = fixture();
    let mut app = list(&f).await;
    const GRID: &str = "ctrl+t to pin to top";
    assert!(shown(&app).contains("ctrl+h/f1 for help"), "{}", shown(&app));
    assert!(!shown(&app).contains(GRID));
    ctrl(&mut app, 'h');
    assert!(shown(&app).contains(GRID));
    ctrl(&mut app, 'h');
    assert!(!shown(&app).contains(GRID), "ctrl+h again puts it away");
    ctrl(&mut app, 'h');
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains(GRID), "esc closes it");
    run(&mut app, "/help");
    assert!(shown(&app).contains(GRID), "/help opens it");
    // With a line typed, esc closes the grid first and leaves the line.
    typed(&mut app, "make");
    ctrl(&mut app, 'h');
    assert!(shown(&app).contains(GRID));
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app).contains(GRID));
    assert_eq!(app.cmd.line.text(), "make");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::F(1));
    assert!(shown(&app).contains(GRID));
    press(&mut app, KeyCode::F(1));
    assert!(!shown(&app).contains(GRID));
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(app.cmd.line.text(), "?", "a ? is typed");
    assert!(!app.help);
}

/// The grid's rows on a screen `w` wide: those under the command line.
fn grid_rows(app: &App, w: u16) -> Vec<String> {
    let rows = screen(app, w, 40);
    let line = rows.iter().rposition(|r| r.contains(" ❯ ")).expect("the command line");
    rows[line + 2..].iter().map(|r| r.trim_end().to_string()).collect()
}

#[tokio::test]
async fn the_grid_stands_under_the_command_line_in_four_columns_wide_and_two_at_80() {
    let f = fixture();
    let mut app = list(&f).await;
    ctrl(&mut app, 'h');
    for (w, cols) in [(140, 4), (80, 2)] {
        let rows = grid_rows(&app, w);
        assert_eq!(
            rows.len(),
            help::KEYS.len().div_ceil(cols),
            "at {w}:\n{}",
            rows.join("\n")
        );
        assert!(rows[0].starts_with("  enter to attach"), "at {w}: {}", rows[0]);
        // Each entry whole, in its row, and its row's columns where they
        // should be.
        for (i, k) in help::KEYS.iter().enumerate() {
            let text = help::text(k);
            let row = &rows[i / cols];
            assert!(row.contains(&text), "at {w}: {text:?} is cut in {row:?}");
            let at = row[..row.find(&text).unwrap()].chars().count();
            assert_eq!(at, 2 + (i % cols) * ((w as usize - 2) / cols), "at {w}: {row:?}");
        }
        assert!(
            !rows.iter().any(|r| r.contains("ctrl+h/f1 for help")),
            "the grid stands in the hints' place"
        );
    }
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

fn mouse(app: &mut App, kind: crossterm::event::MouseEventKind, row: u16) {
    app.mouse(crossterm::event::MouseEvent {
        kind,
        column: 10,
        row,
        modifiers: KeyModifiers::NONE,
    });
}

#[tokio::test]
async fn a_pointer_that_only_moves_leaves_the_cursor_and_the_wheel_steps_it() {
    use crossterm::event::MouseEventKind::{Moved, ScrollDown, ScrollUp};
    let f = fixture();
    let mut app = list(&f).await;
    let rows = screen(&app, 100, 30);
    let at = |text: &str| rows.iter().position(|r| r.contains(text)).unwrap() as u16;
    let keys = app.keys();
    on(&mut app, &keys[0]);
    // Over another line, the cursor stays where the keys or the wheel put it.
    mouse(&mut app, Moved, at("fix the login"));
    assert_eq!(app.sel.as_ref(), Some(&keys[0]), "a hover does not select");
    mouse(&mut app, ScrollDown, at("fix the login"));
    mouse(&mut app, ScrollDown, at("fix the login"));
    assert_eq!(app.sel.as_ref(), Some(&keys[2]), "one line a tick");
    mouse(&mut app, Moved, at("trunk"));
    assert_eq!(app.sel.as_ref(), Some(&keys[2]));
    mouse(&mut app, ScrollUp, at("trunk"));
    assert_eq!(app.sel.as_ref(), Some(&keys[1]));
}

/// The list as read now is the list built afresh: nothing stale.
fn fresh(app: &App) {
    assert_eq!(app.lines(), app.lines_with(&app.filter), "the list is stale");
    assert_eq!(
        app.keys(),
        app.lines_with(&app.filter).iter().map(|l| l.key()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn the_list_built_once_is_never_stale() {
    let f = fixture();
    let mut app = list(&f).await;
    fresh(&app);
    let other = f.repo.parent().unwrap().join("yard");
    std::fs::create_dir_all(&other).unwrap();
    app.set_native(vec![
        waiting(&f.repo),
        NativeRow {
            id: "ef56ab78".into(),
            name: "paint the fence".into(),
            ..waiting(&other)
        },
    ]);
    fresh(&app);
    assert!(app.keys().iter().any(|k| k == "n:claude:ef56ab78"));
    assert!(shown(&app).contains("paint the fence"));
    // The search, the toggles, the grouping, a name, a pin, a hide.
    ctrl(&mut app, 'f');
    typed(&mut app, "fence");
    fresh(&app);
    assert!(!shown(&app).contains("fix the login"));
    press(&mut app, KeyCode::Esc);
    fresh(&app);
    assert!(shown(&app).contains("fix the login"));
    ctrl(&mut app, 'a');
    fresh(&app);
    assert!(!shown(&app).contains("fix the login"));
    ctrl(&mut app, 'a');
    ctrl(&mut app, 's');
    fresh(&app);
    assert!(shown(&app).contains("Needs input"));
    ctrl(&mut app, 's');
    on(&mut app, NATIVE);
    run(&mut app, "/rename the login");
    fresh(&app);
    assert!(shown(&app).contains("the login"));
    ctrl(&mut app, 't');
    fresh(&app);
    assert_eq!(app.keys()[0], NATIVE);
    run(&mut app, "/hide");
    fresh(&app);
    assert!(!app.keys().iter().any(|k| k == NATIVE));
    // The files read again, and the checkouts.
    sessions::remove(&f.home, "shop-sh").unwrap();
    app.reload();
    fresh(&app);
    assert!(!app.keys().iter().any(|k| k == HELD));
    git(&other, &["init", "-q", "-b", "yard-main"]);
    app.regit();
    fresh(&app);
    assert!(shown(&app).contains("yard-main"), "{}", shown(&app));
}

#[tokio::test]
async fn events_waiting_are_taken_together_up_to_one_that_hands_the_terminal_over() {
    use crossterm::event::Event;
    let f = fixture();
    let mut app = list(&f).await;
    let keys = app.keys();
    on(&mut app, &keys[0]);
    let key = |c| Ok(Event::Key(KeyEvent::from(c)));
    // Two downs waiting behind the first: all three taken, at the end.
    let mut more = futures::stream::iter(vec![key(KeyCode::Down), key(KeyCode::Down)]);
    app.events(Some(key(KeyCode::Down)), &mut more).await.unwrap();
    assert_eq!(app.sel.as_ref(), Some(&keys[2]));
    assert!(futures::StreamExt::next(&mut more).await.is_none(), "all taken");
    // An enter that hands the terminal over: what comes after is not the
    // list's, and stays where it was.
    on(&mut app, NATIVE);
    let mut more = futures::stream::iter(vec![key(KeyCode::Down)]);
    app.events(Some(key(KeyCode::Enter)), &mut more).await.unwrap();
    assert!(app.hand.is_some());
    assert_eq!(app.sel.as_deref(), Some(NATIVE));
    assert!(futures::StreamExt::next(&mut more).await.is_some(), "left waiting");
    // The end of the input quits.
    app.hand = None;
    let mut more = futures::stream::iter(Vec::<std::io::Result<Event>>::new());
    app.events(None, &mut more).await.unwrap();
    assert!(app.quit);
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
async fn ctrl_r_renames_a_project() {
    let f = fixture();
    let mut app = list(&f).await;
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
async fn with_hidden_shown_rows_move_too_and_a_hidden_one_says_so() {
    let f = fixture();
    let mut app = list(&f).await;
    let head = format!("g:{}", f.repo.display());
    on(&mut app, NATIVE);
    run(&mut app, "/hide");
    // Hidden off: it's off the list though it needs you, and the header's
    // toggle counts it.
    assert_eq!(app.keys(), [head.as_str(), HELD]);
    assert!(shown(&app).contains("◌ hidden 1"), "{}", shown(&app));
    ctrl(&mut app, 'o');
    assert!(app.prefs().show_hidden);
    assert!(!shown(&app).contains("◌ hidden 1"), "no count while they're shown");
    assert_eq!(app.keys(), [head.as_str(), HELD, NATIVE]);
    // The hidden row wears `◌ hidden`; the other does not.
    let s = shown(&app);
    let line = |t: &str| s.lines().find(|l| l.contains(t)).unwrap_or_default().to_string();
    assert!(line("fix the login").contains("◌ hidden"), "{s}");
    assert!(!line("shop-sh").contains("◌ hidden"), "{s}");
    // Moving works with hidden ones shown, and the hidden row keeps its place.
    on(&mut app, HELD);
    shift(&mut app, KeyCode::Down);
    assert_eq!(app.keys(), [head.as_str(), NATIVE, HELD], "{}", shown(&app));
    let order = Config::load(&f.home).unwrap().list.order;
    assert_eq!(order[0].rows, [NATIVE, HELD]);
    // And so with hidden ones off again: only the shown row is left.
    ctrl(&mut app, 'o');
    assert_eq!(app.keys(), [head.as_str(), HELD]);
}

/// Whether a choice is being asked in the footer.
fn asking(app: &App) -> bool {
    matches!(app.mode, super::app::Mode::Ask(_))
}

/// What `^x` twice asks of any session still going, whoever's it is.
const ASKS: &str = "s stop · keeps the session   r remove · deletes the session for good   esc cancel";
/// And of one that has ended.
const ASKS_ENDED: &str = "r remove · deletes the session for good   esc cancel";

const CLAUDE_ASKS: &str =
    "s stop · keeps the session   r remove · deletes the session for good   esc cancel   — fix the login · claude";

/// The screen wide enough for a whole choice in the footer.
fn wide(app: &App) -> String {
    screen(app, 160, 30).join("\n")
}

#[tokio::test]
async fn ctrl_x_twice_asks_how_to_close_a_claude_session_and_s_stops_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    assert!(
        shown(&app).contains("ctrl+x twice to stop or remove"),
        "the hint says what ctrl+x does here: {}",
        shown(&app)
    );
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ctrl+x again to stop or remove fix the login"));
    ctrl(&mut app, 'x');
    assert!(asking(&app));
    assert!(wide(&app).contains(CLAUDE_ASKS), "{}", wide(&app));
    assert!(shown(&app).contains("esc cancel"), "at 100 columns the keys still fit");
    assert!(app.work.is_empty() && ran_in(&f.repo).is_empty(), "nothing done yet");
    // A key that is no answer leaves the choice standing.
    press(&mut app, KeyCode::Char('q'));
    ctrl(&mut app, 'r');
    assert!(asking(&app) && app.work.is_empty());
    press(&mut app, KeyCode::Char('s'));
    assert!(!asking(&app));
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["stop", "ab12cd34"], "claude stop <id>, in its folder");
    assert!(shown(&app).contains("stopped fix the login"));
    assert!(app.ask_native, "the list asks claude again at once");
    assert!(app.keys().iter().any(|k| k == NATIVE), "still on the list");
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty(), "not hidden");
}

#[tokio::test]
async fn ctrl_x_twice_then_enter_stops_as_ever_and_esc_does_nothing() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Esc);
    assert!(!asking(&app));
    assert!(app.work.is_empty() && ran_in(&f.repo).is_empty(), "esc: nothing");
    assert!(!shown(&app).contains("esc cancel"));
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["stop", "ab12cd34"], "enter: the first, the stop");
}

#[tokio::test]
async fn r_removes_a_claude_session_and_its_row_goes_with_what_was_kept_for_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 't');
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["rm", "ab12cd34"], "claude rm <id>, in its folder");
    let s = shown(&app);
    assert!(s.contains("removed fix the login"), "{s}");
    assert!(app.keys().iter().all(|k| k != NATIVE), "off the list at once: {s}");
    assert!(app.ask_native, "and claude asked again");
    assert!(
        Config::load(&f.home).unwrap().list.pins.is_empty(),
        "its pin goes with it"
    );
}

#[tokio::test]
async fn an_ended_claude_session_is_offered_only_its_remove() {
    let f = fixture();
    let mut app = list(&f).await;
    let mut done = waiting(&f.repo);
    done.state = NativeState {
        words: "completed".into(),
        state: RunState::Finished,
    };
    app.set_native(vec![done]);
    on(&mut app, NATIVE);
    assert!(shown(&app).contains("ctrl+x twice to remove"), "{}", shown(&app));
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ctrl+x again to remove fix the login"));
    ctrl(&mut app, 'x');
    let s = wide(&app);
    assert!(s.contains(&format!("{ASKS_ENDED}   — fix the login · claude")), "{s}");
    assert!(!s.contains("s stop"), "nothing to stop");
    press(&mut app, KeyCode::Char('s'));
    assert!(asking(&app) && app.work.is_empty(), "s is no answer here");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["rm", "ab12cd34"], "enter: the remove");
    assert!(app.keys().iter().all(|k| k != NATIVE));
}

#[tokio::test]
async fn a_claude_rm_that_refuses_keeps_the_row_and_says_what_claude_said() {
    let f = fixture();
    let mut app = list(&f).await;
    let refusal =
        "ab12cd34 has 2 unpushed commits in worktree w-1\nrerun with --discard-unpushed 9f1e@w-1 to discard them";
    std::fs::write(f.repo.join("refuse-rm"), format!("{refusal}\n")).unwrap();
    on(&mut app, NATIVE);
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["rm", "ab12cd34"], "no --discard-unpushed, no --force");
    assert_eq!(
        app.flashing(),
        Some(format!("remove fix the login: {refusal}").as_str()),
        "as claude said it"
    );
    assert!(app.keys().iter().any(|k| k == NATIVE), "the row stays");
    assert_eq!(app.sel.as_deref(), Some(NATIVE));
}

#[tokio::test]
async fn a_harnesss_own_session_is_hidden_by_slash_hide_and_ctrl_x_twice_brings_it_back() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    run(&mut app, "/hide");
    assert!(app.keys().iter().all(|k| k != NATIVE));
    assert!(shown(&app).contains("hid fix the login"));
    let hidden = Config::load(&f.home).unwrap().list.hidden;
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].key, NATIVE);
    assert_eq!(app.sel.as_deref(), Some(HELD), "the cursor on the line before");
    // With hidden on it is there, and ^x twice brings it back, asking nothing.
    ctrl(&mut app, 'o');
    on(&mut app, NATIVE);
    assert!(shown(&app).contains("ctrl+x twice to bring back"), "{}", shown(&app));
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ctrl+x again to bring back fix the login"));
    ctrl(&mut app, 'x');
    assert!(!asking(&app));
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
    assert!(shown(&app).contains("press ctrl+x again"), "armed afresh, not fired");
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty());
}

#[tokio::test]
async fn removing_a_shell_keeps_the_cursor_in_its_project_not_the_top_of_the_list() {
    let f = fixture();
    // A project above shop's, so the top of the list is somewhere else.
    let art = beside(&f, "art");
    let mut app = list(&f).await;
    app.set_native(vec![
        waiting(&f.repo),
        NativeRow {
            id: "ef56ab78".into(),
            name: "paint the fence".into(),
            ..waiting(&art)
        },
    ]);
    app.reload();
    let shop = format!("g:{}", f.repo.display());
    let art_head = format!("g:{}", art.display());
    assert_eq!(
        app.keys(),
        [art_head.as_str(), "n:claude:ef56ab78", shop.as_str(), HELD, NATIVE]
    );
    on(&mut app, HELD);
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    press(&mut app, KeyCode::Char('r'));
    // The list read again before the remove has said it is done (the file
    // watch saw the folder go): the cursor stays in shop all the same.
    std::fs::remove_dir_all(f.home.session_dir("shop-sh")).ok();
    app.reload();
    assert_eq!(app.sel.as_deref(), Some(NATIVE), "{}", shown(&app));
    app.finish().await;
    assert!(app.keys().iter().all(|k| k != HELD));
    assert_eq!(app.sel.as_deref(), Some(NATIVE), "the next row of its project");
}

#[tokio::test]
async fn ctrl_x_twice_then_r_removes_an_ended_held_session_and_what_was_kept_for_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 't');
    ctrl(&mut app, 'g');
    assert_eq!(Config::load(&f.home).unwrap().list.colors.len(), 1);
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ctrl+x again to remove shop-sh"));
    ctrl(&mut app, 'x');
    assert!(asking(&app), "asked, as any other");
    let s = wide(&app);
    assert!(s.contains(&format!("{ASKS_ENDED}   — shop-sh · sh")), "{s}");
    assert!(f.home.session_dir("shop-sh").exists(), "nothing done yet");
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    assert!(!f.home.session_dir("shop-sh").exists());
    let s = shown(&app);
    assert!(s.contains("removed shop-sh"), "{s}");
    assert!(app.keys().iter().all(|k| k != HELD), "{s}");
    assert!(
        Config::load(&f.home).unwrap().list.pins.is_empty(),
        "its pin goes with it"
    );
    assert!(Config::load(&f.home).unwrap().list.colors.is_empty(), "and its colour");
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
    let super::app::To::Argv { argv } = &hand.to else {
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
    assert!(row_of(&shown(&app), "❯ claude fix").contains("agent · claude"));
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

/// What the tests' stand-in Codex daemon was sent, by every test so far.
fn codex_sent() -> Vec<serde_json::Value> {
    // Shared by the tests running at once: a line another test's stand-in is
    // still writing is not read yet.
    std::fs::read_to_string(crate::sources::codex_stand_in().with_extension("log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Whether the stand-in Codex was sent a message `pred` matches, within a
/// few seconds: its log is written as it answers, not before a start returns.
async fn codex_was_sent(pred: impl Fn(&serde_json::Value) -> bool) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if codex_sent().iter().any(&pred) {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn the_line_starts_codex_on_its_daemon_and_enter_resumes_the_thread() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    typed(&mut app, "codex fix the login");
    assert!(row_of(&shown(&app), "❯ codex fix").contains("agent · codex"));
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    let s = shown(&app);
    let id = row_of(&s, "started codex")
        .split_whitespace()
        .skip_while(|w| *w != "codex")
        .nth(1)
        .unwrap()
        .to_string();
    assert!(s.contains(&format!("started codex {id} in its background")), "{s}");
    let sent = codex_sent();
    let start = sent
        .iter()
        .position(|m| m["method"] == "thread/start" && m["params"]["cwd"] == f.repo.to_str().unwrap())
        .expect("a thread in the row's folder");
    assert_eq!(
        sent[start..]
            .iter()
            .find(|m| m["method"] == "turn/start" && m["params"]["threadId"] == id.as_str())
            .map(|m| m["params"]["input"][0]["text"].clone()),
        Some(serde_json::json!("fix the login")),
        "its first turn on the prompt"
    );
    assert!(
        sessions::list(&f.home).iter().all(|h| h.meta.harness != "codex"),
        "not one Relay holds"
    );
    let key = format!("n:codex:{id}");
    assert_eq!(app.awaiting.as_deref(), Some(key.as_str()));
    // Listed: the cursor goes to it, and enter resumes it.
    let mut thread = waiting(&f.repo);
    thread.harness = "codex".into();
    thread.id = id.clone();
    thread.session_id = id.clone();
    app.set_native(vec![waiting(&f.repo), thread]);
    assert_eq!(app.sel.as_deref(), Some(key.as_str()));
    press(&mut app, KeyCode::Enter);
    let hand = app.hand.take().expect("handed over");
    let super::app::To::Argv { argv, .. } = &hand.to else {
        panic!("{:?}", hand.to);
    };
    assert_eq!(argv[1..], ["resume".to_string(), id.clone()], "codex resume <id>");
    // ctrl+x twice asks as for any other; s: its running turn interrupted on
    // the daemon.
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains("press ctrl+x again to stop or remove"));
    ctrl(&mut app, 'x');
    let s = wide(&app);
    assert!(s.contains(&format!("{ASKS}   — fix the login · codex")), "{s}");
    press(&mut app, KeyCode::Char('s'));
    app.finish().await;
    assert!(
        codex_sent()
            .iter()
            .any(|m| m["method"] == "turn/interrupt" && m["params"]["threadId"] == id.as_str()),
        "interrupted"
    );
}

/// Where in what the stand-in Codex was sent the first `method` call on the
/// thread `id` is.
fn codex_call(method: &str, id: &str) -> Option<usize> {
    codex_sent()
        .iter()
        .position(|m| m["method"] == method && m["params"]["threadId"] == id)
}

#[tokio::test]
async fn a_codex_thread_is_deleted_by_r_its_turn_interrupted_first_and_one_at_rest_as_it_is() {
    let f = fixture();
    let mut app = list(&f).await;
    // A thread on the daemon with its first turn running.
    on(&mut app, NATIVE);
    typed(&mut app, "codex fix the login");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    let live = row_of(&shown(&app), "started codex")
        .split_whitespace()
        .skip_while(|w| *w != "codex")
        .nth(1)
        .unwrap()
        .to_string();
    let live = live.as_str();
    let thread = |id: &str, words: &str, state| {
        let mut t = waiting(&f.repo);
        t.harness = "codex".into();
        t.id = id.into();
        t.session_id = id.into();
        t.name = format!("thread {id}");
        t.state = NativeState {
            words: words.into(),
            state,
        };
        t
    };
    let resting = "019f0000-aaaa-7000-8000-000000000002";
    app.set_native(vec![
        thread(live, "working", RunState::Working),
        thread(resting, "resting", RunState::Idle),
    ]);
    on(&mut app, &format!("n:codex:{live}"));
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    assert!(wide(&app).contains(&format!("{ASKS}   — thread {live} · codex")));
    press(&mut app, KeyCode::Char('r'));
    app.finish().await;
    let (interrupted, deleted) = (codex_call("turn/interrupt", live), codex_call("thread/delete", live));
    assert!(
        interrupted.is_some() && interrupted < deleted,
        "turn/interrupt, then thread/delete: {interrupted:?} {deleted:?}"
    );
    assert!(
        shown(&app).contains(&format!("removed thread {live}")),
        "{}",
        shown(&app)
    );
    assert!(
        app.keys().iter().all(|k| *k != format!("n:codex:{live}")),
        "off the list"
    );
    // At rest: nothing to stop, the remove the one choice, and enter takes it.
    on(&mut app, &format!("n:codex:{resting}"));
    ctrl(&mut app, 'x');
    assert!(shown(&app).contains(&format!("press ctrl+x again to remove thread {resting}")));
    ctrl(&mut app, 'x');
    let s = wide(&app);
    assert!(s.contains(&format!("{ASKS_ENDED}   — thread {resting} · codex")), "{s}");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert!(codex_call("thread/delete", resting).is_some(), "thread/delete");
    assert_eq!(codex_call("turn/interrupt", resting), None, "nothing to interrupt");
}

#[tokio::test]
async fn a_bare_codex_runs_here_in_the_folder() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    typed(&mut app, "codex");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    let hand = app.hand.take().expect("handed the terminal");
    assert_eq!(
        hand.to,
        super::app::To::Here {
            argv: vec![crate::sources::codex_stand_in().to_string_lossy().into_owned()],
            cwd: f.repo.clone(),
        }
    );
    assert!(
        sessions::list(&f.home).iter().all(|h| h.meta.harness != "codex"),
        "not one Relay holds"
    );
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
    let super::app::To::Argv { argv, .. } = &hand.to else {
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

/// A conversation open in another terminal, its hooks' and Claude's.
const IN_TERMINAL: &str = "73059956-4a78-496a-bdfa-2f5b4ba1b747";

/// What Claude lists of `IN_TERMINAL`, as `claude agents --json --all`
/// prints it, in `cwd`.
fn in_terminal(cwd: &Path) -> Vec<NativeRow> {
    let text = format!(
        r#"[{{"pid":53845,"cwd":"{}","kind":"interactive","startedAt":1794515122530,"sessionId":"{IN_TERMINAL}","name":"os-dispatch-ea","status":"idle"}}]"#,
        cwd.display()
    );
    rl_detect::claude_native::rows(&text, None).unwrap()
}

#[tokio::test]
async fn a_claude_open_in_another_terminal_goes_by_its_own_name_on_one_row_kept_by_its_conversation() {
    use crate::store::state::Said;
    let f = fixture();
    hooked(&f, IN_TERMINAL, Said::Waiting);
    let mut app = list(&f).await;
    let key = format!("o:claude:{IN_TERMINAL}");
    on(&mut app, &key);
    assert!(shown(&app).contains("session 73059956"), "its hooks alone: no name");
    let mut listed = in_terminal(&f.repo);
    listed.push(waiting(&f.repo));
    app.set_native(listed);
    let keys = app.keys();
    assert_eq!(
        keys.iter().filter(|k| k.contains(IN_TERMINAL)).count(),
        1,
        "one row: {keys:?}"
    );
    on(&mut app, &key);
    let s = wide(&app);
    assert_eq!(s.matches("os-dispatch-ea").count(), 1, "{s}");
    assert!(!s.contains("session 73059956"), "{s}");
    assert!(
        row_of(&s, "os-dispatch-ea").contains("needs you"),
        "its hooks know it waits on you: {s}"
    );
    assert!(s.contains("ctrl+x twice to hide · it runs in another terminal"), "{s}");
    run(&mut app, "/rm");
    assert!(
        wide(&app).contains("/rm can't remove os-dispatch-ea · it runs in another terminal (/hide hides it)"),
        "{}",
        wide(&app)
    );
    assert!(ran_in(&f.repo).is_empty(), "claude was not asked anything");
    // At rest there, it is still that terminal's: enter carries nothing on.
    hooked(&f, IN_TERMINAL, Said::Idle);
    let mut listed = in_terminal(&f.repo);
    listed.push(waiting(&f.repo));
    app.set_native(listed);
    on(&mut app, &key);
    press(&mut app, KeyCode::Enter);
    assert!(app.work.is_empty() && app.hand.is_none(), "nothing begun");
    assert!(
        wide(&app).contains("os-dispatch-ea runs in another terminal · switch to it there"),
        "{}",
        wide(&app)
    );
    assert!(ran_in(&f.repo).is_empty(), "claude was not asked anything");
    // ^x twice hides it, by the key it always had.
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'x');
    let hidden = Config::load(&f.home).unwrap().list.hidden;
    assert_eq!(
        hidden.iter().map(|h| h.key.as_str()).collect::<Vec<_>>(),
        [key.as_str()]
    );
    assert!(app.keys().iter().all(|k| *k != key));
    // Claude alone lists it (its hooks' word gone): still that one row, hidden.
    std::fs::remove_file(crate::store::state::path(&f.home, "claude", IN_TERMINAL).unwrap()).unwrap();
    let mut listed = in_terminal(&f.repo);
    listed.push(waiting(&f.repo));
    app.set_native(listed);
    assert!(app.keys().iter().all(|k| *k != key), "a hide kept before still applies");
    ctrl(&mut app, 'o');
    on(&mut app, &key);
    let s = wide(&app);
    assert!(row_of(&s, "os-dispatch-ea").contains("idle"), "as Claude says: {s}");
    assert!(s.contains("ctrl+x twice to bring back"), "{s}");
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
            to: super::app::To::Argv {
                argv: vec!["claude".into(), "attach".into(), "ab12cd34".into()],
            },
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
    let head = at(&rows, "✦ agents").expect("the toggles are on screen, by name");
    assert!(head < 6, "in the header, beside the mark: {}", rows.join("\n"));
    assert!(rows[head].contains("❯ shells") && rows[head].contains("◌ hidden") && rows[head].contains("by project"));
    assert!(
        !rows[head].contains("ctrl+"),
        "the keys are in the help grid, not the header"
    );
    assert!(at(&rows, "relay").unwrap() < head && at(&rows, "1 needs you").unwrap() < head);
    for r in &rows[6..] {
        assert!(
            !r.contains("✦ agents") && !r.contains("by project"),
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
    assert!(rows[..6].iter().any(|r| r.contains("by state")), "{}", rows.join("\n"));
    // On a short screen the header is one line, and keeps them: with their
    // names at 80, the icons alone where the headline would lose its room.
    let rows = screen(&app, 80, 20);
    assert!(
        rows[0].contains("✦ agents") && rows[0].contains("by state"),
        "{}",
        rows[0]
    );
    let rows = screen(&app, 60, 20);
    assert!(
        rows[0].contains("✦  ❯") && rows[0].contains("by state") && !rows[0].contains("agents"),
        "{}",
        rows[0]
    );
}

#[tokio::test]
async fn the_search_takes_the_command_lines_place_while_it_is_open() {
    let f = fixture();
    let mut app = list(&f).await;
    assert!(line_row(&app).contains(" ❯ "), "{}", line_row(&app));
    ctrl(&mut app, 'f');
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
    assert!(line_row(&app).contains(" ❯ "), "{}", line_row(&app));
    assert!(!line_row(&app).contains("shop ❯"), "no folder before the prompt");
    typed(&mut app, "make test");
    assert_eq!(app.cmd.line.text(), "make test");
    let row = line_row(&app);
    assert!(row.contains(" ❯ make test") && !row.contains("shop ❯"), "{row}");
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
async fn a_slash_starts_a_command_and_only_ctrl_f_searches() {
    let f = fixture();
    let mut app = list(&f).await;
    assert!(line_row(&app).contains("/ for commands"), "{}", line_row(&app));
    press(&mut app, KeyCode::Char('/'));
    assert!(app.search.is_none(), "/ no longer searches");
    assert_eq!(app.cmd.line.text(), "/");
    typed(&mut app, "pin");
    assert!(line_row(&app).contains("command · pin"), "{}", line_row(&app));
    assert!(shown(&app).contains("enter runs it on the row"), "the footer says how");
    press(&mut app, KeyCode::Esc);
    assert!(app.cmd.is_empty());
    ctrl(&mut app, 'f');
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

/// The line typed and run with `enter`.
fn run(app: &mut App, line: &str) {
    typed(app, line);
    press(app, KeyCode::Enter);
}

#[tokio::test]
async fn slash_colour_sets_your_colour_and_a_header_has_none_to_set() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    run(&mut app, "/colour pink");
    assert!(app.cmd.is_empty(), "cleared once run");
    let colors = Config::load(&f.home).unwrap().list.colors;
    assert_eq!(colors.get(HELD).map(String::as_str), Some("pink"));
    assert!(shown(&app).contains("colour: pink"), "{}", shown(&app));
    run(&mut app, "/color #102060");
    assert_eq!(
        Config::load(&f.home).unwrap().list.colors.get(HELD).map(String::as_str),
        Some("#102060"),
        "the alias, and a hex"
    );
    run(&mut app, "/colour none");
    assert!(Config::load(&f.home).unwrap().list.colors.is_empty());
    // Not a colour: said, and the line kept to mend.
    run(&mut app, "/colour chartreuse");
    assert!(shown(&app).contains("not a colour: chartreuse"), "{}", shown(&app));
    assert_eq!(app.cmd.line.text(), "/colour chartreuse");
    press(&mut app, KeyCode::Esc);
    // Claude's colour wins, as with ^g.
    app.set_native(vec![{
        let mut p = waiting(&f.repo);
        p.color = Some("pink".into());
        p
    }]);
    on(&mut app, NATIVE);
    run(&mut app, "/colour blue");
    assert!(
        shown(&app).contains("pink's colour wins · yours: blue"),
        "{}",
        shown(&app)
    );
    // A project header has none.
    let head = format!("g:{}", f.repo.display());
    on(&mut app, &head);
    run(&mut app, "/colour red");
    assert!(shown(&app).contains("/colour colours a session"), "{}", shown(&app));
    assert!(!Config::load(&f.home).unwrap().list.colors.contains_key(&head));
}

#[tokio::test]
async fn slash_rename_names_the_row_and_an_empty_one_goes_back() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    run(&mut app, "/rename build box");
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(HELD).map(String::as_str), Some("build box"));
    assert!(row_of(&shown(&app), "build box").contains("done"));
    // With no name, it is typed where it stands, as ctrl+r: empty goes back.
    run(&mut app, "/rename");
    assert!(matches!(app.mode, super::app::Mode::Rename(_)));
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Enter);
    assert!(Config::load(&f.home).unwrap().list.names.is_empty());
    assert!(shown(&app).contains("back to shop-sh"));
}

/// The name being typed, and what of it is selected.
fn renaming(app: &App) -> (String, Option<std::ops::Range<usize>>) {
    match &app.mode {
        super::app::Mode::Rename(r) => (r.line.text().to_string(), r.line.selection()),
        _ => panic!("no name being typed"),
    }
}

#[tokio::test]
async fn ctrl_r_starts_with_the_name_selected_and_what_is_typed_replaces_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    assert_eq!(renaming(&app), ("shop-sh".into(), Some(0..7)), "selected whole");
    let s = shown(&app);
    assert!(row_of(&s, "shop-sh").contains("shop-sh"), "{s}");
    // Drawn as a selection, in the theme's colours.
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    t.draw(|fr| app.draw(fr)).unwrap();
    let buf = t.backend().buffer().clone();
    let y = (0..30)
        .find(|y| {
            (0..100)
                .map(|x| buf[(x, *y)].symbol())
                .collect::<String>()
                .contains("shop-sh")
        })
        .unwrap();
    let x = (0..100)
        .find(|x| buf[(*x, y)].symbol() == "s" && buf[(*x + 1, y)].symbol() == "h")
        .unwrap();
    assert_eq!(buf[(x, y)].bg, super::theme::selection().bg.unwrap());
    // The first key replaces it.
    typed(&mut app, "box");
    assert_eq!(renaming(&app), ("box".into(), None));
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(HELD).map(String::as_str), Some("box"));
}

#[tokio::test]
async fn backspace_on_the_selected_name_clears_it_and_enter_goes_back_to_its_own() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    run(&mut app, "/rename box");
    ctrl(&mut app, 'r');
    press(&mut app, KeyCode::Backspace);
    assert_eq!(renaming(&app), (String::new(), None));
    press(&mut app, KeyCode::Enter);
    assert!(Config::load(&f.home).unwrap().list.names.is_empty());
    assert!(shown(&app).contains("back to shop-sh"));
}

#[tokio::test]
async fn an_arrow_lets_the_selection_go_and_typing_then_edits_the_name() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    press(&mut app, KeyCode::Left);
    assert_eq!(renaming(&app), ("shop-sh".into(), None));
    typed(&mut app, "my ");
    press(&mut app, KeyCode::End);
    typed(&mut app, "!");
    assert_eq!(renaming(&app).0, "my shop-sh!");
    ctrl(&mut app, 'a');
    typed(&mut app, ">");
    assert_eq!(
        renaming(&app).0,
        ">my shop-sh!",
        "ctrl+a to the start, not the agents' filter"
    );
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(HELD).map(String::as_str), Some(">my shop-sh!"));
}

#[tokio::test]
async fn enter_on_the_name_untouched_keeps_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, super::app::Mode::List));
    assert!(Config::load(&f.home).unwrap().list.names.is_empty(), "nothing changed");
    assert!(!shown(&app).contains("back to"), "nothing said");
}

#[tokio::test]
async fn a_paste_replaces_the_selected_name() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    ctrl(&mut app, 'r');
    app.paste("build\nbox");
    assert_eq!(renaming(&app), ("build box".into(), None));
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(HELD).map(String::as_str), Some("build box"));
}

#[tokio::test]
async fn a_harnesss_session_is_named_in_relay_with_its_own_name_dim_beside_and_both_searched() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'r');
    assert!(matches!(app.mode, super::app::Mode::Rename(_)));
    ctrl(&mut app, 'u');
    typed(&mut app, "login box");
    press(&mut app, KeyCode::Enter);
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(NATIVE).map(String::as_str), Some("login box"));
    assert!(
        shown(&app).contains("named in relay · claude takes it at its next start"),
        "{}",
        shown(&app)
    );
    let row = row_of(&shown(&app), "login box").to_string();
    assert!(row.contains("login box  (fix the login)"), "{row}");
    // Found by either name.
    for word in ["box", "fix the"] {
        ctrl(&mut app, 'f');
        typed(&mut app, word);
        assert!(shown(&app).contains("login box"), "{word}");
        assert_eq!(app.matches(), [NATIVE], "{word}");
        press(&mut app, KeyCode::Esc);
        assert!(app.search.is_none());
    }
    // A narrow name cell keeps the name and cuts the dim part first.
    let narrow = screen(&app, 60, 30).join("\n");
    assert!(row_of(&narrow, "login box").contains("login box"), "{narrow}");
    // Empty restores the original.
    run(&mut app, "/rename");
    press(&mut app, KeyCode::Delete);
    press(&mut app, KeyCode::Enter);
    assert!(Config::load(&f.home).unwrap().list.names.is_empty());
    assert!(shown(&app).contains("back to fix the login"));
    assert!(!shown(&app).contains("(fix the login)"));
}

#[tokio::test]
async fn a_claude_name_stays_in_relay_till_claude_takes_it_at_its_next_start() {
    let f = fixture();
    let mut app = list(&f).await;
    let mut idle = waiting(&f.repo);
    idle.state = NativeState {
        words: "idle".into(),
        state: RunState::Idle,
    };
    app.set_native(vec![idle.clone()]);
    on(&mut app, NATIVE);
    run(&mut app, "/rename login box");
    app.finish().await;
    assert!(app.work.is_empty(), "nothing run: Claude has no rename from outside");
    assert!(
        shown(&app).contains("named in relay · claude takes it at its next start"),
        "{}",
        shown(&app)
    );
    assert!(!f.repo.join("typed").exists(), "nothing typed into it");
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get(NATIVE).map(String::as_str), Some("login box"));
    // Carried on with it (`-n`), Claude calls it so: Relay's goes.
    let mut renamed = idle;
    renamed.name = "login box".into();
    app.set_native(vec![renamed]);
    assert!(Config::load(&f.home).unwrap().list.names.is_empty());
    let row = row_of(&shown(&app), "login box").to_string();
    assert!(!row.contains("(fix the login)"), "{row}");
}

#[tokio::test]
async fn a_codex_thread_is_renamed_on_its_daemon_and_keeps_no_relay_name() {
    let f = fixture();
    let mut app = list(&f).await;
    let codex = crate::sources::harness("codex").unwrap();
    let start = rl_detect::harness::Start {
        cwd: f.repo.clone(),
        prompt: Some("fix it".into()),
        ..Default::default()
    };
    let started = codex.start(&start).unwrap();
    let rl_detect::harness::Started::Background(id) = started else {
        panic!("{started:?}");
    };
    let mut thread = waiting(&f.repo);
    thread.harness = "codex".into();
    thread.id = id.clone();
    thread.session_id = id.clone();
    thread.name = "fix it".into();
    app.set_native(vec![thread.clone()]);
    let key = format!("n:codex:{id}");
    on(&mut app, &key);
    run(&mut app, "/rename shop api");
    app.finish().await;
    let set = |name: &str| {
        codex_sent().iter().any(|m| {
            m["method"] == "thread/name/set" && m["params"] == serde_json::json!({"threadId": id, "name": name})
        })
    };
    assert!(set("shop api"), "thread/name/set on the daemon");
    assert!(
        shown(&app).contains("renamed to shop api · in codex too"),
        "{}",
        shown(&app)
    );
    assert!(
        Config::load(&f.home).unwrap().list.names.is_empty(),
        "codex's name is the name"
    );
    assert!(row_of(&shown(&app), "shop api").contains("shop api"));
    // Cleared: cleared on the daemon too.
    thread.name = "shop api".into();
    app.set_native(vec![thread.clone()]);
    on(&mut app, &key);
    ctrl(&mut app, 'r');
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert!(set(""), "its name set back to none");
    assert!(shown(&app).contains("cleared its name in codex"), "{}", shown(&app));
    // One the daemon does not know: the name stays Relay's, and why.
    thread.id = "019f0000-gone".into();
    app.set_native(vec![thread]);
    on(&mut app, "n:codex:019f0000-gone");
    run(&mut app, "/rename gone box");
    app.finish().await;
    assert!(
        shown(&app).contains(
            "named in relay · codex didn't take it: `codex app-server proxy`: thread/name/set: thread not found"
        ),
        "{}",
        shown(&app)
    );
    let names = Config::load(&f.home).unwrap().list.names;
    assert_eq!(names.get("n:codex:019f0000-gone").map(String::as_str), Some("gone box"));
}

#[tokio::test]
async fn slash_pin_hide_agents_and_group_do_what_their_keys_do() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    run(&mut app, "/pin");
    assert_eq!(Config::load(&f.home).unwrap().list.pins, [NATIVE]);
    assert_eq!(app.keys()[0], NATIVE);
    run(&mut app, "/pin");
    assert!(Config::load(&f.home).unwrap().list.pins.is_empty());
    run(&mut app, "/hide");
    assert_eq!(Config::load(&f.home).unwrap().list.hidden[0].key, NATIVE);
    assert!(app.keys().iter().all(|k| k != NATIVE));
    run(&mut app, "/hidden");
    assert!(Config::load(&f.home).unwrap().list.show_hidden);
    on(&mut app, NATIVE);
    run(&mut app, "/hide");
    assert!(Config::load(&f.home).unwrap().list.hidden.is_empty(), "brought back");
    run(&mut app, "/agents");
    assert!(!Config::load(&f.home).unwrap().list.show_agents);
    assert!(!shown(&app).contains("fix the login"));
    run(&mut app, "/agents");
    assert!(Config::load(&f.home).unwrap().list.show_agents);
    run(&mut app, "/shells");
    assert!(!Config::load(&f.home).unwrap().list.show_shells);
    run(&mut app, "/group");
    assert_eq!(Config::load(&f.home).unwrap().list.grouping, Grouping::State);
    run(&mut app, "/group");
    assert_eq!(Config::load(&f.home).unwrap().list.grouping, Grouping::Project);
    run(&mut app, "/pin now");
    assert!(shown(&app).contains("/pin takes nothing after it"));
    press(&mut app, KeyCode::Esc);
    run(&mut app, "/quit");
    assert!(app.quit, "at once");
}

#[tokio::test]
async fn slash_rm_asks_to_remove_any_session_and_refuses_what_it_cannot() {
    let f = fixture();
    let mut app = list(&f).await;
    // One of Claude's own: asked, the remove the only choice, live or not.
    on(&mut app, NATIVE);
    run(&mut app, "/rm");
    let s = wide(&app);
    assert!(s.contains(&format!("{ASKS_ENDED}   — fix the login · claude")), "{s}");
    assert!(!s.contains("s stop"), "{s}");
    assert!(app.work.is_empty(), "nothing done");
    press(&mut app, KeyCode::Esc);
    assert!(ran_in(&f.repo).is_empty(), "claude was not asked anything");
    let head = format!("g:{}", f.repo.display());
    on(&mut app, &head);
    run(&mut app, "/rm");
    assert!(
        shown(&app).contains("/rm removes the session under the cursor"),
        "{}",
        shown(&app)
    );
    on(&mut app, HELD);
    run(&mut app, "/stop");
    assert!(
        shown(&app).contains("shop-sh has nothing running to stop · /rm removes it"),
        "{}",
        shown(&app)
    );
    // One Relay holds: the same question.
    run(&mut app, "/rm");
    assert!(wide(&app).contains(&format!("{ASKS_ENDED}   — shop-sh · sh")));
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert!(!f.home.session_dir("shop-sh").exists());
    assert!(shown(&app).contains("removed shop-sh"));
    // And enter confirms the remove of Claude's own.
    on(&mut app, NATIVE);
    run(&mut app, "/rm");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["rm", "ab12cd34"]);
    assert!(app.keys().iter().all(|k| k != NATIVE));
}

#[tokio::test]
async fn slash_stop_stops_claudes_own_at_once_and_says_when_nothing_runs() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    run(&mut app, "/stop");
    assert!(!asking(&app), "no question");
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["stop", "ab12cd34"]);
    let mut done = waiting(&f.repo);
    done.state = NativeState {
        words: "completed".into(),
        state: RunState::Finished,
    };
    app.set_native(vec![done]);
    on(&mut app, NATIVE);
    run(&mut app, "/stop");
    assert!(
        shown(&app).contains("fix the login has nothing running to stop · /rm removes it"),
        "{}",
        shown(&app)
    );
}

#[tokio::test]
async fn an_unknown_command_is_said_and_kept_on_the_line() {
    let f = fixture();
    let mut app = list(&f).await;
    typed(&mut app, "/col");
    assert!(!line_row(&app).contains("unknown command"), "the start of one");
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "/nope");
    assert!(line_row(&app).contains("unknown command"), "{}", line_row(&app));
    press(&mut app, KeyCode::Enter);
    assert!(shown(&app).contains("unknown command /nope"), "{}", shown(&app));
    assert_eq!(app.cmd.line.text(), "/nope");
    assert!(app.work.is_empty(), "no shell started");
}

#[tokio::test]
async fn tab_completes_a_command_then_its_colour_and_ghosts_the_rows_name() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    typed(&mut app, "/col");
    assert_eq!(app.cmd.ghost.as_deref(), Some("our"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/colour ");
    typed(&mut app, "p");
    assert_eq!(app.cmd.ghost.as_deref(), Some("ink"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/colour pink");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/colour purple", "tab again goes round");
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "/rename ");
    assert_eq!(app.cmd.ghost.as_deref(), Some("shop-sh"), "the name it has");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/rename shop-sh ");
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
    assert!(
        line_row(&app).contains(" ❯ codex") && !line_row(&app).contains("shop ❯"),
        "the ghost: {}",
        line_row(&app)
    );
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
    let pink_bg = crate::tui::theme::picked("pink").unwrap();
    let f = fixture();
    let mut app = list(&f).await;
    let mut pink = waiting(&f.repo);
    pink.color = Some("pink".into());
    app.set_native(vec![pink]);
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].bg, pink_bg, "the name sits on a pink swatch");
    assert_eq!(
        buf[(x, y)].fg,
        crate::tui::theme::swatch(pink_bg).fg.unwrap(),
        "in dark text"
    );
    assert_eq!(buf[(x + 12, y)].bg, pink_bg, "to the name's last letter");
    assert_ne!(buf[(1, y)].bg, pink_bg, "the icon is not on it");
    assert_ne!(buf[(1, y)].fg, pink_bg, "and keeps its own colour");
    // Without one, the name is the foreground's, as ever.
    app.set_native(vec![waiting(&f.repo)]);
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].fg, Color::Reset);
    assert_eq!(buf[(x, y)].bg, Color::Reset);
    app.set_native(vec![{
        let mut p = waiting(&f.repo);
        p.color = Some("pink".into());
        p
    }]);
    on(&mut app, NATIVE);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].bg, pink_bg, "under the cursor the swatch stays on the name");
    assert_eq!(
        buf[(1, y)].bg,
        crate::tui::theme::selected().bg.unwrap(),
        "and the cursor's band is behind the rest"
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
    assert_eq!(buf[(x + 8, y)].bg, pink_bg, "over the swatch");
}

#[tokio::test]
async fn ctrl_g_cycles_a_rows_own_colour_through_the_palette_and_keeps_it() {
    use crate::tui::theme;
    let f = fixture();
    let mut app = list(&f).await;
    for key in [HELD, NATIVE] {
        on(&mut app, key);
        let mut seen = vec![];
        for _ in 0..theme::PALETTE.len() {
            ctrl(&mut app, 'g');
            let c = Config::load(&f.home).unwrap().list.colors.get(key).cloned();
            seen.push(c.expect("set"));
            assert!(shown(&app).contains(&format!("colour: {}", seen.last().unwrap())));
            assert_eq!(app.sel.as_deref(), Some(key), "the cursor stays");
        }
        assert_eq!(seen, theme::PALETTE);
        ctrl(&mut app, 'g');
        assert!(!Config::load(&f.home).unwrap().list.colors.contains_key(key));
        assert!(shown(&app).contains("colour: none"));
    }
}

#[tokio::test]
async fn your_colour_is_the_names_swatch_and_a_harnesss_own_wins_with_a_word_about_it() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 'g');
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].bg, crate::tui::theme::picked("red").unwrap(), "yours");
    // Claude sets pink: it shows, and ^g still moves yours, saying why.
    app.set_native(vec![{
        let mut p = waiting(&f.repo);
        p.color = Some("pink".into());
        p
    }]);
    on(&mut app, NATIVE);
    ctrl(&mut app, 'g');
    assert!(
        shown(&app).contains("pink's colour wins · yours: blue"),
        "{}",
        shown(&app)
    );
    assert_eq!(
        Config::load(&f.home)
            .unwrap()
            .list
            .colors
            .get(NATIVE)
            .map(String::as_str),
        Some("blue")
    );
    on(&mut app, HELD);
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(
        buf[(x, y)].bg,
        crate::tui::theme::picked("pink").unwrap(),
        "the harness's"
    );
}

#[tokio::test]
async fn ctrl_g_on_a_project_header_does_nothing_but_say_so() {
    let f = fixture();
    let mut app = list(&f).await;
    let head = format!("g:{}", f.repo.display());
    on(&mut app, &head);
    ctrl(&mut app, 'g');
    assert!(shown(&app).contains("ctrl+g colours a session"));
    assert!(Config::load(&f.home).unwrap().list.colors.is_empty());
}

#[tokio::test]
async fn a_hex_colour_of_your_own_is_drawn_as_the_names_background() {
    let f = fixture();
    let mut cfg = Config::load(&f.home).unwrap();
    cfg.list.colors.insert(NATIVE.into(), "#102060".into());
    cfg.save(&f.home).unwrap();
    let app = list(&f).await;
    let (buf, x, y) = cells_of(&app, "fix the login");
    assert_eq!(buf[(x, y)].bg, ratatui::style::Color::Rgb(0x10, 0x20, 0x60));
    assert_eq!(
        buf[(x, y)].fg,
        crate::tui::theme::swatch(buf[(x, y)].bg).fg.unwrap(),
        "light text on dark"
    );
}

/// Setup as a process with its HOME at the fixture's root would run it:
/// `claude` and `agy` the stand-ins, codex and gemini found but never run.
fn setup_host(f: &Fixture) -> crate::setup::Host {
    let root = f.home.path().parent().unwrap().to_path_buf();
    let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures"));
    crate::setup::Host {
        rl_bin: PathBuf::from("/opt/relay/bin/rl"),
        env: std::sync::Arc::new(move |k| (k == "HOME").then(|| root.clone().into())),
        bin_of: std::sync::Arc::new(move |p| match p.name.as_str() {
            "claude" => Some(fixtures.join("stand-in-claude-mcp")),
            "antigravity" => Some(fixtures.join("stand-in-agy-mcp")),
            "codex" | "gemini" => Some(PathBuf::from("/nowhere").join(&p.name)),
            _ => None,
        }),
    }
}

#[tokio::test]
async fn the_list_offers_setup_and_slash_setup_does_it_and_the_notice_goes() {
    let f = fixture();
    let mut app = list(&f).await;
    app.setup_host = Some(setup_host(&f));
    app.check_setup();
    app.finish().await;
    let foot = shown(&app);
    assert!(
        foot.contains("setup needed: antigravity, claude, codex, gemini · /setup"),
        "{foot}"
    );
    assert!(app.flashing().is_none(), "the check says nothing of its own");

    // `/setup` hands the terminal to the wizard (`main_loop`), as an
    // attach does; nothing is set up from here.
    run(&mut app, "/setup");
    assert!(app.wizard, "the wizard is asked for");
    assert!(!app.quit);
    assert_eq!(app.setup.as_ref().map(|n| n.as_ref().map(Vec::len)), Some(Ok(4)));
}

#[tokio::test]
async fn what_you_declined_is_not_in_the_notice() {
    let f = fixture();
    std::fs::write(
        f.home.config_json(),
        r#"{"badges": [], "setup": {"declined": ["antigravity/mcp"]}}"#,
    )
    .unwrap();
    let mut app = list(&f).await;
    app.setup_host = Some(setup_host(&f));
    app.check_setup();
    app.finish().await;
    let foot = shown(&app);
    assert!(foot.contains("setup needed: claude, codex, gemini · /setup"), "{foot}");
}

#[tokio::test]
async fn a_setup_that_fails_says_why_and_the_list_goes_on() {
    let f = fixture();
    let root = f.home.path().parent().unwrap().to_path_buf();
    std::fs::write(root.join(".claude.json"), "{ not json").unwrap();
    let mut app = list(&f).await;
    app.setup_host = Some(setup_host(&f));
    app.check_setup();
    app.finish().await;
    assert!(shown(&app).contains("setup check failed · /setup says why"));
    run(&mut app, "/setup");
    assert!(app.wizard, "the wizard says why");
    assert!(!app.quit);
}

/// Not a test: how many `↓` a second the list takes, each drawn, on about
/// forty rows in eight projects. `cargo test -p rl --release scroll_speed --
/// --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn scroll_speed() {
    let f = fixture();
    let mut app = list(&f).await;
    let root = f.repo.parent().unwrap().to_path_buf();
    let rows: Vec<NativeRow> = (0..40)
        .map(|i| {
            let cwd = root.join(format!("p{}", i % 8));
            std::fs::create_dir_all(&cwd).unwrap();
            NativeRow {
                id: format!("{i:08x}"),
                session_id: format!("s-{i}"),
                name: format!("session number {i}"),
                ..waiting(&cwd)
            }
        })
        .collect();
    app.set_native(rows);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    let n = 1000;
    let at = std::time::Instant::now();
    for i in 0..n {
        press(&mut app, if (i / 50) % 2 == 0 { KeyCode::Down } else { KeyCode::Up });
        t.draw(|f| app.draw(f)).unwrap();
    }
    let took = at.elapsed();
    let keys_at = std::time::Instant::now();
    for i in 0..n {
        press(&mut app, if (i / 50) % 2 == 0 { KeyCode::Down } else { KeyCode::Up });
    }
    let draws_at = std::time::Instant::now();
    for _ in 0..n {
        t.draw(|f| app.draw(f)).unwrap();
    }
    println!(
        "keys alone {:?} each, draws alone {:?} each",
        (draws_at - keys_at) / n,
        draws_at.elapsed() / n
    );
    println!(
        "{n} keys, each drawn: {took:?}, {:.0} a second, {:?} each",
        n as f64 / took.as_secs_f64(),
        took / n
    );
}

#[tokio::test]
async fn drift_leads_the_footer_until_reported_or_dismissed_and_slash_report_takes_the_terminal() {
    let f = fixture();
    let mut app = list(&f).await;
    run(&mut app, "/report");
    assert!(!app.reporting, "nothing to report");
    let dir = crate::drift::dir(&f.home);
    let n = rl_detect::drift::Noticed::new(
        "codex",
        None,
        "codex.schema.thread/name/set",
        "thread/name/set gone, rename stays in Relay",
    );
    crate::drift::record_as(&dir, &n, "0.170.0", jiff::Timestamp::now()).unwrap();
    app.flash = None;
    let foot = shown(&app);
    assert!(
        foot.contains("codex 0.170.0: thread/name/set gone, rename stays in Relay · /report"),
        "{foot}"
    );
    run(&mut app, "/report");
    assert!(app.reporting, "the report is asked for");
    app.reporting = false;
    run(&mut app, "/report nope");
    assert!(shown(&app).contains("/report takes nothing, or dismiss"));
    app.cmd.clear();
    run(&mut app, "/report dismiss");
    let foot = shown(&app);
    assert!(foot.contains("dismissed 1"), "{foot}");
    assert!(!foot.contains("thread/name/set gone"), "{foot}");
}

fn key_with(app: &mut App, code: KeyCode, m: KeyModifiers) {
    app.key(KeyEvent::new(code, m));
}

#[tokio::test]
async fn shift_or_alt_enter_adds_a_line_and_enter_starts_it_whole() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    typed(&mut app, "claude fix the login");
    key_with(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    typed(&mut app, "and the logout");
    // Alt+enter, as `ESC CR` reads where shift+enter is not told apart.
    key_with(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    typed(&mut app, "then test");
    // `^j`, a line feed: what iTerm2 set up for Claude Code sends for
    // shift+enter, through tmux too.
    ctrl(&mut app, 'j');
    typed(&mut app, "now");
    assert_eq!(
        app.cmd.line.text(),
        "claude fix the login\nand the logout\nthen test\nnow"
    );
    assert_eq!(app.sel.as_deref(), Some(NATIVE), "^j added a line, it did not move");
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.cmd.line.text(), "claude fix the login\nand the logout\nthen test");
    assert!(app.work.is_empty(), "nothing started yet");
    // Three rows, the prompt and what it starts on the first.
    let rows = screen(&app, 100, 30);
    assert!(
        rows[25].contains(" ❯ claude fix the login") && rows[25].contains("agent · claude"),
        "{}",
        rows.join("\n")
    );
    assert!(rows[26].starts_with("   and the logout"), "{}", rows[26]);
    assert!(rows[27].starts_with("   then test"), "{}", rows[27]);
    assert!(rows[28].starts_with("───"), "the separator under it: {}", rows[28]);
    assert!(rows[29].contains("shift+enter adds a line"), "{}", rows[29]);
    // ↑ walks the lines, then on the first moves on the list.
    press(&mut app, KeyCode::Up);
    assert_eq!(app.cmd.line.cursor(), 30, "column 9 of the second line");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.cmd.line.cursor(), 9);
    assert_eq!(app.sel.as_deref(), Some(NATIVE));
    press(&mut app, KeyCode::Up);
    assert_eq!(app.sel.as_deref(), Some(HELD), "the list's, from the first line");
    assert_eq!(app.cmd.line.text(), "claude fix the login\nand the logout\nthen test");
    press(&mut app, KeyCode::Enter);
    assert!(app.cmd.is_empty());
    app.finish().await;
    assert_eq!(
        std::fs::read_to_string(f.repo.join("argv")).unwrap(),
        "--bg\nfix the login\nand the logout\nthen test\n",
        "the prompt whole, its newlines kept"
    );
}

#[tokio::test]
async fn a_paste_keeps_its_newlines_on_the_command_line_and_not_in_the_search() {
    let f = fixture();
    let mut app = list(&f).await;
    app.paste("make build\r\nmake test\n");
    assert_eq!(app.cmd.line.text(), "make build\nmake test\n");
    assert!(app.work.is_empty(), "a paste starts nothing");
    assert!(row_of(&shown(&app), "❯ make build").contains("shell"));
    press(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'f');
    app.paste("fix\nlogin");
    assert_eq!(app.filter, "fix login", "the search is one line");
}

#[tokio::test]
async fn with_claude_for_default_a_line_is_its_prompt_a_word_still_picks_and_a_bang_is_a_shell() {
    let f = fixture();
    std::fs::write(f.home.config_json(), r#"{"badges": [], "list": {"default": "claude"}}"#).unwrap();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    assert!(
        line_row(&app)
            .contains("a prompt for claude, or claude/codex/agy and a prompt, ! for a shell · / for commands"),
        "{}",
        line_row(&app)
    );
    typed(&mut app, "make");
    assert_eq!(app.cmd.ghost, None, "a program would be the prompt: none offered");
    assert!(line_row(&app).contains("agent · claude"), "{}", line_row(&app));
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "! ls");
    let row = line_row(&app);
    assert!(row.contains("shell") && !row.contains("agent"), "{row}");
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "fix the login");
    assert!(line_row(&app).contains("agent · claude"));
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["--bg", "fix the login"], "the whole line its prompt");
    typed(&mut app, "codex fix the default");
    assert!(line_row(&app).contains("agent · codex"));
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert!(
        codex_was_sent(|m| m["method"] == "turn/start" && m["params"]["input"][0]["text"] == "fix the default").await,
        "codex, on the rest"
    );
}

#[tokio::test]
async fn slash_default_sets_what_a_line_starts_and_refuses_what_is_not_an_agent() {
    let f = fixture();
    let mut app = list(&f).await;
    run(&mut app, "/default codex");
    assert!(app.cmd.is_empty(), "cleared once run");
    assert_eq!(Config::load(&f.home).unwrap().list.default, "codex", "kept");
    assert!(
        shown(&app).contains("a typed line is a prompt for codex"),
        "{}",
        shown(&app)
    );
    typed(&mut app, "fix it");
    assert!(line_row(&app).contains("agent · codex"), "{}", line_row(&app));
    press(&mut app, KeyCode::Esc);
    run(&mut app, "/default make");
    assert_eq!(app.cmd.line.text(), "/default make", "kept to mend");
    assert!(shown(&app).contains("not an agent harness: make"), "{}", shown(&app));
    assert_eq!(Config::load(&f.home).unwrap().list.default, "codex");
    press(&mut app, KeyCode::Esc);
    run(&mut app, "/default shell");
    assert_eq!(Config::load(&f.home).unwrap().list.default, "shell");
    typed(&mut app, "fix it");
    assert!(line_row(&app).contains("shell"), "{}", line_row(&app));
}

/// A folder beside the fixture's repo, with nothing running in it.
fn beside(f: &Fixture, name: &str) -> PathBuf {
    let p = f.repo.parent().unwrap().join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test]
async fn hiding_a_projects_last_row_keeps_the_cursor_in_that_project() {
    let f = fixture();
    let yard = beside(&f, "yard");
    let mut app = list(&f).await;
    app.set_native(vec![
        waiting(&f.repo),
        NativeRow {
            id: "ef56ab78".into(),
            name: "paint the fence".into(),
            ..waiting(&yard)
        },
    ]);
    app.reload();
    let shop = format!("g:{}", f.repo.display());
    let yard_head = format!("g:{}", yard.display());
    assert_eq!(
        app.keys(),
        [shop.as_str(), HELD, NATIVE, yard_head.as_str(), "n:claude:ef56ab78"]
    );
    // The last row of shop: the cursor goes up to the one before it, not on
    // to yard.
    on(&mut app, NATIVE);
    run(&mut app, "/hide");
    assert_eq!(app.sel.as_deref(), Some(HELD));
}

#[tokio::test]
async fn a_line_typed_on_a_session_in_a_worktree_starts_in_its_project_root() {
    let f = fixture();
    let tree = f.repo.join(".claude/worktrees/login");
    std::fs::create_dir_all(&tree).unwrap();
    let mut app = list(&f).await;
    app.set_native(vec![waiting(&tree)]);
    app.reload();
    on(&mut app, NATIVE);
    assert_eq!(
        app.line_folder(),
        f.repo,
        "the project it is listed under, not its worktree"
    );
    typed(&mut app, "claude fix the logout");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&f.repo), ["--bg", "fix the logout"]);
}

#[tokio::test]
async fn slash_project_adds_an_empty_project_the_cursor_on_it_and_a_line_starts_there() {
    let f = fixture();
    let notes = beside(&f, "notes");
    let mut app = list(&f).await;
    on(&mut app, HELD);
    // Relative to the row's folder.
    run(&mut app, "/project ../notes");
    assert!(app.cmd.is_empty(), "cleared once run");
    let key = format!("g:{}", notes.display());
    assert_eq!(Config::load(&f.home).unwrap().list.projects, [notes.to_str().unwrap()]);
    assert_eq!(app.sel.as_deref(), Some(key.as_str()), "the cursor on its header");
    let s = shown(&app);
    assert!(s.contains("added notes · type to start"), "{s}");
    let head = row_of(&s, super::draw::EMPTY_HEAD);
    assert!(head.contains("notes"), "{head}");
    // Its header is where a line starts: claude, in the folder.
    assert_eq!(app.line_folder(), notes);
    typed(&mut app, "claude write it down");
    press(&mut app, KeyCode::Enter);
    app.finish().await;
    assert_eq!(ran_in(&notes), ["--bg", "write it down"], "run in the project's folder");
    // Kept: a list opened again has it.
    let again = list(&f).await;
    assert!(again.keys().contains(&key), "{:?}", again.keys());
}

#[tokio::test]
async fn slash_project_in_a_checkout_adds_its_project_and_one_already_there_is_only_gone_to() {
    let f = fixture();
    std::fs::create_dir_all(f.repo.join("app/src")).unwrap();
    let notes = beside(&f, "notes");
    let mut app = list(&f).await;
    let shop = format!("g:{}", f.repo.display());
    on(&mut app, NATIVE);
    // A folder inside the repo is the repo's project — one there already.
    run(&mut app, "/project app/src");
    assert_eq!(app.sel.as_deref(), Some(shop.as_str()));
    assert!(shown(&app).contains("shop is a project already"), "{}", shown(&app));
    assert!(Config::load(&f.home).unwrap().list.projects.is_empty(), "nothing added");
    // An added one, added again: no second one.
    run(&mut app, &format!("/project {}", notes.display()));
    on(&mut app, HELD);
    run(&mut app, &format!("/project {}/", notes.display()));
    let key = format!("g:{}", notes.display());
    assert_eq!(app.sel.as_deref(), Some(key.as_str()));
    assert!(shown(&app).contains("notes is a project already"), "{}", shown(&app));
    assert_eq!(Config::load(&f.home).unwrap().list.projects.len(), 1);
    assert_eq!(app.keys().iter().filter(|k| **k == key).count(), 1);
    // Grouped by state, an empty project has no place.
    ctrl(&mut app, 's');
    assert!(!app.keys().contains(&key), "{:?}", app.keys());
    assert!(!shown(&app).contains(super::draw::EMPTY_HEAD));
}

#[tokio::test]
async fn slash_project_refuses_what_is_not_a_folder_and_says_how_with_nothing() {
    let f = fixture();
    std::fs::write(f.repo.join("README"), "").unwrap();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    run(&mut app, "/project nowhere");
    assert!(shown(&app).contains("no such folder: nowhere"), "{}", shown(&app));
    assert_eq!(app.cmd.line.text(), "/project nowhere", "kept to mend");
    press(&mut app, KeyCode::Esc);
    run(&mut app, "/project README");
    assert!(shown(&app).contains("not a folder: README"), "{}", shown(&app));
    press(&mut app, KeyCode::Esc);
    run(&mut app, "/project");
    assert!(shown(&app).contains("/project takes a folder"), "{}", shown(&app));
    assert!(Config::load(&f.home).unwrap().list.projects.is_empty());
}

#[tokio::test]
async fn slash_hide_on_an_empty_added_project_takes_it_off_and_out_of_config() {
    let f = fixture();
    let notes = beside(&f, "notes");
    let mut app = list(&f).await;
    run(&mut app, &format!("/project {}", notes.display()));
    let key = format!("g:{}", notes.display());
    // Renamed, and moved among the projects, as any header.
    run(&mut app, "/rename my notes");
    assert_eq!(
        Config::load(&f.home).unwrap().list.names.get(&key).map(String::as_str),
        Some("my notes")
    );
    assert!(row_of(&shown(&app), super::draw::EMPTY_HEAD).contains("my notes"));
    shift(&mut app, KeyCode::Down);
    let order = Config::load(&f.home).unwrap().list.order;
    assert!(order[0].rows.contains(&key), "{order:?}");
    on(&mut app, &key);
    app.flash = None;
    assert!(shown(&app).contains("ctrl+x twice to take it off"), "{}", shown(&app));
    run(&mut app, "/hide");
    let p = Config::load(&f.home).unwrap().list;
    assert!(p.projects.is_empty() && p.hidden.is_empty(), "{p:?}");
    assert!(!p.names.contains_key(&key), "its name goes with it");
    assert!(p.order.iter().all(|o| !o.rows.contains(&key)), "{:?}", p.order);
    assert!(!app.keys().contains(&key));
    assert!(shown(&app).contains("took my notes off the list"), "{}", shown(&app));
    // A project with sessions is hidden, as ever.
    let shop = format!("g:{}", f.repo.display());
    on(&mut app, &shop);
    run(&mut app, "/hide");
    assert_eq!(Config::load(&f.home).unwrap().list.hidden[0].key, shop);
}

#[tokio::test]
async fn tab_completes_the_folder_of_slash_project_under_the_rows_folder() {
    let f = fixture();
    std::fs::create_dir_all(f.repo.join("src")).unwrap();
    std::fs::write(f.repo.join("setup.sh"), "").unwrap();
    let mut app = list(&f).await;
    on(&mut app, HELD);
    typed(&mut app, "/proj");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/project ");
    typed(&mut app, "s");
    assert_eq!(app.cmd.ghost.as_deref(), Some("rc/"), "folders only");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.cmd.line.text(), "/project src/");
}

#[tokio::test]
async fn a_row_carried_on_as_a_new_one_elsewhere_shows_once_under_the_new_key_with_what_you_kept() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    ctrl(&mut app, 't');
    std::thread::sleep(std::time::Duration::from_millis(20));
    // `rl mcp` woke it as a new session and carried the row over.
    let new = "n:claude:9f1e2d3c";
    let mut c = Config::load(&f.home).unwrap();
    c.list.names.insert(NATIVE.into(), "login box".into());
    c.list.carry(NATIVE, new, Timestamp::now());
    c.save(&f.home).unwrap();
    let old = NativeRow {
        state: NativeState {
            words: "completed".into(),
            state: RunState::Finished,
        },
        ..waiting(&f.repo)
    };
    let carried = NativeRow {
        id: "9f1e2d3c".into(),
        session_id: "s-2".into(),
        name: "fix the login".into(),
        ..waiting(&f.repo)
    };
    app.set_native(vec![old, carried]);
    let s = shown(&app);
    assert_eq!(s.matches("login box").count(), 1, "one row: {s}");
    assert!(!app.keys().iter().any(|k| k == NATIVE), "the old one hidden");
    assert_eq!(app.keys()[0], new, "pinned, as the old one was: {s}");
    assert_eq!(app.prefs().pins, [new]);
}

#[tokio::test]
async fn a_conversation_carried_on_from_another_terminal_takes_what_you_kept_for_its_row() {
    let f = fixture();
    let mut app = list(&f).await;
    let (old, new) = ("o:claude:s-9", "n:claude:9f1e2d3c");
    app.change_prefs(|p| {
        p.names.insert(old.into(), "login box".into());
        p.colors.insert(old.into(), "blue".into());
        p.pins.push(old.into());
    });
    app.done(super::app::Done {
        carried: Some((old.into(), new.into())),
        ..super::app::Done::said("login box carries on in claude's background")
    });
    let p = Config::load(&f.home).unwrap().list;
    assert_eq!(p.names.get(new).map(String::as_str), Some("login box"));
    assert_eq!(p.colors.get(new).map(String::as_str), Some("blue"));
    assert_eq!(p.pins, [new]);
    assert!(
        p.hidden.iter().any(|h| h.key == old),
        "the old row hidden, as /hide does"
    );
}

#[test]
fn the_settle_counts_from_when_our_modes_were_set() {
    use std::time::{Duration, Instant};
    // The first frame took most of it already: only the rest is left. (What
    // is left, not how long a sleep took: a loaded machine oversleeps.)
    let late = super::Settling {
        since: Instant::now() - Duration::from_millis(50),
    };
    assert!(late.left() <= Duration::from_millis(10), "{:?}", late.left());
    // Past it: nothing left.
    let past = super::Settling {
        since: Instant::now() - super::SETTLE * 2,
    };
    assert_eq!(past.left(), Duration::ZERO);
    // Just set: the whole of it, so what was on its way still comes first —
    // and a wait does wait it out (a sleep never ends early).
    let fresh = super::Settling { since: Instant::now() };
    assert!(fresh.left() > super::SETTLE / 2, "{:?}", fresh.left());
    fresh.wait();
    assert!(
        fresh.since.elapsed() >= super::SETTLE,
        "waited out from when the modes were set"
    );
}

#[tokio::test]
async fn nothing_typed_reads_no_path_and_a_scan_under_way_is_what_the_line_gets() {
    let f = fixture();
    let app = App::new(f.home.clone()).unwrap();
    // The list drawn with an empty line: `PATH` is not read for it.
    shown(&app);
    assert!(app.detected().is_none());
    assert!(app.commands.get().is_none(), "no PATH read for an empty line");
    // A scan under way is waited for, not done again.
    let bin = f.repo.parent().unwrap().join("bin");
    let (tx, rx) = std::sync::mpsc::channel();
    app.scanning.set(Some(rx));
    tx.send(super::cmdline::bin_dir(&bin, &["only-here"])).unwrap();
    assert!(app.commands().has("only-here"));
    assert!(!app.commands().has("sh"), "the scan's, not this PATH");
}

#[tokio::test]
async fn coming_back_does_not_wait_for_the_badges() {
    let f = fixture();
    std::fs::write(f.home.config_json(), r#"{"badges": ["sleep 5; echo slow"]}"#).unwrap();
    let at = std::time::Instant::now();
    let mut app = App::new(f.home.clone()).unwrap();
    app.back("r:shop-sh", "let go of shop-sh · it keeps going".into());
    app.regit();
    shown(&app);
    assert!(
        at.elapsed() < std::time::Duration::from_secs(2),
        "the list is read and drawn while the badge is still asked: {:?}",
        at.elapsed()
    );
    assert!(shown(&app).contains("⎇ trunk"), "the checkout is there at once");
}

#[tokio::test]
async fn rows_read_off_the_ui_thread_are_taken_unless_read_before_what_the_list_has() {
    let f = fixture();
    let mut app = list(&f).await;
    let (at, home, native) = app.to_read();
    let rows = super::app::read_rows(&home, &native);
    assert_eq!(rows, app.rows, "what a reload reads");
    // A reload since: what it read is newer.
    app.reload();
    assert!(!app.read_done(at, rows.clone()));
    let (at, ..) = app.to_read();
    assert!(app.read_done(at, rows.clone()));
    // The harnesses said something new since: read with their old rows.
    let (at, ..) = app.to_read();
    app.heard_native(app.native.clone());
    assert!(!app.read_done(at, Vec::new()));
    assert_eq!(app.rows, rows, "passed over");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_asked_for_while_one_runs_are_one_more_after_it() {
    let f = fixture();
    let mut app = list(&f).await;
    let mut reads = super::Reads::default();
    for _ in 0..3 {
        reads.ask(&mut app);
    }
    let (at, rows) = reads.next().await.expect("read");
    assert!(app.read_done(at, rows));
    // A session that came meanwhile is in the one more.
    let dir = f.home.session_dir("late-sh");
    std::fs::create_dir_all(&dir).unwrap();
    sessions::write_meta(
        &dir,
        &Meta {
            name: "late-sh".into(),
            harness: "sh".into(),
            kind: RunKind::Shell,
            cwd: f.repo.clone(),
            argv: vec!["sh".into()],
            started: Timestamp::now(),
            socket: dir.join("sock"),
            holder_pid: None,
        },
    )
    .unwrap();
    reads.again(&mut app);
    assert!(reads.task.is_some(), "one more, for what was asked meanwhile");
    let (at, rows) = reads.next().await.expect("read");
    assert!(app.read_done(at, rows));
    assert!(app.rows.iter().any(|r| r.key == "r:late-sh"));
    reads.again(&mut app);
    assert!(reads.task.is_none(), "and no more");
}

// ---- space: the peek pane --------------------------------------------

/// The peek pane as drawn at 100 by 30: its rule, then its lines, up to the
/// rule over the command line. Empty where there is none.
fn pane(app: &App) -> Vec<String> {
    let rows = screen(app, 100, 30);
    let rule = |r: &String| r.trim_end().chars().all(|c| c == '─');
    let Some(at) = rows.iter().position(|r| r.starts_with("─ ") && !rule(r)) else {
        return Vec::new();
    };
    rows[at..]
        .iter()
        .take_while(|r| !rule(r))
        .map(|r| r.trim_end().to_string())
        .collect()
}

/// Until the pane shows what the row under the cursor's harness said: each
/// ask as the list's loop would make it, once it is due, and each answer
/// taken as it comes. Answers for other rows may still be on their way.
async fn peek_settles(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let here = app.peek_row().map(|r| r.key.clone()).expect("on a session");
        if app.peek.as_ref().and_then(|p| p.shown.as_ref()).map(|(k, _)| k) == Some(&here) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "no peek came for {here}");
        if let Some(at) = app.peek_due() {
            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
            app.ask_peek();
        }
        let next = tokio::time::timeout(std::time::Duration::from_millis(100), app.peeks.join_next()).await;
        if let Ok(Some(p)) = next {
            app.peeked(p.unwrap());
        }
    }
}

#[tokio::test]
async fn space_peeks_at_the_session_and_the_cursor_moving_peeks_at_the_next() {
    let f = fixture();
    let mut app = list(&f).await;
    on(&mut app, NATIVE);
    assert!(shown(&app).contains("space to peek"), "a session row's hint");
    assert!(pane(&app).is_empty(), "shut till asked");
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(pane(&app)[1..], ["  …"], "asked, not yet said");
    peek_settles(&mut app).await;
    let p = pane(&app);
    assert!(
        p[0].starts_with("─ fix the login · claude · needs input ──"),
        "its name, harness and state: {p:?}"
    );
    assert_eq!(
        p[1..],
        ["  ab12cd34 is at work", "  step 2 of 3"],
        "what `claude logs` printed, as its terminal showed it"
    );
    let again = app
        .peek_due()
        .expect("asked again while it stays")
        .duration_since(std::time::Instant::now());
    assert!(
        again > std::time::Duration::from_secs(1) && again <= super::peek::AGAIN,
        "{again:?}"
    );
    assert!(
        row_of(&shown(&app), "fix the login").contains("needs input"),
        "the list stays"
    );
    assert!(shown(&app).contains("space to close the peek"));
    // The cursor moved: the next row is peeked at once it settles, and what
    // the last said is not shown as its.
    on(&mut app, HELD);
    assert!(pane(&app)[0].starts_with("─ shop-sh · sh"), "{:?}", pane(&app));
    assert_eq!(pane(&app)[1..], ["  …"]);
    peek_settles(&mut app).await;
    let p = pane(&app);
    assert!(
        p[1].starts_with("  shop-sh is not answering"),
        "an ended one whose holder is gone: why, said {p:?}"
    );
    // Back, and asked again.
    on(&mut app, NATIVE);
    peek_settles(&mut app).await;
    assert_eq!(pane(&app)[1..], ["  ab12cd34 is at work", "  step 2 of 3"]);
    // Enter attaches as ever.
    press(&mut app, KeyCode::Enter);
    let hand = app.hand.take().expect("handed over");
    assert_eq!(hand.key, NATIVE);
    // Esc shuts it, and space again; neither clears the search.
    app.filter = "login".into();
    press(&mut app, KeyCode::Esc);
    assert!(app.peek.is_none() && pane(&app).is_empty());
    assert_eq!(app.filter, "login", "the pane shut first");
    press(&mut app, KeyCode::Char(' '));
    assert!(app.peek.is_some());
    press(&mut app, KeyCode::Char(' '));
    assert!(app.peek.is_none(), "space shuts it too");
}

#[tokio::test]
async fn on_a_header_space_opens_no_pane_and_the_footer_says_why() {
    let f = fixture();
    let mut app = list(&f).await;
    let header = app.keys()[0].clone();
    on(&mut app, &header);
    assert!(matches!(app.selected(), Some(crate::rows::Line::Group(_))));
    press(&mut app, KeyCode::Char(' '));
    assert!(app.peek.is_none());
    assert!(pane(&app).is_empty());
    assert!(shown(&app).contains("space peeks at a session: put the cursor on one"));
    // Open on a session, then onto the header: no pane, and why.
    on(&mut app, NATIVE);
    press(&mut app, KeyCode::Char(' '));
    on(&mut app, &header);
    assert!(pane(&app).is_empty());
    assert_eq!(app.peek_due(), None, "nothing to ask");
    assert!(shown(&app).contains("the peek shows a session: put the cursor on one"));
    // A space typed on the command line is a space in it.
    typed(&mut app, "make");
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.cmd.line.text(), "make ");
    assert!(app.peek.is_some(), "still open, as it was");
}

#[tokio::test]
async fn a_slow_peek_never_holds_a_key_up() {
    let f = fixture();
    let mut app = list(&f).await;
    // Its harness takes two seconds to say anything.
    std::fs::write(f.repo.join("peek-slow"), "2").unwrap();
    on(&mut app, NATIVE);
    let at = std::time::Instant::now();
    press(&mut app, KeyCode::Char(' '));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
    }
    on(&mut app, HELD);
    let _ = shown(&app);
    assert!(
        at.elapsed() < std::time::Duration::from_millis(500),
        "keys and draws wait for no peek: {:?}",
        at.elapsed()
    );
    // The row moved to is peeked at meanwhile, the slow one still on its way.
    peek_settles(&mut app).await;
    assert!(pane(&app)[1].starts_with("  shop-sh is not answering"));
    assert!(
        at.elapsed() < std::time::Duration::from_millis(1500),
        "not after the slow one: {:?}",
        at.elapsed()
    );
    assert!(!app.peeks.is_empty(), "the slow one still under way");
    // Its answer, for a row the cursor has left, is not shown.
    let late = app.peeks.join_next().await.unwrap().unwrap();
    assert_eq!(late.0, NATIVE);
    app.peeked(late);
    assert!(pane(&app)[1].starts_with("  shop-sh is not answering"));
}
