//! What a step leaves for the steps after it: values by `$RELAY_OUT` and `rl flow state`, files
//! in `$RELAY_WORK`, what it printed under `$RELAY_IN`, and the steps after it reading them.

mod common;
use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, FiringStatus, StepState, StepStatus};
use rl_proto::types::{Event, Run};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

fn write(home: &Home, name: &str, files: &[(&str, &str)]) {
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
}

/// A daemon on a fresh home with `flow.md` written for `name`; `None` when `rl-session` is not built.
async fn start(tmp: &Path, name: &str, flow_md: &str) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    write(&home, name, &[("flow.md", flow_md)]);
    let h = Harness::on(home)?;
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing is as `done` says.
async fn wait_firing(c: &mut Conn, flow: &str, done: impl Fn(&Firing) -> bool) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next() {
            if done(&f) {
                return f;
            }
            assert!(Instant::now() < deadline, "the firing never got there: {f:?}");
        } else {
            assert!(Instant::now() < deadline, "`{flow}` never fired");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn ended(f: &Firing) -> bool {
    f.ended_at.is_some()
}

/// Waiting on a question: the step waiting has it.
fn waiting(f: &Firing) -> bool {
    f.status == FiringStatus::Waiting
        && f.steps
            .iter()
            .any(|s| s.status == StepStatus::Waiting && s.decision.is_some())
}

fn step<'a>(f: &'a Firing, name: &str) -> &'a StepState {
    f.steps.iter().find(|s| s.name == name).unwrap()
}

async fn approve_and_run(c: &mut Conn, flow: &str) -> Firing {
    common::approve(c, flow).await;
    c.ok("flow.run", json!({ "name": flow })).await
}

async fn the_question(c: &mut Conn) -> Decision {
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "{open:?}");
    open.into_iter().next().unwrap()
}

#[tokio::test]
async fn a_probes_values_reach_the_if_the_question_and_the_next_command() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  new-errors:\n    \
              run: 'printf \"found=3\\r\\nsignature=abc\" >> \"$RELAY_OUT\"; echo two new errors'\n  \
              triage:\n    after: new-errors\n    if: new-errors.found\n    \
              run: 'echo \"v=$RELAY_V_new_errors_found in=$(cat \"$RELAY_IN/new-errors.out\")\" > \"$RELAY_WORK/seen\"'\n  \
              review:\n    after: triage\n    ask: \"act on {new-errors.signature}?\"\n    \
              group: \"{new-errors.signature}\"\n    options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "logs", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "logs").await;
    let f = wait_firing(&mut c, "logs", waiting).await;
    assert_eq!(
        step(&f, "triage").status,
        StepStatus::Done,
        "if: new-errors.found held: {f:?}"
    );
    let seen = std::fs::read_to_string(h.home.firing_dir("logs", &fired.id).join("seen")).unwrap();
    assert_eq!(
        seen, "v=3 in=two new errors\n",
        "the value by its variable, the out under $RELAY_IN"
    );
    let q = the_question(&mut c).await;
    assert_eq!(q.title, "act on abc?");
    assert_eq!(q.group.as_deref(), Some("abc"));
    let _: Decision = c.ok("decision.take", json!({ "id": q.id, "options": ["ok"] })).await;
    let f = wait_firing(&mut c, "logs", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    h.stop().await;
}

#[tokio::test]
async fn nothing_set_skips_the_if_and_leaves_the_question_ungrouped() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  probe:\n    run: 'true'\n  triage:\n    after: probe\n    if: probe.found\n    \
              run: echo no\n  review:\n    after: probe\n    ask: again?\n    group: \"{probe.signature}\"\n    \
              options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "quiet", md).await else {
        return;
    };
    approve_and_run(&mut c, "quiet").await;
    let f = wait_firing(&mut c, "quiet", waiting).await;
    assert_eq!(step(&f, "triage").status, StepStatus::Skipped, "{f:?}");
    assert_eq!(step(&f, "triage").run, None);
    let q = the_question(&mut c).await;
    assert_eq!(q.group, None, "no value, no group — not the braces");
    let _: Decision = c.ok("decision.take", json!({ "id": q.id, "options": ["ok"] })).await;
    wait_firing(&mut c, "quiet", ended).await;
    h.stop().await;
}

