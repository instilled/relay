//! The wizard step by step: drawn at 100 and at 60 columns, every way
//! through it, and the changes made through the stand-ins (`claude` and
//! `agy`; codex and gemini are files) in a HOME of its own.

use super::engine;
use super::model::{Came, Effect, Ended, Progress, Run, Step, Wizard, gather};
use super::view::{self, Drawn};
use crate::setup::{self, Host};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rl_core::Home;
use std::path::PathBuf;
use std::sync::Arc;

const RL: &str = "/opt/relay/bin/rl";

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    home: Home,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let home = Home::at(dir.join(".relay"));
        Fixture { _tmp: tmp, dir, home }
    }

    /// Claude and agy the stand-ins, codex and gemini found but never run.
    fn host(&self) -> Host {
        let dir = self.dir.clone();
        let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures"));
        Host {
            rl_bin: PathBuf::from(RL),
            env: Arc::new(move |k| (k == "HOME").then(|| dir.clone().into())),
            bin_of: Arc::new(move |p| match p.name.as_str() {
                "claude" => Some(fixtures.join("stand-in-claude-mcp")),
                "antigravity" => Some(fixtures.join("stand-in-agy-mcp")),
                "codex" | "gemini" => Some(PathBuf::from("/nowhere").join(&p.name)),
                _ => None,
            }),
        }
    }

    fn wizard(&self, came: Came) -> Wizard {
        Wizard::new(came, gather(&self.home, &self.host()).unwrap())
    }

    /// What a key asked for, done here and now, as the screen's loop does
    /// it off its thread.
    fn effect(&self, wz: &mut Wizard, e: Option<Effect>) {
        let Some(Effect::Apply { jobs, decline, accept }) = e else {
            return;
        };
        setup::set_declined(&self.home, &decline, &accept).unwrap();
        if !jobs.is_empty() {
            engine::work(&self.home, &self.host(), &jobs, |p| wz.progress(p));
        }
    }

    fn press(&self, wz: &mut Wizard, k: KeyEvent) {
        let e = wz.key(k);
        self.effect(wz, e);
    }

    fn claude_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    fn agy_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(".gemini/config/argv"))
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    fn check(&self) -> Vec<String> {
        self.host()
            .check(&self.home)
            .unwrap()
            .iter()
            .map(|n| n.to_string())
            .collect()
    }
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// The wizard drawn `w` by `h`: its text, a line a row, and where things
/// went.
fn screen(wz: &Wizard, w: u16, h: u16) -> (String, Drawn) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut drawn = Drawn::default();
    t.draw(|f| drawn = view::draw(wz, f)).unwrap();
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    (out, drawn)
}

/// Every line of `text` fits `w`.
fn fits(text: &str, w: usize) {
    for l in text.lines() {
        assert!(unicode_width::UnicodeWidthStr::width(l) <= w, "wider than {w}: {l:?}");
    }
}

#[test]
fn step_1_lists_every_agent_harness_and_the_two_ways_on() {
    let f = Fixture::new();
    let wz = f.wizard(Came::Command);
    for w in [100, 60] {
        let (s, _) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("relay setup  1/6"), "{s}");
        assert!(s.contains("✻ claude"), "claude's icon: {s}");
        assert!(s.contains("⬡ codex"), "{s}");
        assert!(s.contains("found"), "{s}");
        assert!(s.contains("not installed"), "cursor and opencode: {s}");
        assert!(!s.contains(" sh "), "no shells: {s}");
        assert!(s.contains("▸ Walk me through it"), "{s}");
        assert!(s.contains("Yes, just get me there"), "{s}");
        assert!(s.contains("enter to walk through · tab to just get me there"), "{s}");
    }
    let (wide, _) = screen(&wz, 100, 30);
    assert!(wide.contains("One list for every agent. Let's connect your harnesses."));
    assert!(wide.contains("hooks · mcp relay · ← back to list"), "{wide}");
    assert!(wide.contains(crate::tui::mark::SMALL[1]), "the list's mark: {wide}");
}

#[test]
fn the_icons_wear_their_harness_colour() {
    let f = Fixture::new();
    let wz = f.wizard(Command);
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|fr| {
        view::draw(&wz, fr);
    })
    .unwrap();
    let buf = t.backend().buffer();
    let at = (0..30)
        .flat_map(|y| (0..100).map(move |x| (x, y)))
        .find(|&(x, y)| buf[(x, y)].symbol() == "✻")
        .unwrap();
    assert_eq!(buf[at].fg, ratatui::style::Color::Rgb(0xd9, 0x77, 0x57));
}

use Came::Command;

