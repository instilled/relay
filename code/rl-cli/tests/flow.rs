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

    // Taken away before the daemon is reached: one that cannot be started keeps nothing either.
    for verb in ["new", "edit"] {
        let mut c = rl(&["flow", verb, "nodaemon"]);
        c.env("RELAY_RLD_BIN", dir.join("no-rld"));
        let (ok, o, e) = out(c);
        assert!(ok, "{o}{e}");
        assert_eq!(o, "nodaemon left as the template: not kept");
        assert!(!home.join("flows/nodaemon").exists());
    }

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

    // A folder holding a script of the person's: the template goes, the script stays, by either
    // way in.
    let mine = home.join("flows/mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("probe.sh"), "echo mine\n").unwrap();
    for args in [["flow", "new", "mine"], ["flow", "edit", "mine"]] {
        let (ok, o, e) = out(rl(&args));
        assert!(ok, "{o}{e}");
        assert_eq!(o, "mine left as the template: not kept");
        assert!(!mine.join("flow.md").exists(), "{args:?}");
        assert_eq!(std::fs::read_to_string(mine.join("probe.sh")).unwrap(), "echo mine\n");
    }

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
    // Too short a prefix says too little of what was looked at.
    let (ok, o, e) = out(rl(&["flow", "approve", "prod", &now[..5]]));
    assert!(!ok, "{o}{e}");
    assert!(e.contains("at least 6"), "{e}");
    let (ok, o, e) = out(rl(&["flow", "approve", "prod", &now[..6].to_uppercase()]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, format!("approved prod · {now}"));
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.contains("  on  "), "{listed}");

    assert!(out(rl(&["daemon", "stop"])).0);
}

#[test]
fn flow_state_outside_a_step_is_refused_without_starting_rld() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for args in [
        &["flow", "state", "set", "a=1"][..],
        &["flow", "state", "get", "a"],
        &["flow", "state", "read", "x"],
        &["flow", "state", "write", "x"],
    ] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .env("RELAY_HOME", &home)
            .env_remove("RELAY_RUN")
            .env_remove("RELAY_FIRING")
            .env_remove("RELAY_STEP")
            .stdin(std::process::Stdio::null());
        let (ok, _, e) = out(c);
        assert!(!ok, "{args:?}");
        assert!(e.contains("not in a flow step"), "{args:?}: {e}");
        assert!(!e.contains("rld started"), "nothing to start rld for: {e}");
    }
}

#[test]
fn flow_new_opens_the_bare_template_and_says_yes() {
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
    let flows = home.join("flows");

    // The bare template: not kept unless changed, and --go has nothing to write it from.
    let (ok, o, _) = out(rl(&["flow", "new", "pulse"]));
    assert!(ok && o == "pulse left as the template: not kept", "{o}");
    assert!(!flows.join("pulse").exists());
    let (ok, _, e) = out(rl(&["flow", "new", "pulse", "--go"]));
    assert!(!ok && e.contains("the bare template needs $EDITOR"), "{e}");
    assert!(!flows.join("pulse").exists());

    // An editor that fails leaves nothing of a new flow behind.
    let mut failing = rl(&["flow", "new", "pulse"]);
    failing.env("EDITOR", "false");
    assert!(!out(failing).0);
    assert!(!flows.join("pulse").exists());

    // Saved changed: it loads, and gets its yes.
    let mut edit = rl(&["flow", "new", "pulse"]);
    edit.env("EDITOR", adds_a_line(&dir));
    let (ok, o, e) = out(edit);
    assert!(ok, "{o}{e}");
    assert_eq!(o, "approved pulse");
    let md = std::fs::read_to_string(flows.join("pulse/flow.md")).unwrap();
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(
        listed
            .lines()
            .any(|l| l.starts_with("pulse") && l.contains("  on  ") && l.contains("every 1h")),
        "{listed}"
    );

    // Its name is taken now: refused, and the flow there is as it was.
    let (ok, _, e) = out(rl(&["flow", "new", "pulse"]));
    assert!(
        !ok && e.contains("`pulse` is a flow already; rl flow edit pulse opens it"),
        "{e}"
    );
    assert_eq!(std::fs::read_to_string(flows.join("pulse/flow.md")).unwrap(), md);

    assert!(out(rl(&["daemon", "stop"])).0);
}

