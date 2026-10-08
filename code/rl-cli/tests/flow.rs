//! `rl flow`: edit writes, loads and approves; a flow runs and the list says
//! how it went; what did not load is said with its file and line.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

/// `rl` against a throwaway home, with `true` for an editor: it leaves the
/// file as it found it.
fn rl(home: &Path, ws: &Path, rld: &Path, session: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
    c.args(args)
        .current_dir(ws)
        .env("RELAY_HOME", home)
        .env("RELAY_RLD_BIN", rld)
        .env("RELAY_SESSION_BIN", session)
        .env("EDITOR", "true")
        .env_remove("VISUAL")
        .env_remove("RELAY_RUN");
    c
}

/// An editor that saves the file with a line of prose added, as `sh <script> +1 <file>`.
fn adds_a_line(dir: &Path) -> String {
    let script = dir.join("editor.sh");
    std::fs::write(&script, "printf '\\nEdited.\\n' >> \"$2\"\n").unwrap();
    format!("sh {}", script.display())
}

fn out(mut c: Command) -> (bool, String, String) {
    let o = c.output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
        String::from_utf8_lossy(&o.stderr).trim().to_string(),
    )
}

#[test]
fn flow_edit_writes_approves_and_runs() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    let ws = dir.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let rl = |args: &[&str]| rl(&home, &ws, &rld, &session, args);

    // The editor leaves the template as it was: nothing is kept, nothing approved.
    let (ok, o, e) = out(rl(&["flow", "edit", "pulse"]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, "pulse left as the template: not kept");
    assert!(!home.join("flows/pulse").exists());

    // Saved, it loads, and is approved.
    let mut edit = rl(&["flow", "edit", "pulse"]);
    edit.env("EDITOR", adds_a_line(&dir));
    let (ok, o, e) = out(edit);
    assert!(ok, "{o}{e}");
    assert_eq!(o, "approved pulse");
    let text = std::fs::read_to_string(home.join("flows/pulse/flow.md")).unwrap();
    assert!(
        text.contains("when: every 1h") && text.contains("run: echo hello") && text.contains("Edited."),
        "{text}"
    );

    let (ok, listed, e) = out(rl(&["flow", "list"]));
    assert!(ok, "{listed}{e}");
    let lines: Vec<&str> = listed.lines().collect();
    assert!(
        ["NAME", "STATUS", "WHEN", "LAST", "IN"]
            .iter()
            .all(|h| lines[0].contains(h)),
        "{listed}"
    );
    assert!(
        lines[1].starts_with("pulse") && lines[1].contains("  on  ") && lines[1].contains("every 1h"),
        "{listed}"
    );
    assert!(lines[1].contains("never"), "{listed}");

    // By hand, and the list comes to say it went well.
    let (ok, o, e) = out(rl(&["flow", "run", "pulse"]));
    assert!(ok, "{o}{e}");
    assert!(o.starts_with("fired pulse · firing "), "{o}");
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, listed, _) = out(rl(&["flow", "list"]));
        if listed.lines().nth(1).is_some_and(|l| l.contains("done ")) {
            break;
        }
        assert!(Instant::now() < until, "the firing did not finish:\n{listed}");
        std::thread::sleep(Duration::from_millis(200));
    }

    // Pause stops it firing by itself, resume lets it; a hand still fires it.
    let (ok, o, _) = out(rl(&["flow", "pause", "pulse"]));
    assert!(ok && o == "paused pulse", "{o}");
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("paused"), "{listed}");
    let (ok, o, e) = out(rl(&["flow", "run", "pulse"]));
    assert!(ok && o.starts_with("fired pulse"), "{o}{e}");
    let (ok, o, _) = out(rl(&["flow", "resume", "pulse"]));
    assert!(ok && o == "resumed pulse", "{o}");
    let (ok, o, _) = out(rl(&["flow", "reload"]));
    assert!(ok && o.starts_with("1 loaded"), "{o}");

    assert!(out(rl(&["daemon", "stop"])).0);
}

#[test]
fn flow_list_says_what_did_not_load() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    let ws = dir.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let rl = |args: &[&str]| rl(&home, &ws, &rld, &session, args);

    // Written by hand: line 3 is not a step.
    let broken = home.join("flows/broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("flow.md"),
        "---\nwhen: every 1h\nstepz:\n  first:\n    run: echo hello\n---\n",
    )
    .unwrap();

    let (ok, listed, e) = out(rl(&["flow", "list"]));
    assert!(ok, "{listed}{e}");
    assert!(listed.contains("not loaded:"), "{listed}");
    assert!(listed.contains("broken · flow.md:3"), "{listed}");

    // An edit that leaves it broken says so and exits 1, with no yes given.
    let (ok, o, e) = out(rl(&["flow", "edit", "broken"]));
    assert!(!ok, "{o}{e}");
    assert!(e.contains("not loaded: broken · flow.md:3"), "{e}");
    assert!(!o.contains("approved"), "{o}");

    assert!(out(rl(&["daemon", "stop"])).0);
}

