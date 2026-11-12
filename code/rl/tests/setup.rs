//! `rl setup --check`: what a run would change, and the exit status that
//! says whether anything does. Codex only: its entries are files, so no
//! harness CLI is needed.

use std::process::{Command, Output};

fn rl(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rl"))
        .args(args)
        .env("HOME", home)
        .env("RELAY_HOME", home.join(".relay"))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("RL_SESSION")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn check_says_what_setup_would_change_and_exits_1_until_it_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let check = rl(home, &["setup", "--check", "--harness", "codex"]);
    assert_eq!(check.status.code(), Some(1), "{check:?}");
    assert_eq!(stdout(&check), "codex: hooks missing · mcp relay missing\n");
    assert!(!home.join(".codex").exists(), "check writes nothing");

    let run = rl(home, &["setup", "--harness", "codex"]);
    assert!(run.status.success(), "{run:?}");
    assert!(stdout(&run).starts_with("codex: hooks installed · mcp relay registered"));

    let check = rl(home, &["setup", "--check", "--harness", "codex"]);
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(stdout(&check), "all set up\n");
}

#[test]
fn check_and_remove_do_not_go_together() {
    let tmp = tempfile::tempdir().unwrap();
    let o = rl(tmp.path(), &["setup", "--check", "--remove"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}

#[test]
fn yes_sets_up_without_asking_in_plain_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let run = rl(home, &["setup", "--yes", "--harness", "codex"]);
    assert!(run.status.success(), "{run:?}");
    assert!(stdout(&run).starts_with("codex: hooks installed · mcp relay registered"));
    let o = rl(home, &["setup", "--yes", "--check"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}

#[test]
fn a_declined_change_is_not_made_and_check_says_how_many() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".relay")).unwrap();
    std::fs::write(
        home.join(".relay/config.json"),
        r#"{"setup": {"declined": ["codex/mcp"]}}"#,
    )
    .unwrap();
    let check = rl(home, &["setup", "--check", "--harness", "codex"]);
    assert_eq!(
        stdout(&check),
        "codex: hooks missing\n(1 declined, rl setup to change)\n"
    );
    let run = rl(home, &["setup", "--yes", "--harness", "codex"]);
    assert!(stdout(&run).starts_with("codex: hooks installed\n"), "{run:?}");
    assert!(!home.join(".codex/config.toml").exists(), "declined: not made");
    let check = rl(home, &["setup", "--check", "--harness", "codex"]);
    assert_eq!(check.status.code(), Some(0));
    assert_eq!(stdout(&check), "all set up\n(1 declined, rl setup to change)\n");
}
