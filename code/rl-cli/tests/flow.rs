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

    // The editor leaves the template; it loads, so it is approved.
    let (ok, o, e) = out(rl(&["flow", "edit", "pulse"]));
    assert!(ok, "{o}{e}");
    assert_eq!(o, "approved pulse");
    let text = std::fs::read_to_string(home.join("flows/pulse/flow.md")).unwrap();
    assert!(
        text.contains("when: every 1h") && text.contains("run: echo hello"),
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