#[test]
fn enter_walks_through_and_quitting_before_step_3_changes_nothing() {
    let f = Fixture::new();
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::Changes);
    f.press(&mut wz, key(KeyCode::Esc));
    assert_eq!(wz.step, Step::Found, "esc goes back");
    f.press(&mut wz, key(KeyCode::Char('w')));
    assert_eq!(wz.step, Step::Changes, "w walks too");
    f.press(&mut wz, key(KeyCode::Char(' ')));
    f.press(&mut wz, key(KeyCode::Char('q')));
    assert_eq!(wz.ended, Some(Ended::Skipped));
    assert_eq!(f.check().len(), 4, "nothing set up");
    assert!(f.claude_calls().is_empty() && f.agy_calls().is_empty());
    assert!(!f.home.config_json().exists(), "nothing written");
    let sum: Vec<String> = view::summary(&wz).iter().map(|l| l.to_string()).collect();
    assert!(sum[0].contains("nothing changed · rl setup any time"), "{sum:?}");

    let mut wz = f.wizard(Command);
    wz.key(ctrl('c'));
    assert_eq!(wz.ended, Some(Ended::Interrupted));
}

#[test]
fn step_2_has_a_box_per_change_and_space_or_a_click_ticks_it() {
    let f = Fixture::new();
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Enter));
    for w in [100, 60] {
        let (s, _) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("relay setup  2/6"), "{s}");
        assert!(s.contains("[x] + hook"), "{s}");
        assert!(s.contains("[x] + mcp relay"), "{s}");
        assert!(s.contains("[x] ~ leftArrowOpensAgents false"), "{s}");
        assert!(s.contains("~/.codex/hooks.json"), "{s}");
    }
    let (wide, _) = screen(&wz, 100, 30);
    assert!(
        wide.contains("▸ [x] ✦ antigravity"),
        "the cursor on the first card: {wide}"
    );
    assert!(wide.contains("so agents can message each other"), "{wide}");

    // Space on a change: that one alone.
    let rows = wz.rows();
    let left = rows
        .iter()
        .position(|(h, c)| c.is_some_and(|c| wz.harns[*h].changes()[c].label.starts_with("leftArrow")))
        .unwrap();
    wz.cursor = left;
    f.press(&mut wz, key(KeyCode::Char(' ')));
    let (s, drawn) = screen(&wz, 100, 30);
    assert!(s.contains("[ ] ~ leftArrowOpensAgents false"), "{s}");
    assert!(s.contains("[-] ✻ claude"), "the header says some: {s}");
    assert!(
        s.contains("← goes back to Relay's list"),
        "the why under the cursor: {s}"
    );

    // A click on that line ticks it again.
    let y = drawn.rows.iter().find(|(_, r)| *r == left).unwrap().0;
    assert_eq!(drawn.row_at(y), Some(left));
    wz.click(drawn.row_at(y).unwrap());
    assert!(screen(&wz, 100, 30).0.contains("[x] ~ leftArrowOpensAgents false"));

    // The header ticks all of its changes.
    let claude = rows
        .iter()
        .position(|(h, c)| wz.harns[*h].name == "claude" && c.is_none())
        .unwrap();
    wz.cursor = claude;
    f.press(&mut wz, key(KeyCode::Char(' ')));
    let (s, _) = screen(&wz, 100, 30);
    assert!(s.contains("[ ] ✻ claude"), "{s}");
    assert!(!s.contains("[x] ~ leftArrow"), "{s}");
    f.press(&mut wz, key(KeyCode::Char(' ')));
    assert!(screen(&wz, 100, 30).0.contains("[x] ✻ claude"));

    // ↑↓ and ctrl+j/k move among the lines.
    wz.cursor = 0;
    f.press(&mut wz, key(KeyCode::Down));
    f.press(&mut wz, ctrl('j'));
    assert_eq!(wz.cursor, 2);
    f.press(&mut wz, ctrl('k'));
    f.press(&mut wz, key(KeyCode::Up));
    f.press(&mut wz, key(KeyCode::Up));
    assert_eq!(wz.cursor, 0);
}

