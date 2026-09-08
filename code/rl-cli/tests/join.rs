//! J1's done-when, through the real binaries: an agent is running, `rld` is
//! killed with SIGKILL, a new daemon starts, and `rl join` shows the same
//! session with its scrollback intact — the agent never noticed.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

/// Reads from `r` until `needle` appears (or a deadline), returning all of it.
fn read_until(r: &mut impl Read, needle: &str) -> String {
    let mut seen = String::new();
    let mut buf = [0u8; 1024];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen.contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; seen {seen:?}"
        );
        match r.read(&mut buf) {
            Ok(0) => panic!("eof waiting for {needle:?}; seen {seen:?}"),
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) => panic!("{e}"),
        }
    }
    seen
}

#[test]
fn join_survives_a_daemon_kill() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let base = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_REF")
            .env_remove("RELAY_TAGS");
        c
    };
    let run = |args: &[&str]| {
        let out = base(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    assert!(run(&["daemon", "start"]).0);
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli");
    let (ok, out, err) = run(&["run", "--agent", fake.to_str().unwrap()]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();

    // Something happened before the crash, so scrollback has content.
    {
        let mut join = base(&["join", &id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut stdin = join.stdin.take().unwrap();
        let mut stdout = join.stdout.take().unwrap();
        read_until(&mut stdout, "fake-cli ready");
        stdin.write_all(b"before the crash\n").unwrap();
        read_until(&mut stdout, "echo: before the crash");
        stdin.write_all(&[0x1c]).unwrap(); // ctrl-\
        let st = join.wait().unwrap();
        assert_eq!(st.code(), Some(0));
    }

    // kill -9 the daemon.
    let (_, status, _) = run(&["daemon", "status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&status).unwrap();
    let pid = v["daemon"]["pid"].as_u64().unwrap();
    assert!(
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while run(&["daemon", "status"]).0 {
        assert!(Instant::now() < deadline, "old daemon still answering");
        std::thread::sleep(Duration::from_millis(50));
    }

    // New daemon; same session, intact scrollback, agent never noticed.
    let (ok, _, err) = run(&["daemon", "start"]);
    assert!(ok, "{err}");
    let mut join = base(&["join", &id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = join.stdin.take().unwrap();
    let mut stdout = join.stdout.take().unwrap();
    let seen = read_until(&mut stdout, "echo: before the crash");
    assert!(seen.contains("fake-cli ready"), "scrollback replayed: {seen:?}");
    assert!(seen.contains("[relay] join"), "banner: {seen:?}");
    stdin.write_all(b"after the crash\n").unwrap();
    read_until(&mut stdout, "echo: after the crash");
    stdin.write_all(&[0x1c]).unwrap();
    assert_eq!(join.wait().unwrap().code(), Some(0));

    // Watching (piped) streams until the run ends, and exits with its code.
    let mut watch = base(&["watch", &id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut wout = BufReader::new(watch.stdout.take().unwrap());
    let mut first = String::new();
    wout.read_line(&mut first).unwrap();
    let mut banner = String::new();
    wout.read_line(&mut banner).unwrap();
    assert!(
        banner.contains("[relay] watch") && banner.contains("q to leave"),
        "{first:?} {banner:?}"
    );
    assert!(run(&["stop", &id]).0);
    let st = watch.wait().unwrap();
    assert_eq!(st.code(), Some(143), "watch exits with the run's code");

    // An ended run: scrollback, then the exit code, no daemon-side holder.
    let (ok, out, _) = run(&["watch", &id]);
    assert!(!ok);
    assert!(
        out.contains("echo: after the crash") && out.contains("ended (exit 143)"),
        "{out:?}"
    );

    assert!(run(&["daemon", "stop"]).0);
}
