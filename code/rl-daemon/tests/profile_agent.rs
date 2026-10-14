//! An agent added by its profile alone: `fake`, a `detect/fake.yaml` in the
//! test's Relay home with a `sessions.hooks` block of its own, and no Rust
//! that names it. Its hooks, called as the agent would call them (`rl hook
//! fake <Event>`, the payload on stdin), put its session on the digest as
//! its profile says each event stands; enter on it carries it on as the
//! profile's `resume` says; and under a Relay run of it, the same hooks are
//! that run's signal, as its profile says. That setup offers it, and
//! installs it into its file, is `rl-cli`'s to test (`tui::setup`).

mod common;

use common::{Conn, Harness, fake_cli, rl_bin};
use rl_core::Home;
use rl_proto::methods::Digest;
use rl_proto::types::Run;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// `fake`'s profile: its own events, named as no built-in agent names
/// them, and its own way of resuming. Its binary is the fixture, found by
/// its whole path, and known by its name in a run's argv.
fn profile(bin: &str) -> String {
    format!(
        r#"kind: agent
bins: ["{bin}", "fake-cli"]
resume: ["--carry-on", "{{session}}"]
sessions:
  hooks:
    file: ["$FAKE_HOME/hooks.json"]
    timeout: 1s
    events:
      Hello: started
      Typed: {{ state: working, prompt: true }}
      Asks:
        by: kind
        permission: {{ state: needs, waiting_for: "permission prompt" }}
      Done: idle
      Bye: ended
"#
    )
}

/// `rl hook fake <event>` as the agent runs it, outside any run of
/// Relay's: it says nothing, and succeeds.
async fn hook(rl: &Path, home: &Home, event: &str, payload: serde_json::Value) {
    hook_in(rl, home, None, event, payload).await
}