#[test]
fn apply_runs_only_what_is_ticked_and_declines_the_rest() {
    let f = Fixture::new();
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Enter));
    // Claude off altogether; antigravity's one change stays on.
    let claude = wz
        .rows()
        .iter()
        .position(|(h, c)| wz.harns[*h].name == "claude" && c.is_none())
        .unwrap();
    wz.cursor = claude;
    f.press(&mut wz, key(KeyCode::Char(' ')));
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::Applying);
    assert!(wz.all_done);
    assert!(
        wz.jobs.iter().all(|j| matches!(j.run, Run::Done(Ok(_)))),
        "{:?}",
        wz.jobs
    );
    assert!(f.claude_calls().is_empty(), "claude never run: it was off");
    assert_eq!(f.agy_calls().len(), 1);
    assert!(f.dir.join(".codex/hooks.json").exists());
    assert!(!f.dir.join(".claude").exists());
    assert_eq!(
        setup::declined(&f.home).unwrap(),
        ["claude/hooks", "claude/mcp", "claude/settings/leftArrowOpensAgents"]
    );
    assert!(f.check().is_empty(), "declined, so not named: {:?}", f.check());

    for w in [100, 60] {
        let (s, _) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("relay setup  3/6"), "{s}");
        assert!(s.contains("✓ ⬡ codex"), "{s}");
        assert!(s.contains("✓ all set up"), "{s}");
    }
    // No going back past what is done.
    f.press(&mut wz, key(KeyCode::Esc));
    assert_eq!(wz.step, Step::Applying);

    // Step 4: what only you can do.
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::YourTurn);
    for w in [100, 60] {
        let (s, _) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("4/6") && s.contains("[ ] ⬡ codex"), "{s}");
        assert!(s.contains("/hooks"), "codex's note: {s}");
    }
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::Keys);
    f.press(&mut wz, key(KeyCode::Esc));
    assert_eq!(wz.step, Step::YourTurn, "back once more");
    f.press(&mut wz, key(KeyCode::Enter));

    // Step 5: ctrl+j goes on, as it will move on the list.
    f.press(&mut wz, ctrl('j'));
    assert_eq!(wz.step, Step::Now);
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.ended, Some(Ended::Finished));
    let sum: Vec<String> = view::summary(&wz).iter().map(|l| l.to_string()).collect();
    let all = sum.join("\n");
    assert!(all.contains("✓ ⬡ codex  hook · mcp relay"), "{all}");
    assert!(all.contains("→ codex: codex asks you to trust it once"), "{all}");
    assert!(all.contains(crate::guide::MESSAGING_GUIDE), "{all}");
    assert!(!all.contains("claude"), "{all}");
    assert_eq!(wz.said(), "set up: antigravity, codex, gemini");
}

#[test]
fn a_declined_change_comes_back_unticked_and_ticking_it_makes_it() {
    let f = Fixture::new();
    let left = "claude/settings/leftArrowOpensAgents".to_string();
    setup::set_declined(&f.home, std::slice::from_ref(&left), &[]).unwrap();
    assert!(!f.check().iter().any(|n| n.contains('←')), "{:?}", f.check());

    // The quick way leaves it be.
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Tab));
    assert!(f.check().is_empty());
    assert!(f.json_claude().get("leftArrowOpensAgents").is_none());
    assert_eq!(setup::declined(&f.home).unwrap(), std::slice::from_ref(&left));

    // The walkthrough shows it unticked; ticked, it is made.
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Enter));
    let (s, _) = screen(&wz, 100, 30);
    assert!(s.contains("[ ] ~ leftArrowOpensAgents false"), "{s}");
    let row = wz.rows().iter().position(|(_, c)| c.is_some()).unwrap();
    wz.click(row);
    f.press(&mut wz, key(KeyCode::Enter));
    assert!(wz.all_done);
    assert_eq!(f.json_claude()["leftArrowOpensAgents"], serde_json::json!(false));
    assert!(setup::declined(&f.home).unwrap().is_empty());
}

impl Fixture {
    fn json_claude(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.dir.join(".claude.json")).unwrap()).unwrap()
    }
}

#[test]
fn tab_sets_up_everything_and_leaves_a_summary() {
    let f = Fixture::new();
    let mut wz = f.wizard(Came::FirstRun);
    let e = wz.key(key(KeyCode::Tab));
    assert!(wz.fast);
    assert_eq!(wz.step, Step::Applying);
    // Drawn while it runs: the spinner, no step number.
    wz.progress(Progress::Started(0));
    let (s, _) = screen(&wz, 60, 30);
    assert!(s.contains("Setting up everything") && !s.contains("3/6"), "{s}");
    assert!(s.contains("⠋"), "{s}");
    assert!(s.contains("setting up…"), "{s}");
    f.effect(&mut wz, e);
    assert_eq!(wz.ended, Some(Ended::Finished), "over once done: no steps 4 to 6");
    assert!(f.check().is_empty());
    assert_eq!(f.claude_calls().len(), 2);
    let all: String = view::summary(&wz).iter().map(|l| format!("{l}\n")).collect();
    assert!(
        all.contains("✓ ✻ claude  hook · mcp relay · leftArrowOpensAgents false"),
        "{all}"
    );
    assert!(
        all.contains("→ codex: codex asks you to trust it once: open codex, /hooks"),
        "{all}"
    );
    assert!(
        all.contains("enter attach · ctrl+\\ detach · ↑↓ ctrl+j/k move · ctrl+h/f1 all keys"),
        "{all}"
    );
    assert!(all.contains(crate::guide::MESSAGING_GUIDE), "{all}");
    assert!(all.lines().count() <= 10, "compact: {all}");

    // `y` is the quick way too; with nothing to do it says so and ends.
    let mut wz = f.wizard(Came::FirstRun);
    assert_eq!(wz.key(key(KeyCode::Char('y'))), None);
    assert_eq!(wz.ended, Some(Ended::Finished));
    let all: String = view::summary(&wz).iter().map(|l| format!("{l}\n")).collect();
    assert!(all.contains("✓ already set up"), "{all}");
}

