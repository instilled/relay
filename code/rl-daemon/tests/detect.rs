//! Detection end to end: the PTY floor on real runs, exact hook signals over
//! the socket, a human's `run.mark`, and what a `claude` run is started with.

mod common;

use common::{Conn, Harness, fake_cli, wait_journal};
use rl_core::Home;
use rl_proto::rpc::RpcError;
use rl_proto::types::{Confidence, Event, Run, RunKind, RunState, Source};
use serde_json::json;
use std::time::Duration;

fn start() -> Option<(tempfile::TempDir, Harness)> {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let registry = Harness::quick_registry(&home);
    let h = Harness::with_registry(home, Some(registry))?;
    Some((tmp, h))
}

fn kinds(evs: &[Event]) -> Vec<&str> {
    evs.iter().map(|e| e.kind.as_str()).collect()
}

#[tokio::test]
async fn pty_floor_reads_prompts_and_questions() {
    let Some((tmp, h)) = start() else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    // An "agent" that shows a prompt, asks a question, then (as if answered) works on.
    let script = "printf '> '; sleep 1.2; printf 'Do you want to proceed? [y/N] '; sleep 1.2; printf 'y\\nworking on it\\n'; sleep 0.8";
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", script], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    assert_eq!((run.kind, run.cli.as_deref()), (RunKind::Agent, Some("sh")));
    assert_eq!(
        (run.state, run.source, run.confidence),
        (RunState::Working, Source::Pty, Confidence::Guess)
    );

    let idle = c.wait_state(&run.id, RunState::Idle).await;
    assert_eq!(
        (idle.source, idle.confidence),
        (Source::Pty, Confidence::Likely),
        "a bare prompt is idle~"
    );
    let asked = c.wait_state(&run.id, RunState::NeedsInput).await;
    assert_eq!(asked.confidence, Confidence::Likely);
    let working = c.wait_state(&run.id, RunState::Working).await;
    assert_eq!(working.confidence, Confidence::Guess);
    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Finished);

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    assert_eq!(
        kinds(&evs),
        [
            "run.started",
            "run.state",
            "run.needs_input",
            "run.resumed",
            "run.finished"
        ],
        "{evs:#?}"
    );
    assert_eq!(evs[1].summary, "idle: >");
    assert_eq!(evs[2].summary, "needs input: > Do you want to proceed? [y/N]");
    assert_eq!(evs[2].data.as_ref().unwrap()["confidence"], "likely");
    assert_eq!(evs[3].summary, "resumed (working after needs_input)");
    h.stop().await;
}

#[tokio::test]
async fn commands_go_stale_not_idle() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo start; sleep 1.6; echo late; sleep 0.3"], "cwd": cwd }),
        )
        .await;
    assert_eq!(
        (run.kind, run.cli),
        (RunKind::Command, None),
        "`sh -c` is a command, not a shell"
    );

    let stale = c.wait_state(&run.id, RunState::Stale).await;
    assert_eq!((stale.source, stale.confidence), (Source::Pty, Confidence::Likely));
    let back = c.wait_state(&run.id, RunState::Working).await;
    assert_eq!(
        back.confidence,
        Confidence::Likely,
        "a command printing is working~, never idle"
    );
    c.wait_terminal(&run.id).await;

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    assert_eq!(
        kinds(&evs),
        ["run.started", "run.state", "run.resumed", "run.finished"],
        "{evs:#?}"
    );
    assert!(
        evs[1].summary.starts_with("stale: no output for 1s"),
        "{}",
        evs[1].summary
    );
    h.stop().await;
}