#[test]
fn flow_test_says_what_would_fire_and_starts_nothing() {
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
    let flow = home.join("flows/probe");
    std::fs::create_dir_all(&flow).unwrap();
    std::fs::write(
        flow.join("flow.md"),
        "---\nwhen: every 30m\nsteps:\n  probe:\n    run: echo found\n  triage:\n    after: probe\n    if: probe.out\n    run: echo triage\n---\n",
    )
    .unwrap();

    let (ok, o, e) = out(rl(&["flow", "test", "probe"]));
    assert!(ok, "{o}{e}");
    let lines: Vec<&str> = o.lines().collect();
    assert_eq!(lines[0], "probe · every 30m");
    assert!(lines[1].starts_with("  held    needs a yes"), "{o}");
    assert!(lines[2].starts_with("  next    "), "{o}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("  probe   runs") && l.contains("echo found")),
        "{o}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("  triage  if probe.out") && l.contains("echo triage")),
        "{o}"
    );
    assert_eq!(
        *lines.last().unwrap(),
        "started nothing · rl flow run probe fires it now"
    );

    let (ok, o, _) = out(rl(&["--json", "flow", "test", "probe"]));
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&o).unwrap();
    assert_eq!(v["steps"][1]["runs"], serde_json::json!({ "if": ["probe.out"] }));

    // Nothing fired.
    let (_, listed, _) = out(rl(&["flow", "list"]));
    assert!(listed.lines().nth(1).is_some_and(|l| l.contains("never")), "{listed}");

    let (ok, _, e) = out(rl(&["flow", "test", "nope"]));
    assert!(!ok && e.contains("nope"), "{e}");
    assert!(out(rl(&["daemon", "stop"])).0);
}

#[test]
fn flow_park_lists_and_unparks() {
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
    let flow = home.join("flows/hello");
    std::fs::create_dir_all(&flow).unwrap();
    std::fs::write(flow.join("flow.md"), "---\nsteps:\n  a:\n    run: echo hello\n---\n").unwrap();

    // One run of it, ended, for parking to archive.
    let (ok, o, e) = out(rl(&["flow", "approve", "hello"]));
    assert!(ok, "{o}{e}");
    let (ok, o, e) = out(rl(&["flow", "run", "hello"]));
    assert!(ok, "{o}{e}");
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, listed, _) = out(rl(&["flow", "list"]));
        if listed.lines().nth(1).is_some_and(|l| l.contains("done ")) {
            break;
        }
        assert!(Instant::now() < until, "the firing did not finish:\n{listed}");
        std::thread::sleep(Duration::from_millis(200));
    }

    let (ok, o, e) = out(rl(&["flow", "park", "hello"]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, "hello parked as _hello · 1 run archived");
    assert!(home.join("flows/_hello").is_dir() && !flow.exists());

    let (ok, o, e) = out(rl(&["flow", "parked"]));
    assert!(ok, "{o}{e}");
    assert!(o.starts_with("_hello  hello  ") && o.contains(" ago"), "{o}");
    let (ok, o, _) = out(rl(&["--json", "flow", "parked"]));
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&o).unwrap();
    assert_eq!(v[0]["folder"], "_hello");

    let (ok, o, e) = out(rl(&["flow", "unpark", "hello"]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, "hello back · it needs a yes: rl flow approve hello");
    assert!(flow.join("flow.md").is_file());
    let (ok, o, _) = out(rl(&["flow", "parked"]));
    assert!(ok && o == "nothing parked", "{o}");

    // Nothing parked by that name, or a folder that is not one: said, and exit 1.
    let (ok, _, e) = out(rl(&["flow", "unpark", "hello"]));
    assert!(!ok && e.contains("nothing parked as hello"), "{e}");
    let (ok, _, e) = out(rl(&["flow", "unpark", "_nope"]));
    assert!(!ok && e.contains("_nope is not a parked flow"), "{e}");

    // Back but not loaded: why it did not, with its file and line, and exit 1; as JSON, the answer.
    for json in [false, true] {
        let (ok, o, e) = out(rl(&["flow", "park", "hello"]));
        assert!(ok, "{o}{e}");
        std::fs::write(home.join("flows/_hello/flow.md"), "---\nsteps: [\n---\n").unwrap();
        if json {
            let (ok, o, e) = out(rl(&["--json", "flow", "unpark", "hello"]));
            assert!(ok, "{o}{e}");
            let v: serde_json::Value = serde_json::from_str(&o).unwrap();
            assert_eq!(v["name"], "hello");
            assert!(v.get("flow").is_none(), "{o}");
            assert!(v["problem"].as_str().unwrap().contains("flow.md:"), "{o}");
        } else {
            let (ok, _, e) = out(rl(&["flow", "unpark", "hello"]));
            assert!(!ok, "{e}");
            assert!(
                e.contains("hello is back in flows/hello but did not load: ")
                    && e.contains("flow.md:")
                    && e.contains(" · rl flow edit hello, or rl flow reload"),
                "{e}"
            );
        }
        assert!(flow.join("flow.md").is_file());
        std::fs::write(flow.join("flow.md"), "---\nsteps:\n  a:\n    run: echo hello\n---\n").unwrap();
        let (ok, o, e) = out(rl(&["flow", "reload"]));
        assert!(ok, "{o}{e}");
    }
    assert!(out(rl(&["daemon", "stop"])).0);
}