#[tokio::test]
async fn a_bad_line_in_relay_out_fails_the_step_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  probe:\n    run: 'echo \"found=3\" >> \"$RELAY_OUT\"; echo \"not a value\" >> \"$RELAY_OUT\"'\n  \
              next:\n    after: probe\n    run: echo next\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "sloppy", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "sloppy").await;
    let f = wait_firing(&mut c, "sloppy", ended).await;
    assert_eq!(f.status, FiringStatus::Failed, "{f:?}");
    assert_eq!(
        step(&f, "probe").status,
        StepStatus::Failed,
        "its run ended well, but what it said was not kept"
    );
    assert_eq!(step(&f, "next").status, StepStatus::Skipped);
    let events: Vec<Event> = c.ok("event.list", json!({ "kinds": ["flow.refused"] })).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let data = events[0].data.clone().unwrap();
    assert_eq!(data["flow"], "sloppy");
    assert_eq!(data["firing"], fired.id.as_str());
    assert_eq!(data["step"], "probe");
    assert!(data["reason"].as_str().unwrap().contains("line 2"), "{data}");
    h.stop().await;
}

#[tokio::test]
async fn every_firing_says_when_the_last_one_started() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: 'echo \"last=$RELAY_LAST_FIRING.\" > \"$RELAY_WORK/last\"'\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "clock", md).await else {
        return;
    };
    let first = approve_and_run(&mut c, "clock").await;
    wait_firing(&mut c, "clock", ended).await;
    let second: Firing = c.ok("flow.run", json!({ "name": "clock" })).await;
    wait_firing(&mut c, "clock", |f| f.id == second.id && ended(f)).await;
    let last = |id: &str| std::fs::read_to_string(h.home.firing_dir("clock", id).join("last")).unwrap();
    assert_eq!(last(&first.id), "last=.\n", "the first has none");
    assert_eq!(
        last(&second.id),
        format!("last={}.\n", first.started_at),
        "a firing by hand too"
    );
    h.stop().await;
}

#[tokio::test]
async fn state_is_refused_outside_a_step() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), "none", "---\nsteps:\n  a:\n    run: echo a\n---\n").await else {
        return;
    };
    let cwd = tmp.path().to_string_lossy().into_owned();
    let plain: Run = c
        .ok("run.start", json!({ "argv": ["sh", "-c", "sleep 5"], "cwd": cwd }))
        .await;
    for run in [plain.id.as_str(), "nope"] {
        for (method, params) in [
            ("flow.state.set", json!({ "run": run, "values": [["a", "1"]] })),
            ("flow.state.get", json!({ "run": run, "name": "a" })),
            ("flow.state.write", json!({ "run": run, "name": "x", "bytes": [1] })),
            ("flow.state.read", json!({ "run": run, "name": "x" })),
        ] {
            let err = c.call(method, params).await.error.expect(method);
            assert!(
                err.message.contains("not in a flow step"),
                "{method} for {run}: {err:?}"
            );
        }
    }
    let _ = c.call("run.stop", json!({ "id": plain.id })).await;
    h.stop().await;
}

