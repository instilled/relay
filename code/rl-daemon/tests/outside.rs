//! Claude's own sessions on the digest: `claude agents --json` asked in the
//! background, Relay's own runs left out, one a run was sent to pointed at
//! from that run, the last list kept a while when `claude` stops answering
//! or hangs. The `claude` here is a shell script in a
//! tempdir, first on `PATH`: no real agent, no model.

mod common;

use common::{Conn, Harness, fake_cli};
use rl_core::Home;
use rl_daemon::outside::{Polling, WENT};
use rl_proto::methods::Digest;
use rl_proto::types::Run;
use serde_json::json;
use std::time::{Duration, Instant};

/// Asks for the digest until `ok` holds of it, or fails after a few seconds.
async fn digest_until(c: &mut Conn, what: &str, ok: impl Fn(&Digest) -> bool) -> Digest {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let d: Digest = c.ok("digest.get", json!({})).await;
        if ok(&d) {
            return d;
        }
        assert!(Instant::now() < deadline, "never {what}: {:?}", d.outside);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn claudes_sessions_reach_the_digest_less_relays_own() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let agents = tmp.path().join("agents.json");
    let fail = tmp.path().join("fail");
    let hang = tmp.path().join("hang");
    let script = format!(
        "#!/bin/sh\n[ -e '{fail}' ] && {{ echo 'it broke' >&2; exit 1; }}\n[ -e '{hang}' ] && exec sleep 30\n[ \"$1\" = agents ] || exit 2\ncat '{agents}'\n",
        fail = fail.display(),
        hang = hang.display(),
        agents = agents.display()
    );
    let claude = bin.join("claude");
    std::fs::write(&claude, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&old));
    // SAFETY: the only test in this binary; nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.path()).unwrap();
    let polling = Polling {
        every: Duration::from_millis(100),
        grace: Duration::from_millis(800),
        wait: Duration::from_millis(200),
    };
    let Some(h) = Harness::with_claude(home.clone(), polling) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    // A repo no project registers, with a session in a folder inside it.
    let repo = tmp.path().canonicalize().unwrap().join("repo");
    let sub = repo.join("deep").join("down");
    std::fs::create_dir_all(&sub).unwrap();
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q", "-b", "trunk"])
        .status()
        .unwrap();
    assert!(st.success());

    // A Claude run of Relay's says which session it is: tagged, once.
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent", "title": "ours" }),
        )
        .await;
    for _ in 0..2 {
        let r: Run = c
            .ok(
                "run.hook",
                json!({ "id": run.id, "event": "SessionStart",
                        "payload": { "session_id": "sid-relay", "source": "startup" } }),
            )
            .await;
        assert_eq!(
            r.tags.iter().filter(|t| *t == "claude-session:sid-relay").count(),
            1,
            "{:?}",
            r.tags
        );
    }

    // One that never says which session it is (an assistant's `claude -p`,
    // say) is still Relay's by its pid while it lives.
    let quiet: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 30"], "cwd": cwd, "kind": "agent", "title": "headless", "tty": false }),
        )
        .await;
    let started: Vec<rl_proto::types::Event> = c.ok("event.list", json!({ "run": quiet.id })).await;
    let pid = started
        .iter()
        .find(|e| e.kind == "run.started")
        .and_then(|e| e.data.as_ref()?.get("pid")?.as_u64())
        .expect("run.started names the pid");

    std::fs::write(
        &agents,
        serde_json::to_string(&json!([
            {"pid": 4_000_001, "cwd": cwd, "kind": "interactive", "sessionId": "sid-relay",
             "status": "busy", "startedAt": 1_789_821_153_884_i64},
            {"pid": pid, "cwd": cwd, "kind": "interactive", "sessionId": "sid-headless", "status": "busy"},
            {"pid": 4_000_002, "id": "b1", "cwd": cwd, "kind": "background", "sessionId": "sid-theirs",
             "name": "asset list", "status": "waiting", "waitingFor": "permission prompt", "state": "blocked"},
            {"cwd": cwd, "kind": "background", "state": "done"},
            {"pid": 4_000_003, "cwd": sub, "kind": "interactive", "sessionId": "sid-sub", "status": "idle"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let d = digest_until(&mut c, "listed", |d| !d.outside.is_empty()).await;
    assert_eq!(
        d.outside.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
        ["sid-theirs", "sid-sub"],
        "Relay's own are left out, by tag and by pid; the one with no sessionId skipped"
    );
    let s = &d.outside[0];
    assert_eq!(
        (s.agent.as_str(), s.name.as_str(), s.pid),
        ("claude", "asset list", Some(4_000_002))
    );
    assert_eq!(s.waiting_for.as_deref(), Some("permission prompt"));
    // Each is filed as a run there would be: a folder in no repo and no
    // project by itself, one inside an unregistered repo under its root.
    assert_eq!(s.workspace, Some(tmp.path().canonicalize().unwrap()));
    assert_eq!(d.outside[1].workspace.as_ref(), Some(&repo));
    let first = s.state_at.expect("since when it stands so");

    // It stands as it did: since when is kept. It moves on: since now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d = digest_until(&mut c, "heard again", |d| d.outside.len() == 2).await;
    assert_eq!(d.outside[0].state_at, Some(first), "unchanged, kept");
    let text = std::fs::read_to_string(&agents)
        .unwrap()
        .replace("\"blocked\"", "\"working\"");
    std::fs::write(&agents, text).unwrap();
    let d = digest_until(&mut c, "working", |d| {
        d.outside.first().is_some_and(|s| s.state.as_deref() == Some("working"))
    })
    .await;
    assert!(d.outside[0].state_at.is_some_and(|t| t > first), "{:?}", d.outside[0]);

    // `claude` stops answering: the last list stands a while, then goes.
    std::fs::write(&fail, "").unwrap();
    let broke = Instant::now();
    let d = digest_until(&mut c, "cleared", |d| d.outside.is_empty()).await;
    assert!(
        broke.elapsed() >= Duration::from_millis(500),
        "kept through the grace period"
    );
    assert!(d.outside.is_empty());
    // And the daemon is still there.
    let _: serde_json::Value = c.ok("daemon.ping", json!(null)).await;
    std::fs::remove_file(&fail).unwrap();
    digest_until(&mut c, "listed again", |d| !d.outside.is_empty()).await;

    // A run of Relay's sent to Claude's background (`/background`): it
    // ended here, and its conversation goes on there, started after it.
    let went: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent", "title": "fix the build" }),
        )
        .await;
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": went.id, "event": "SessionStart",
                    "payload": { "session_id": "sid-went", "source": "startup" } }),
        )
        .await;
    let _ = c.call("run.stop", json!({ "id": went.id })).await;
    c.wait_terminal(&went.id).await;
    let later = jiff::Timestamp::now().as_millisecond() + 1_000;
    std::fs::write(
        &agents,
        serde_json::to_string(&json!([
            {"pid": 4_000_009, "id": "b9", "cwd": cwd, "kind": "background", "sessionId": "sid-went",
             "status": "busy", "state": "working", "startedAt": later}
        ]))
        .unwrap(),
    )
    .unwrap();
    let d = digest_until(&mut c, "went", |d| d.outside.iter().any(|s| s.session_id == "sid-went")).await;
    let s = d.outside.iter().find(|s| s.session_id == "sid-went").unwrap();
    assert_eq!(
        s.was.as_ref().map(|w| (w.run.as_str(), w.title.as_str())),
        Some((went.id.as_str(), "fix the build"))
    );
    assert_eq!(s.run, None);
    let item = d
        .items
        .iter()
        .find(|i| i.run.id == went.id)
        .expect("the ended run is on the digest");
    assert_eq!((item.summary.as_str(), item.recap.as_deref()), (WENT, None));

    // `claude` hangs: every digest still answers at once, from what was
    // last heard, and the list goes once the grace period is over.
    std::fs::write(&hang, "").unwrap();
    let hung = Instant::now();
    loop {
        let asked = Instant::now();
        let d: Digest = c.ok("digest.get", json!({})).await;
        assert!(
            asked.elapsed() < Duration::from_millis(150),
            "a digest waited on claude: {:?}",
            asked.elapsed()
        );
        if d.outside.is_empty() {
            break;
        }
        assert!(
            hung.elapsed() < Duration::from_secs(10),
            "never cleared: {:?}",
            d.outside
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hung.elapsed() >= Duration::from_millis(500),
        "kept through the grace period"
    );
    let _: serde_json::Value = c.ok("daemon.ping", json!(null)).await;

    let _ = c.call("run.stop", json!({ "id": run.id })).await;
    let _ = c.call("run.stop", json!({ "id": quiet.id })).await;
    h.stop().await;
}