/// A daemon in this process, on a throwaway home, and a client of it; a flow `pulse` written to
/// it and said yes to, and its `flow.md` as written.
async fn approved_pulse() -> (
    tempfile::TempDir,
    rl_core::Home,
    tokio_util::sync::CancellationToken,
    rl_cli::client::Client,
    String,
) {
    let tmp = tempfile::tempdir().unwrap();
    let home = rl_core::Home::at(tmp.path());
    let token = tokio_util::sync::CancellationToken::new();
    tokio::spawn(rl_daemon::serve_with(
        home.clone(),
        token.clone(),
        rl_daemon::ServeOptions::default(),
    ));
    let mut rpc = loop {
        match rl_cli::client::Client::connect(&home).await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    };
    let base = "---\nwhen: every 1d\nsteps:\n  first:\n    run: echo hello\n---\n".to_string();
    let name = rl_cli::flow::check_name(&home, "pulse").unwrap();
    let files = vec![rl_cli::flow::NewFile::flow_md(base.as_str())];
    rl_cli::flow::write_new(&mut rpc, &home, &name, files, &[])
        .await
        .unwrap();
    (tmp, home, token, rpc, base)
}

#[tokio::test]
async fn write_changed_approves_a_changed_flow() {
    let (_tmp, home, token, mut rpc, base) = approved_pulse().await;
    let text = base.replace("every 1d", "every 2d");
    let edited = rl_cli::flow::write_changed(&mut rpc, &home, "pulse", &base, &text, &[])
        .await
        .unwrap();
    assert!(
        matches!(edited, rl_cli::flow::Edited::Approved(_)),
        "{}",
        edited.words("pulse")
    );
    let md = home.flow_dir("pulse").join("flow.md");
    assert_eq!(std::fs::read_to_string(md).unwrap(), text);
    token.cancel();
}

#[tokio::test]
async fn write_changed_refuses_a_moved_base() {
    let (_tmp, home, token, mut rpc, base) = approved_pulse().await;
    let md = home.flow_dir("pulse").join("flow.md");
    let moved = base.replace("echo hello", "echo moved");
    std::fs::write(&md, &moved).unwrap();
    let text = base.replace("every 1d", "every 2d");
    let Err(e) = rl_cli::flow::write_changed(&mut rpc, &home, "pulse", &base, &text, &[]).await else {
        panic!("written over a flow.md changed since it was asked");
    };
    assert_eq!(
        format!("{e:#}"),
        "pulse's flow.md changed since it was asked · type to ask again"
    );
    assert_eq!(std::fs::read_to_string(&md).unwrap(), moved);
    token.cancel();
}