/// A yes from an edit covers only what was edited: a script changed with no yes before it, or
/// while the editor was open, is not approved with it.
#[test]
fn flow_edit_approves_no_more_than_was_edited() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    let ws = dir.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let rl = |args: &[&str]| rl(&home, &ws, &rld, &session, args);
    let editing = |args: &[&str], editor: &str| {
        let mut c = rl(args);
        c.env("EDITOR", editor);
        c
    };
    let flow = home.join("flows/prod");
    std::fs::create_dir_all(&flow).unwrap();
    std::fs::write(flow.join("flow.md"), "---\nsteps:\n  a:\n    run: sh apply.sh\n---\n").unwrap();
    std::fs::write(flow.join("apply.sh"), "echo as seen\n").unwrap();
    let (ok, o, e) = out(rl(&["flow", "approve", "prod"]));
    assert!(ok && o.starts_with("approved prod"), "{o}{e}");

    // A script changed behind its back; an edit just to look, quit without saving.
    std::fs::write(flow.join("apply.sh"), "echo unseen\n").unwrap();
    let (ok, o, e) = out(rl(&["flow", "edit", "prod"]));
    assert!(ok, "{o}{e}");
    assert_eq!(
        o,
        "flow.md unchanged: no yes given · prod needs one: rl flow approve prod"
    );
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("needs approval"), "{listed}");

    // Saved now, but the change it did not show still has no yes.
    let (ok, o, e) = out(editing(&["flow", "edit", "prod"], &adds_a_line(&dir)));
    assert!(!ok, "{o}{e}");
    assert!(
        e.contains("prod needs a yes: its folder changed since its last yes"),
        "{e}"
    );
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("needs approval"), "{listed}");

    // Looked at and approved, an edit's yes covers the edit.
    let (ok, o, e) = out(rl(&["flow", "approve", "prod"]));
    assert!(ok, "{o}{e}");
    let (ok, o, e) = out(editing(&["flow", "edit", "prod"], &adds_a_line(&dir)));
    assert!(ok && o == "approved prod", "{o}{e}");

    // A script changed while flow.md was open is not covered.
    let sneaky = dir.join("sneaky.sh");
    std::fs::write(
        &sneaky,
        format!(
            "echo sneaked > {}\nprintf '\\nAgain.\\n' >> \"$2\"\n",
            flow.join("apply.sh").display()
        ),
    )
    .unwrap();
    let (ok, o, e) = out(editing(&["flow", "edit", "prod"], &format!("sh {}", sneaky.display())));
    assert!(!ok, "{o}{e}");
    assert!(
        e.contains("prod needs a yes: something else in its folder changed while flow.md was open"),
        "{e}"
    );
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("needs approval"), "{listed}");

    assert!(out(rl(&["daemon", "stop"])).0);
}

/// A new flow's template is dropped however the editor ends, and only what the template was:
/// a `runs/` already there stays.
#[test]
fn a_template_is_dropped_however_the_editor_ends() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    let ws = dir.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let rl = |args: &[&str]| rl(&home, &ws, &rld, &session, args);

    // An editor that fails, as `:cq` does.
    let mut edit = rl(&["flow", "edit", "aborted"]);
    edit.env("EDITOR", "false");
    let (ok, o, e) = out(edit);
    assert!(!ok, "{o}{e}");
    assert!(!home.join("flows/aborted").exists(), "{o}{e}");

    // A folder holding only what a firing left: the template goes, runs/ stays.
    let kept = home.join("flows/kept");
    std::fs::create_dir_all(kept.join("runs/f1")).unwrap();
    std::fs::write(kept.join("runs/f1/probe.out"), "o").unwrap();
    let (ok, o, e) = out(rl(&["flow", "edit", "kept"]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, "kept left as the template: not kept");
    assert!(!kept.join("flow.md").exists());
    assert!(
        kept.join("runs/f1/probe.out").exists(),
        "what a firing left is not the edit's to drop"
    );

    assert!(out(rl(&["daemon", "stop"])).0);
}

/// `rl flow approve` says yes to a version: the list shows each flow's, and a prefix that is
/// not the folder as it is now is refused.
#[test]
fn flow_approve_binds_to_the_version_shown() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    let ws = dir.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let rl = |args: &[&str]| rl(&home, &ws, &rld, &session, args);
    let flow = home.join("flows/prod");
    std::fs::create_dir_all(&flow).unwrap();
    std::fs::write(flow.join("flow.md"), "---\nsteps:\n  a:\n    run: sh apply.sh\n---\n").unwrap();
    std::fs::write(flow.join("apply.sh"), "echo as seen\n").unwrap();

    let (ok, listed, e) = out(rl(&["flow", "list"]));
    assert!(ok && listed.lines().next().unwrap().contains("HASH"), "{listed}{e}");
    let shown = listed
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
    assert!(
        shown.len() == 8 && shown.chars().all(|c| c.is_ascii_hexdigit()),
        "{listed}"
    );

    // Changed after it was looked at: the version shown is gone, and takes no yes.
    std::fs::write(flow.join("apply.sh"), "echo unseen\n").unwrap();
    let (ok, o, e) = out(rl(&["flow", "approve", "prod", &shown]));
    assert!(!ok, "{o}{e}");
    assert!(e.contains(&format!("not {shown}")), "{e}");
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("needs approval"), "{listed}");

    // The version now, by its prefix.
    let now = listed
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
    assert_ne!(now, shown);
    let (ok, o, e) = out(rl(&["flow", "approve", "prod", &now[..6]]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, format!("approved prod · {now}"));
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("  on  "), "{listed}");

    assert!(out(rl(&["daemon", "stop"])).0);
}