#[test]
fn with_nothing_to_do_the_walk_goes_to_the_keys() {
    let f = Fixture::new();
    let mut wz = f.wizard(Command);
    f.press(&mut wz, key(KeyCode::Tab));
    let mut wz = f.wizard(Came::List);
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::Keys);
    for w in [100, 60] {
        let (s, _) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("relay setup  5/6"), "{s}");
        assert!(s.contains("✓ already set up"), "{s}");
        assert!(s.contains("try it: press ctrl+j to go on"), "{s}");
        for (id, does) in crate::tui::help::ESSENTIAL {
            assert!(s.contains(crate::tui::help::get(id).keys), "{s}");
            assert!(s.contains(does), "{s}");
        }
        assert!(s.contains("the session keeps running"), "{s}");
    }
    let (wide, _) = screen(&wz, 100, 30);
    assert!(wide.contains("↑↓ ctrl+j/k  move"), "two columns: {wide}");
    assert!(
        wide.contains("enter      attach") || wide.contains("enter       attach"),
        "{wide}"
    );
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.step, Step::Now);
    for w in [100, 60] {
        let (s, drawn) = screen(&wz, w, 30);
        fits(&s, w as usize);
        assert!(s.contains("relay setup  6/6"), "{s}");
        assert!(
            s.contains("✻ api") && s.contains("──send──▶") && s.contains("⬡ web"),
            "{s}"
        );
        assert!(s.contains("review my diff"), "{s}");
        assert!(s.contains(view::LINK_TEXT), "{s}");
        assert!(s.contains("enter to go back to the list"), "{s}");
        let r = drawn.link.expect("the link is on screen");
        let row: String = s.lines().nth(r.y as usize).unwrap().to_string();
        assert!(row.contains(view::LINK_TEXT), "{row}");
    }
    f.press(&mut wz, key(KeyCode::Esc));
    assert_eq!(wz.step, Step::Keys);
    f.press(&mut wz, key(KeyCode::Right));
    f.press(&mut wz, key(KeyCode::Enter));
    assert_eq!(wz.ended, Some(Ended::Finished));
    assert_eq!(wz.said(), "setup: already set up");
}

#[test]
fn a_change_that_fails_says_why_and_the_rest_go_on() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.dir.join(".gemini")).unwrap();
    let mut wz = f.wizard(Command);
    // Gemini's file goes bad between the check and the run.
    f.press(&mut wz, key(KeyCode::Enter));
    std::fs::write(f.dir.join(".gemini/settings.json"), "{ nope").unwrap();
    f.press(&mut wz, key(KeyCode::Enter));
    assert!(wz.all_done);
    assert_eq!(wz.failed(), 2, "{:?}", wz.jobs);
    assert!(f.dir.join(".codex/hooks.json").exists(), "the rest went on");
    let (s, _) = screen(&wz, 100, 30);
    assert!(s.contains("✗") && s.contains("failed"), "{s}");
    let all: String = view::summary(&wz).iter().map(|l| format!("{l}\n")).collect();
    assert!(all.contains("✗ ✧ gemini"), "{all}");
}

#[test]
fn nothing_runs_while_changes_are_made() {
    let f = Fixture::new();
    let mut wz = f.wizard(Command);
    let _ = wz.key(key(KeyCode::Enter));
    let e = wz.key(key(KeyCode::Enter));
    assert!(matches!(e, Some(Effect::Apply { .. })));
    assert!(wz.running());
    assert_eq!(wz.key(key(KeyCode::Char('q'))), None);
    wz.key(ctrl('c'));
    assert_eq!(wz.ended, None, "not while files are written");
}

#[test]
fn wrap_breaks_between_words_and_inside_one_too_long() {
    assert_eq!(view::wrap("a bb ccc", 4), ["a bb", "ccc"]);
    assert_eq!(view::wrap("abcdefgh", 3), ["abc", "def", "gh"]);
    assert_eq!(view::wrap("", 3), Vec::<String>::new());
}

#[test]
fn the_summary_makes_the_guide_a_link() {
    let line = ratatui::text::Line::raw(crate::guide::MESSAGING_GUIDE);
    let a = view::ansi(&line);
    assert!(a.starts_with("\x1b]8;;https://"), "{a:?}");
    assert!(a.contains(crate::guide::MESSAGING_GUIDE));
}