/// The same, under `run` where there is one (`RELAY_RUN`).
async fn hook_in(rl: &Path, home: &Home, run: Option<&str>, event: &str, payload: serde_json::Value) {
    let mut cmd = tokio::process::Command::new(rl);
    cmd.args(["hook", "fake", event])
        .env("RELAY_HOME", home.path())
        .env_remove("RELAY_RUN")
        .env_remove(rl_detect::user_hooks::ASKING);
    if let Some(id) = run {
        cmd.env("RELAY_RUN", id);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(payload.to_string().as_bytes()).await.unwrap();
    drop(stdin);
    let out = child.wait_with_output().await.unwrap();
    assert!(out.status.success(), "rl hook fake {event}: {:?}", out.status);
    assert!(out.stdout.is_empty(), "a hook prints nothing");
}

/// Asks for the digest until `ok` holds of one of fake's sessions, `sid`
/// (none: it is not there), or fails after a few seconds.
async fn until(c: &mut Conn, sid: &str, ok: impl Fn(Option<&rl_proto::methods::Outside>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let d: Digest = c.ok("digest.get", json!({})).await;
        let s = d.outside.iter().find(|s| s.agent == "fake" && s.session_id == sid);
        if ok(s) {
            return;
        }
        assert!(Instant::now() < deadline, "{sid} never as wanted: {:?}", d.outside);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_agent_added_by_its_profile_alone_is_told_of_and_carried_on() {
    let Some(rl) = rl_bin() else {
        eprintln!("skipping: rl not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(home.detect_dir().join("fake.yaml"), profile(&fake_cli())).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().canonicalize().unwrap().join("app");
    std::fs::create_dir_all(&cwd).unwrap();
    let said = |sid: &str, more: serde_json::Value| {
        let mut p = json!({ "session_id": sid, "cwd": cwd });
        p.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        p
    };
    let stands = |status: &'static str, waiting: Option<&'static str>| {
        move |s: Option<&rl_proto::methods::Outside>| {
            s.is_some_and(|s| s.status == status && s.waiting_for.as_deref() == waiting)
        }
    };

    hook(&rl, &home, "Hello", said("f1", json!({}))).await;
    until(&mut c, "f1", stands("idle", None)).await;
    hook(&rl, &home, "Typed", said("f1", json!({ "prompt": "fix it" }))).await;
    until(&mut c, "f1", stands("busy", None)).await;
    hook(&rl, &home, "Asks", said("f1", json!({ "kind": "permission" }))).await;
    until(&mut c, "f1", stands("waiting", Some("permission prompt"))).await;
    // A value its profile does not list says nothing: it still waits.
    hook(&rl, &home, "Asks", said("f1", json!({ "kind": "chatter" }))).await;
    hook(&rl, &home, "Unheard", said("f1", json!({}))).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(
        d.outside.iter().any(|s| s.session_id == "f1" && s.status == "waiting"),
        "{:?}",
        d.outside
    );
    hook(&rl, &home, "Done", said("f1", json!({}))).await;
    until(&mut c, "f1", stands("idle", None)).await;

    // Another, which ends: off the list.
    hook(&rl, &home, "Hello", said("f2", json!({}))).await;
    until(&mut c, "f2", |s| s.is_some()).await;
    hook(&rl, &home, "Bye", said("f2", json!({}))).await;
    until(&mut c, "f2", |s| s.is_none()).await;

    // Enter on it, once it has been quiet a moment: carried on here, as its
    // profile resumes one.
    tokio::time::sleep(rl_daemon::import::HEARD_QUIET + Duration::from_millis(200)).await;
    let run: Run = c
        .ok("import.move", json!({ "agent": "fake", "session_id": "f1" }))
        .await;
    assert_eq!(run.argv, [fake_cli(), "--carry-on".into(), "f1".into()]);
    assert!(run.tags.iter().any(|t| t == "fake-session:f1"), "{:?}", run.tags);
    assert_eq!(run.cwd, cwd);
    until(&mut c, "f1", |s| s.is_none()).await;

    let _ = c.call("run.stop", json!({ "id": run.id, "force": true })).await;
    h.stop().await;
}

/// The run, as `rld` has it now.
async fn run_now(c: &mut Conn, id: &str) -> Run {
    c.ok("run.get", json!({ "id": id })).await
}

#[tokio::test]
async fn a_relay_run_of_an_agent_added_by_its_profile_alone_is_told_by_its_hooks() {
    let Some(rl) = rl_bin() else {
        eprintln!("skipping: rl not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(home.detect_dir().join("fake.yaml"), profile(&fake_cli())).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().canonicalize().unwrap();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("fake"), "known by its profile");
    let said = |more: serde_json::Value| {
        let mut p = json!({ "session_id": "r1", "cwd": cwd });
        p.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        p
    };
    let id = run.id.as_str();

    // Its start: the run is tagged with its session, which never lists.
    hook_in(&rl, &home, Some(id), "Hello", said(json!({}))).await;
    let r = run_now(&mut c, id).await;
    assert!(r.tags.iter().any(|t| t == "fake-session:r1"), "{:?}", r.tags);
    // A prompt: working, exactly, and named for it.
    hook_in(
        &rl,
        &home,
        Some(id),
        "Typed",
        said(json!({ "prompt": "fix the build" })),
    )
    .await;
    let r = run_now(&mut c, id).await;
    assert_eq!(
        (r.state, r.title.as_str()),
        (rl_proto::types::RunState::Working, "fake: fix the build")
    );
    // A wait on you, then the turn's end.
    hook_in(&rl, &home, Some(id), "Asks", said(json!({ "kind": "permission" }))).await;
    assert_eq!(run_now(&mut c, id).await.state, rl_proto::types::RunState::NeedsInput);
    hook_in(&rl, &home, Some(id), "Done", said(json!({}))).await;
    assert_eq!(run_now(&mut c, id).await.state, rl_proto::types::RunState::Idle);
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(d.outside.iter().all(|s| s.agent != "fake"), "{:?}", d.outside);

    let _ = c.call("run.stop", json!({ "id": id, "force": true })).await;
    h.stop().await;
}