#[tokio::test]
async fn a_running_step_sets_gets_writes_and_reads_through_rld() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: 'while [ ! -e \"$RELAY_WORK/go\" ]; do sleep 0.05; done; echo done'\n  \
              b:\n    after: a\n    ask: \"{a.signature} · {a.out}\"\n    options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "st", md).await else {
        return;
    };
    approve_and_run(&mut c, "st").await;
    let f = wait_firing(&mut c, "st", |f| step(f, "a").run.is_some()).await;
    let run = step(&f, "a").run.clone().unwrap();
    let dir = h.home.firing_dir("st", &f.id);

    let set = c
        .call(
            "flow.state.set",
            json!({ "run": run, "values": [["signature", "abc"], ["found", "3"]] }),
        )
        .await;
    assert!(set.error.is_none(), "{:?}", set.error);
    let get = |name: &str| json!({ "run": run, "name": name });
    let v: Option<String> = c.ok("flow.state.get", get("signature")).await;
    assert_eq!(v.as_deref(), Some("abc"), "a bare key is the step's own");
    let v: Option<String> = c.ok("flow.state.get", get("a.found")).await;
    assert_eq!(v.as_deref(), Some("3"));
    let v: Option<String> = c.ok("flow.state.get", get("a.nope")).await;
    assert_eq!(v, None);

    let wrote = c
        .call(
            "flow.state.write",
            json!({ "run": run, "name": "report.md", "bytes": b"# found\n".to_vec() }),
        )
        .await;
    assert!(wrote.error.is_none(), "{:?}", wrote.error);
    assert_eq!(std::fs::read_to_string(dir.join("report.md")).unwrap(), "# found\n");
    let back: Vec<u8> = c.ok("flow.state.read", get("report.md")).await;
    assert_eq!(back, b"# found\n");

    let big = "x".repeat(4097);
    for (method, params, want) in [
        (
            "flow.state.set",
            json!({ "run": run, "values": [["out", "x"]] }),
            "Relay's",
        ),
        (
            "flow.state.set",
            json!({ "run": run, "values": [["big", big]] }),
            "4 KB",
        ),
        (
            "flow.state.write",
            json!({ "run": run, "name": "../x", "bytes": [1] }),
            "not a file's name",
        ),
        (
            "flow.state.write",
            json!({ "run": run, "name": ".relay", "bytes": [1] }),
            "not a file's name",
        ),
        (
            "flow.state.write",
            json!({ "run": run, "name": "a.out", "bytes": [1] }),
            "what a printed",
        ),
        (
            "flow.state.read",
            json!({ "run": run, "name": "nope.md" }),
            "no `nope.md`",
        ),
        ("flow.state.get", json!({ "run": run, "name": "zz.found" }), "no step"),
    ] {
        let err = c.call(method, params).await.error.expect(method);
        assert!(err.message.contains(want), "{method}: {err:?}");
    }
    assert!(
        !dir.parent().unwrap().join("x").exists(),
        "nothing is written out of the firing's folder"
    );

    std::fs::write(dir.join("go"), "").unwrap();
    wait_firing(&mut c, "st", waiting).await;
    let q = the_question(&mut c).await;
    assert_eq!(q.title, "abc · done");
    // Its run over, the step is not there to ask for anything: a child it left behind cannot write.
    let late = c
        .call("flow.state.set", json!({ "run": run, "values": [["late", "1"]] }))
        .await;
    assert!(late.error.expect("over").message.contains("not in a flow step"));
    let _: Decision = c.ok("decision.take", json!({ "id": q.id, "options": ["ok"] })).await;
    h.stop().await;
}

/// The `rl` a step finds on its `PATH` is rld's link to the one beside it, ahead of the user's
/// own: plain `rl` works in a step, whatever `PATH` rld was started with.
#[tokio::test]
async fn a_step_uses_rl_flow_state_and_the_next_reads_it() {
    let Some(rl) = common::rl_bin() else {
        eprintln!("skipping: rl not built (run `cargo build --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  probe:\n    \
              run: '[ \"$(command -v rl)\" = \"$RELAY_HOME/run/bin/rl\" ] && rl flow state set signature=abc && \
              echo hi | rl flow state write note.txt && echo said'\n  \
              triage:\n    after: probe\n    \
              run: '(rl flow state get probe.signature; rl flow state read note.txt; rl flow state read probe.out) > \"$RELAY_WORK/got\"'\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "uses", md).await else {
        return;
    };
    assert_eq!(
        std::fs::read_link(h.home.run_dir().join("bin").join("rl")).unwrap(),
        rl,
        "the link is to the rl beside rld"
    );
    let fired = approve_and_run(&mut c, "uses").await;
    let f = wait_firing(&mut c, "uses", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let got = std::fs::read_to_string(h.home.firing_dir("uses", &fired.id).join("got")).unwrap();
    assert_eq!(got, "abc\nhi\nsaid", "the value, the file, and what probe printed");
    h.stop().await;
}

/// `rl flow state` says nothing of rld, so what a step printed is the step's own.
#[tokio::test]
async fn rl_flow_state_adds_nothing_to_a_steps_out() {
    if common::rl_bin().is_none() {
        eprintln!("skipping: rl not built (run `cargo build --workspace`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: rl flow state set x=1\n  b:\n    after: a\n    \
              run: 'printf \"[%s]\" \"$(cat \"$RELAY_IN/a.out\")\" > \"$RELAY_WORK/out\"'\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "hush", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "hush").await;
    let f = wait_firing(&mut c, "hush", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let out = std::fs::read_to_string(h.home.firing_dir("hush", &fired.id).join("out")).unwrap();
    assert_eq!(out, "[]", "a's out is empty");
    h.stop().await;
}

#[tokio::test]
async fn a_step_starts_with_every_variable_the_pack_teaches() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: 'env > \"$RELAY_WORK/env\"'\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "envs", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "envs").await;
    let f = wait_firing(&mut c, "envs", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let env = std::fs::read_to_string(h.home.firing_dir("envs", &fired.id).join("env")).unwrap();
    for (name, _) in rl_flow::state::STEP_ENV {
        assert!(
            env.lines().any(|l| l.starts_with(&format!("{name}="))),
            "{name} is not set:\n{env}"
        );
    }
    h.stop().await;
}
