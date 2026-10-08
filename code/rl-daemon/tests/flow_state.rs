//! What a step leaves for the steps after it: values by `$RELAY_OUT` and `rl flow state`, files
//! in `$RELAY_WORK`, what it printed under `$RELAY_IN`, and the steps after it reading them.

mod common;
use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, FiringStatus, StepState, StepStatus};
use rl_proto::types::Event;
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

fn waiting(f: &Firing) -> bool {
    f.status == FiringStatus::Waiting
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