#[tokio::test]
async fn write_changed_refuses_what_would_not_load() {
    let (_tmp, home, token, mut rpc, base) = approved_pulse().await;
    let text = base.replace("every 1d", "every now and then");
    let Err(e) = rl_cli::flow::write_changed(&mut rpc, &home, "pulse", &base, &text, &[]).await else {
        panic!("written what would not load");
    };
    let e = format!("{e:#}");
    assert!(e.starts_with("pulse would not load: "), "{e}");
    let md = home.flow_dir("pulse").join("flow.md");
    assert_eq!(std::fs::read_to_string(md).unwrap(), base);
    token.cancel();
}

#[tokio::test]
async fn write_changed_puts_back_what_the_daemon_would_not_load() {
    let (tmp, home, token, mut rpc, base) = approved_pulse().await;
    // A project the panel was told of and the daemon was not: it checks, and the daemon says no.
    let elsewhere = std::fs::canonicalize(tmp.path()).unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let text = base.replace("---\nwhen", &format!("---\nin: {}\nwhen", elsewhere.display()));
    let edited = rl_cli::flow::write_changed(&mut rpc, &home, "pulse", &base, &text, &[elsewhere])
        .await
        .unwrap();
    assert!(
        matches!(edited, rl_cli::flow::Edited::NotLoaded(_)),
        "{}",
        edited.words("pulse")
    );
    let md = home.flow_dir("pulse").join("flow.md");
    assert_eq!(std::fs::read_to_string(md).unwrap(), base, "put back");
    // Read back as it was: loaded, with its yes.
    let flows = rpc
        .call::<rl_proto::flow::FlowList>(rl_proto::flow::FlowFilter { all: false })
        .await
        .unwrap();
    let pulse = flows.iter().find(|f| f.name == "pulse").expect("loaded");
    assert!(pulse.approved());
    token.cancel();
}

#[tokio::test]
async fn start_changed_writes_the_change_for_the_editor() {
    let (_tmp, home, token, mut rpc, base) = approved_pulse().await;
    let text = base.replace("every 1d", "every 2d");
    let editing = rl_cli::flow::start_changed(&home, "pulse", &base, &text).unwrap();
    let md = home.flow_dir("pulse").join("flow.md");
    assert_eq!(editing.file, md);
    assert_eq!(std::fs::read_to_string(&md).unwrap(), text);
    // Saved as it was written, it is still a change to what was there: the editor's yes.
    let (edited, _) = rl_cli::flow::finish_edit(&mut rpc, editing.clone()).await.unwrap();
    assert!(
        matches!(edited, rl_cli::flow::Edited::Approved(_)),
        "{}",
        edited.words("pulse")
    );
    // And what it was can be put back.
    rl_cli::flow::restore(&editing, &base).unwrap();
    assert_eq!(std::fs::read_to_string(&md).unwrap(), base);
    token.cancel();
}

#[tokio::test]
async fn start_changed_refuses_a_moved_base() {
    let (_tmp, home, token, _rpc, base) = approved_pulse().await;
    let md = home.flow_dir("pulse").join("flow.md");
    let moved = base.replace("echo hello", "echo moved");
    std::fs::write(&md, &moved).unwrap();
    let Err(e) = rl_cli::flow::start_changed(&home, "pulse", &base, "anything") else {
        panic!("opened over a flow.md changed since it was asked");
    };
    assert_eq!(
        format!("{e:#}"),
        "pulse's flow.md changed since it was asked · type to ask again"
    );
    assert_eq!(std::fs::read_to_string(&md).unwrap(), moved);
    token.cancel();
}
