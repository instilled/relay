//! Claude's own sessions on the digest: `claude agents --json` asked in the
//! background, Relay's own runs left out, one a run was sent to pointed at
//! from that run, the list left as it stands when `claude` stops answering
//! or hangs; and when `claude` is asked at all, which is seldom where its
//! hook tells. The `claude` here is a shell script in a tempdir, the claude
//! profile's binary for the test's daemon, that counts its calls: no real
//! agent, no model.

mod common;

use common::{Conn, Harness, fake_cli};
use rl_core::Home;
use rl_daemon::outside::{Polling, WENT};
use rl_proto::methods::Digest;
use rl_proto::types::Run;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A fake `claude` in `dir`: `claude agents --json` prints `agents.json`
/// there and counts itself in `calls`; `fail` there makes it fail, `hang`
/// makes it hang.
struct FakeClaude {
    bin: PathBuf,
    agents: PathBuf,
    calls: PathBuf,
    fail: PathBuf,
    hang: PathBuf,
}

impl FakeClaude {
    fn in_dir(dir: &Path) -> Self {
        Self::firing(dir, "")
    }

    /// The same, running `then` (shell) on each call, before it answers:
    /// a `claude` that fires a hook of its own, say.
    fn firing(dir: &Path, then: &str) -> Self {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let f = Self {
            bin: bin.join("claude"),
            agents: dir.join("agents.json"),
            calls: dir.join("calls"),
            fail: dir.join("fail"),
            hang: dir.join("hang"),
        };
        let script = format!(
            "#!/bin/sh\necho . >> '{calls}'\n{then}\n[ -e '{fail}' ] && {{ echo 'it broke' >&2; exit 1; }}\n[ -e '{hang}' ] && exec sleep 30\n[ \"$1\" = agents ] || exit 2\ncat '{agents}'\n",
            calls = f.calls.display(),
            fail = f.fail.display(),
            hang = f.hang.display(),
            agents = f.agents.display()
        );
        std::fs::write(&f.bin, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f.bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f.list(json!([]));
        f
    }

    fn list(&self, sessions: serde_json::Value) {
        std::fs::write(&self.agents, serde_json::to_string(&sessions).unwrap()).unwrap();
    }

    /// How many times it was called.
    fn calls(&self) -> usize {
        std::fs::read_to_string(&self.calls).map_or(0, |t| t.lines().count())
    }
}

/// Asks for the digest until `ok` holds of it, or fails after a few seconds.
async fn digest_until(c: &mut Conn, what: &str, ok: impl Fn(&Digest) -> bool) -> Digest {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let d: Digest = c.ok("digest.get", json!({ "watching": true })).await;
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
    let fake = FakeClaude::in_dir(tmp.path());
    let (agents, fail, hang) = (fake.agents.clone(), fake.fail.clone(), fake.hang.clone());

    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.path()).unwrap();
    // No hook of Claude's: asked every 100ms while the list shows.
    let polling = Polling {
        unhooked: Duration::from_millis(100),
        tick: Duration::from_millis(20),
        wait: Duration::from_millis(200),
        settings: Some(tmp.path().join("claude-config/settings.json")),
        ..Polling::default()
    };
    let Some(h) = Harness::with_claude(home.clone(), &fake.bin, polling) else {
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
            {"pid": 4_000_003, "cwd": sub, "kind": "interactive", "sessionId": "sid-within", "status": "idle"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let d = digest_until(&mut c, "listed", |d| !d.outside.is_empty()).await;
    assert_eq!(
        d.outside.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
        ["sid-theirs", "sid-within"],
        "Relay's own are left out, by tag and by pid; the one with no sessionId skipped; by key"
    );
    let s = &d.outside[0];
    assert_eq!(
        (s.agent.as_str(), s.name.as_str(), s.pid),
        ("claude", "asset list", None),
        "a process that is not there is not kept"
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

    // `claude` stops answering: the list stands as it was.
    std::fs::write(&fail, "").unwrap();
    let broke = fake.calls();
    while fake.calls() < broke + 3 {
        let d: Digest = c.ok("digest.get", json!({ "watching": true })).await;
        assert_eq!(d.outside.len(), 2, "{:?}", d.outside);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // And the daemon is still there.
    let _: serde_json::Value = c.ok("daemon.ping", json!(null)).await;
    std::fs::remove_file(&fail).unwrap();
    // One no longer listed goes.
    let text = std::fs::read_to_string(&agents).unwrap();
    let mut listed: serde_json::Value = serde_json::from_str(&text).unwrap();
    listed
        .as_array_mut()
        .unwrap()
        .retain(|s| s["sessionId"] != "sid-within");
    fake.list(listed);
    digest_until(&mut c, "one gone", |d| d.outside.len() == 1).await;

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
    // last heard, which stands.
    std::fs::write(&hang, "").unwrap();
    let hung = fake.calls();
    while fake.calls() < hung + 2 {
        let asked = Instant::now();
        let d: Digest = c.ok("digest.get", json!({ "watching": true })).await;
        assert!(
            asked.elapsed() < Duration::from_millis(150),
            "a digest waited on claude: {:?}",
            asked.elapsed()
        );
        assert!(d.outside.iter().any(|s| s.session_id == "sid-went"));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _: serde_json::Value = c.ok("daemon.ping", json!(null)).await;

    let _ = c.call("run.stop", json!({ "id": run.id })).await;
    let _ = c.call("run.stop", json!({ "id": quiet.id })).await;
    h.stop().await;
}

/// Quick intervals, Claude's settings in `dir`, its hook there or not.
fn quick(dir: &Path, hooked: bool) -> Polling {
    let settings = dir.join("claude-config/settings.json");
    if hooked {
        rl_detect::user_hooks::install(&settings, "claude", Path::new("/x/rl")).unwrap();
    }
    Polling {
        tick: Duration::from_millis(20),
        between: Duration::from_millis(100),
        background: Duration::from_millis(500),
        unhooked: Duration::from_millis(300),
        recheck: Duration::from_millis(100),
        wait: Duration::from_secs(2),
        settings: Some(settings),
    }
}

/// A daemon asking the fake `claude` in `tmp` as `quick` says.
async fn asking(tmp: &Path, hooked: bool) -> Option<(FakeClaude, Harness, Conn)> {
    asking_of(FakeClaude::in_dir(tmp), tmp, hooked).await
}

async fn asking_of(fake: FakeClaude, tmp: &Path, hooked: bool) -> Option<(FakeClaude, Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    std::fs::create_dir_all(home.path()).unwrap();
    let Some(h) = Harness::with_claude(home.clone(), &fake.bin, quick(tmp, hooked)) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return None;
    };
    let c = Conn::open(&home).await;
    Some((fake, h, c))
}

/// Asks for the digest every `every`, `n` times: the list showing.
async fn show(c: &mut Conn, n: usize, every: Duration) -> Digest {
    let mut d: Digest = c.ok("digest.get", json!({ "watching": true })).await;
    for _ in 1..n {
        tokio::time::sleep(every).await;
        d = c.ok("digest.get", json!({ "watching": true })).await;
    }
    d
}

/// What Claude's user-level hook posts for `event` of session `sid`.
fn hook(sid: &str, event: &str, cwd: &Path) -> serde_json::Value {
    json!({ "agent": "claude", "session_id": sid, "cwd": cwd, "event": event })
}

#[tokio::test]
async fn with_its_hook_there_claude_is_asked_once_and_again_only_for_a_session_it_did_not_list() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((fake, h, mut c)) = asking(tmp.path(), true).await else {
        return;
    };
    let cwd = tmp.path().canonicalize().unwrap();
    fake.list(json!([
        {"cwd": cwd, "kind": "interactive", "sessionId": "s-old", "status": "idle", "name": "from before"}
    ]));
    // Ten digests over a second (the panel's every five seconds, quickened):
    // one ask, for the list first wanted.
    let d = show(&mut c, 10, Duration::from_millis(100)).await;
    assert_eq!(fake.calls(), 1);
    assert_eq!(
        d.outside.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["from before"],
        "one begun before the hook, found by asking"
    );

    // A hook tells of one not listed: asked once more, to fill it in.
    fake.list(json!([
        {"cwd": cwd, "kind": "interactive", "sessionId": "s-old", "status": "idle", "name": "from before"},
        {"cwd": cwd, "kind": "interactive", "sessionId": "s-new", "status": "busy", "name": "fresh",
         "pid": std::process::id(), "startedAt": 1_789_821_153_884_i64}
    ]));
    let _: serde_json::Value = c.ok("outside.hook", hook("s-new", "SessionStart", &cwd)).await;
    let _: serde_json::Value = c.ok("outside.hook", hook("s-new", "UserPromptSubmit", &cwd)).await;
    let d = show(&mut c, 10, Duration::from_millis(100)).await;
    assert_eq!(fake.calls(), 2, "one more, for both words");
    let s = d.outside.iter().find(|s| s.session_id == "s-new").expect("told");
    assert_eq!(
        (s.name.as_str(), s.pid, s.status.as_str()),
        ("fresh", Some(std::process::id()), "busy"),
        "filled in by the ask"
    );
    assert!(s.started_at.is_some());
    // Its words go on coming: nothing more is asked.
    let _: serde_json::Value = c.ok("outside.hook", hook("s-new", "Stop", &cwd)).await;
    show(&mut c, 5, Duration::from_millis(100)).await;
    assert_eq!(fake.calls(), 2);
    // Its end takes it off, ask or no ask.
    let _: serde_json::Value = c.ok("outside.hook", hook("s-new", "SessionEnd", &cwd)).await;
    let d = show(&mut c, 3, Duration::from_millis(100)).await;
    assert!(!d.outside.iter().any(|s| s.session_id == "s-new"), "{:?}", d.outside);
    h.stop().await;
}

#[tokio::test]
async fn a_digest_that_is_not_watching_asks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((fake, h, mut c)) = asking(tmp.path(), true).await else {
        return;
    };
    for _ in 0..5 {
        let _: Digest = c.ok("digest.get", json!({})).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(fake.calls(), 0, "a one-off digest only reads");
    // The panel's, watching, is what has it asked.
    show(&mut c, 3, Duration::from_millis(100)).await;
    assert_eq!(fake.calls(), 1);
    h.stop().await;
}

#[tokio::test]
async fn a_hook_is_answered_with_wanting_the_pid_only_while_none_is_known() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((_fake, h, mut c)) = asking(tmp.path(), true).await else {
        return;
    };
    let cwd = tmp.path().canonicalize().unwrap();
    let wants = |r: &serde_json::Value| r["wants_pid"].as_bool().unwrap();
    let said: serde_json::Value = c.ok("outside.hook", hook("s-pid", "SessionStart", &cwd)).await;
    assert!(wants(&said), "no process known yet");
    let mut with = hook("s-pid", "SessionStart", &cwd);
    with["pid"] = json!(std::process::id());
    let said: serde_json::Value = c.ok("outside.hook", with).await;
    assert!(!wants(&said), "told it");
    let said: serde_json::Value = c.ok("outside.hook", hook("s-pid", "PermissionRequest", &cwd)).await;
    assert!(!wants(&said), "known now, and kept");
    let said: serde_json::Value = c.ok("outside.hook", hook("s-pid", "SessionEnd", &cwd)).await;
    assert!(!wants(&said), "over");
    h.stop().await;
}

#[tokio::test]
async fn a_background_session_listed_has_claude_asked_now_and_then() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((fake, h, mut c)) = asking(tmp.path(), true).await else {
        return;
    };
    let cwd = tmp.path().canonicalize().unwrap();
    // Told by its hook first, then filled in: in Claude's background.
    let _: serde_json::Value = c.ok("outside.hook", hook("s-bg", "UserPromptSubmit", &cwd)).await;
    fake.list(json!([
        {"id": "bg1", "cwd": cwd, "kind": "background", "sessionId": "s-bg", "status": "busy", "state": "working"}
    ]));
    let started = Instant::now();
    let d = show(&mut c, 26, Duration::from_millis(100)).await;
    let s = d.outside.iter().find(|s| s.session_id == "s-bg").unwrap();
    assert_eq!((s.id.as_deref(), s.kind.as_str()), (Some("bg1"), "background"));
    // At once, then every 500ms: as many as the time taken allows, give
    // or take one.
    let most = (started.elapsed().as_millis() / 500) as usize + 2;
    let calls = fake.calls();
    assert!((3..=most).contains(&calls), "{calls} asks in {:?}", started.elapsed());
    h.stop().await;
}

#[tokio::test]
async fn without_its_hook_claude_is_asked_every_little_while_but_only_while_showing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((fake, h, mut c)) = asking(tmp.path(), false).await else {
        return;
    };
    // Nothing showing: nothing asked.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.calls(), 0);
    // Showing for 1.5s: at once, then every 300ms — some five or six.
    let started = Instant::now();
    show(&mut c, 15, Duration::from_millis(100)).await;
    let most = (started.elapsed().as_millis() / 300) as usize + 2;
    let calls = fake.calls();
    assert!((4..=most).contains(&calls), "{calls} asks in {:?}", started.elapsed());
    h.stop().await;
}