#[tokio::test]
async fn hooks_are_exact_and_the_pty_cannot_undo_them() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    wait_journal(&h.home, &run.id, "> ").await;
    // The floor sees fake-cli's prompt first.
    let idle = c.wait_state(&run.id, RunState::Idle).await;
    assert_eq!(idle.source, Source::Pty);

    let asked: Run = c
        .ok(
            "run.hook",
            json!({
                "id": run.id, "event": "Notification",
                "payload": { "hook_event_name": "Notification", "notification_type": "permission_prompt",
                             "message": "Claude needs your permission to use Bash" }
            }),
        )
        .await;
    assert_eq!(
        (asked.state, asked.source, asked.confidence),
        (RunState::NeedsInput, Source::Hook, Confidence::Exact)
    );
    // Quiet spells and output keep coming from the pty; none of it counts.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let still = c.get(&run.id).await;
    assert_eq!((still.state, still.source), (RunState::NeedsInput, Source::Hook));
    assert_eq!(still.state_at, asked.state_at);

    // An event that says nothing about state changes nothing.
    let same: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Notification", "payload": { "notification_type": "auth_success" } }),
        )
        .await;
    assert_eq!(same.state, RunState::NeedsInput);

    let idle: Run = c
        .ok("run.hook", json!({ "id": run.id, "event": "Stop", "payload": {} }))
        .await;
    assert_eq!((idle.state, idle.source), (RunState::Idle, Source::Hook));

    // A human overrides, and sticks, until another exact source speaks.
    let blocked: Run = c
        .ok(
            "run.mark",
            json!({ "id": run.id, "mark": "blocked", "note": "waiting on API key" }),
        )
        .await;
    assert_eq!((blocked.state, blocked.source), (RunState::Blocked, Source::Manual));
    let working: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "PreToolUse", "payload": { "tool_name": "Bash" } }),
        )
        .await;
    assert_eq!(working.state, RunState::Working);
    let done: Run = c.ok("run.mark", json!({ "id": run.id, "mark": "done" })).await;
    assert_eq!(
        (done.state, done.source, done.confidence),
        (RunState::Idle, Source::Manual, Confidence::Exact)
    );

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    assert_eq!(
        kinds(&evs),
        [
            "run.started",
            "run.state",
            "run.needs_input",
            "run.resumed",
            "run.blocked",
            "run.resumed",
            "run.state"
        ],
        "{evs:#?}"
    );
    assert_eq!(evs[2].summary, "needs input: Claude needs your permission to use Bash");
    assert_eq!(evs[2].data.as_ref().unwrap()["source"], "hook");
    assert_eq!(evs[4].summary, "blocked: waiting on API key");
    assert_eq!(evs[6].summary, "idle: marked done");

    let bad = c.call("run.hook", json!({ "id": "nope0000", "event": "Stop" })).await;
    assert_eq!(bad.error.unwrap().code, RpcError::INTERNAL);
    let bad = c.call("run.mark", json!({ "id": "nope0000", "mark": "done" })).await;
    assert_eq!(bad.error.unwrap().code, RpcError::INTERNAL);
    let bad = c
        .call("run.hook", json!({ "id": run.id, "dialect": "aider", "event": "Stop" }))
        .await;
    assert_eq!(bad.error.unwrap().code, RpcError::INVALID_PARAMS);

    c.ok::<Run>("run.stop", json!({ "id": run.id })).await;
    let ended = c.wait_terminal(&run.id).await;
    assert_eq!(ended.state, RunState::Stopped);
    // Terminal is final: no signal moves it.
    let after: Run = c.ok("run.mark", json!({ "id": run.id, "mark": "working" })).await;
    assert_eq!(after.state, RunState::Stopped);
    h.stop().await;
}