#[tokio::test]
async fn a_hook_that_claude_agents_fires_itself_is_not_a_session_and_asks_nothing() {
    let Some(rl) = common::rl_bin() else {
        eprintln!("skipping: rl not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().canonicalize().unwrap();
    // Each `claude agents` fires Claude's user-level hook, as a session
    // that is new each time would.
    let fire = |env: &str| {
        format!(
            "echo '{{\"session_id\":\"s-'$$'\",\"cwd\":\"{cwd}\"}}' | {env} RELAY_HOME='{home}' '{rl}' hook claude SessionStart --user",
            cwd = cwd.display(),
            home = home.display(),
            rl = rl.display()
        )
    };
    let fake = FakeClaude::firing(tmp.path(), &fire(""));
    let Some((fake, h, mut c)) = asking_of(fake, tmp.path(), true).await else {
        return;
    };
    let d = show(&mut c, 10, Duration::from_millis(100)).await;
    assert_eq!(fake.calls(), 1, "asked once, as the list was first wanted");
    assert!(d.outside.is_empty(), "{:?}", d.outside);

    // The same hook from any other claude tells of its session.
    let st = std::process::Command::new("sh")
        .args(["-c", &fire("env -u RELAY_ASKING")])
        .status()
        .unwrap();
    assert!(st.success());
    let d = show(&mut c, 3, Duration::from_millis(100)).await;
    assert_eq!(d.outside.len(), 1, "{:?}", d.outside);
    h.stop().await;
}