#[tokio::test]
async fn an_agent_nobody_named_is_named_for_its_first_prompt_once() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let start = |title: Option<&str>| {
        let mut spec = json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" });
        if let Some(t) = title {
            spec["title"] = json!(t);
        }
        spec
    };
    let prompt = |id: &str, p: &str| {
        json!({ "id": id, "event": "UserPromptSubmit",
                "payload": { "hook_event_name": "UserPromptSubmit", "prompt": p } })
    };
    let unnamed: Run = c.ok("run.start", start(None)).await;
    let named: Run = c.ok("run.start", start(Some("the importer"))).await;
    let renamed: Run = c.ok("run.start", start(None)).await;
    // Until it is asked anything, it is what was run.
    assert!(
        unnamed.title.ends_with("fake-cli") || unnamed.title.ends_with('…'),
        "{}",
        unnamed.title
    );

    // A slash command says how to run, not what for: no name yet.
    let r: Run = c.ok("run.hook", prompt(&unnamed.id, "/model opus")).await;
    assert_eq!(r.title, unnamed.title);
    // The first real prompt names it, by its first line.
    let r: Run = c
        .ok("run.hook", prompt(&unnamed.id, "add a recipe import\nfrom any url"))
        .await;
    assert_eq!(r.title, "fake-cli: add a recipe import");
    assert_eq!(r.state, RunState::Working, "the hook still says what it is doing");
    // Later prompts never rename it.
    let r: Run = c.ok("run.hook", prompt(&unnamed.id, "now the tests")).await;
    assert_eq!(r.title, "fake-cli: add a recipe import");
    // The list reads the new name like any rename.
    let d: rl_proto::methods::Digest = c.ok("digest.get", json!({})).await;
    let item = d.items.iter().find(|i| i.run.id == unnamed.id).unwrap();
    assert_eq!(item.run.title, "fake-cli: add a recipe import");

    // A run you named keeps its name.
    let r: Run = c.ok("run.hook", prompt(&named.id, "add a recipe import")).await;
    assert_eq!(r.title, "the importer");

    // A rename by hand wins, before the first prompt and after it.
    c.ok::<Run>("run.rename", json!({ "id": renamed.id, "title": "mine" }))
        .await;
    let r: Run = c.ok("run.hook", prompt(&renamed.id, "add a recipe import")).await;
    assert_eq!(r.title, "mine");
    c.ok::<Run>("run.rename", json!({ "id": unnamed.id, "title": "recipes" }))
        .await;
    let r: Run = c.ok("run.hook", prompt(&unnamed.id, "and more")).await;
    assert_eq!(r.title, "recipes");

    // An empty name takes a rename back, to the name Relay gave the run: what
    // its first prompt named it, or what it started as — and one renamed
    // before it was asked anything is still named for the next prompt.
    let r: Run = c.ok("run.rename", json!({ "id": unnamed.id, "title": "" })).await;
    assert_eq!(r.title, "fake-cli: add a recipe import");
    let r: Run = c.ok("run.rename", json!({ "id": renamed.id, "title": "  " })).await;
    assert_eq!(r.title, renamed.title);
    let r: Run = c.ok("run.hook", prompt(&renamed.id, "fix the importer")).await;
    assert_eq!(r.title, "fake-cli: fix the importer");
    c.ok::<Run>("run.rename", json!({ "id": named.id, "title": "x" })).await;
    let r: Run = c.ok("run.rename", json!({ "id": named.id, "title": "" })).await;
    assert_eq!(r.title, "the importer");

    for id in [&unnamed.id, &named.id, &renamed.id] {
        c.ok::<Run>("run.stop", json!({ "id": id })).await;
        c.wait_terminal(id).await;
    }
    h.stop().await;
}

#[tokio::test]
async fn claude_runs_get_the_hooks_settings_file() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    // A stand-in `claude` that prints what it was called with.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\necho \"argv: $*\"\n").unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();

    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [claude.to_string_lossy(), "-p", "say hi"], "cwd": cwd }),
        )
        .await;
    assert_eq!(
        (run.kind, run.cli.as_deref()),
        (RunKind::Agent, Some("claude")),
        "detected without --agent"
    );
    assert_eq!(
        run.title,
        format!("{} -p say hi", claude.display()),
        "title is what the user typed"
    );
    let settings = h.home.hooks_dir().join("claude.json");
    assert_eq!(
        run.argv[1..3],
        ["--settings".to_string(), settings.to_string_lossy().into_owned()]
    );
    assert_eq!(run.argv[3..], ["-p".to_string(), "say hi".to_string()]);
    let out = wait_journal(&h.home, &run.id, "argv: --settings").await;
    assert!(out.contains("-p say hi"), "{out:?}");
    c.wait_terminal(&run.id).await;

    let text = std::fs::read_to_string(&settings).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    for ev in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Notification",
        "Stop",
    ] {
        let cmd = hooks[ev][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, format!("/usr/local/bin/rl hook claude {ev}"));
    }
    h.stop().await;
}
