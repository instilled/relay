//! Reactions against a real daemon: a rule in the user's own file fires on a
//! real event and starts a run in the chain that caused it, a rule that asks
//! leaves a question, `where` narrows, and nothing a rule starts feeds itself.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::methods::ReactionTrial;
use rl_proto::reaction::Reaction;
use rl_proto::types::{Cause, Event, Run};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

fn write(home: &Home, yaml: &str) {
    std::fs::write(home.reactions_file(), yaml).unwrap();
}

/// Waits for a file a reaction was supposed to write. Asynchronous on
/// purpose: the daemon shares this test's runtime, so a blocking sleep here
/// would stop the reactor it is waiting for.
async fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text;
        }
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn runs_caused_by(c: &mut Conn, cause: &str) -> Vec<Run> {
    c.ok("run.list", json!({ "cause": cause, "limit": 50 })).await
}

/// The runs a rule started, once there are `want` of them.
///
/// A rule's run reaches the ledger when rld writes the row, which is after
/// the holder is up and the child is already running (`supervisor::start`).
/// So a file the child wrote is not a sign that the row is there yet, and
/// waiting on one to judge the other is a race.
async fn wait_caused_by(c: &mut Conn, cause: &str, want: usize) -> Vec<Run> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let runs = runs_caused_by(c, cause).await;
        if runs.len() >= want {
            return runs;
        }
        assert!(
            Instant::now() < deadline,
            "{cause} started {} runs, wanted {want}",
            runs.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_rule_answers_a_real_event() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    write(
        &home,
        &format!(
            "reactions:\n  \
               tell-me:\n    \
                 on: run.failed\n    \
                 run: 'echo \"{{kind}} {{title}}\" >> told.txt'\n    \
                 cwd: {}\n",
            dir.display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    let listed: Vec<Reaction> = c.ok("reaction.list", json!({})).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].scope, "global");
    assert!(listed[0].approved(), "the user's own file is trusted");
    assert_eq!(listed[0].spec.on, ["run.failed"]);

    // A run fails for real; the rule answers it.
    let failing: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 3"], "cwd": dir, "title": "nightly tests" }),
        )
        .await;
    c.wait_terminal(&failing.id).await;
    let told = wait_for(&dir.join("told.txt")).await;
    assert_eq!(told.trim(), "run.failed nightly tests", "the braces were filled in");

    // The run it started says who started it, and joins the same chain.
    let started = wait_caused_by(&mut c, "reaction:tell-me", 1).await;
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].cause, Cause::Reaction("tell-me".into()));
    assert_eq!(started[0].flow, failing.flow, "the answer belongs to the chain");
    c.wait_terminal(&started[0].id).await;

    // And that run's own events do not set the rule off again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        runs_caused_by(&mut c, "reaction:tell-me").await.len(),
        1,
        "a rule reacted to itself"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_rule_can_ask_instead_of_run() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    write(
        &home,
        &format!(
            "reactions:\n  \
               ask-me:\n    \
                 on: run.failed\n    \
                 ask:\n      \
                   title: '{{title}} failed'\n      \
                   summary: '{{summary}}'\n      \
                   options:\n        \
                     - key: rerun\n          \
                       label: Run it again\n          \
                       run: 'echo rerun >> again.txt'\n        \
                     - key: ignore\n          \
                       label: Ignore\n    \
                 cwd: {}\n",
            dir.display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    let failing: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 1"], "cwd": dir, "title": "deploy" }),
        )
        .await;
    c.wait_terminal(&failing.id).await;

    // The question turns up, wearing the event's words.
    let deadline = Instant::now() + Duration::from_secs(15);
    let d = loop {
        let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
        if let Some(d) = open.into_iter().next() {
            break d;
        }
        assert!(Instant::now() < deadline, "no question was raised");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(d.title, "deploy failed");
    assert_eq!(d.group.as_deref(), Some("reaction:ask-me"));
    assert_eq!(d.run.as_deref(), Some(failing.id.as_str()));
    assert_eq!(d.options.len(), 2);

    // Answering it runs what the option carries.
    let taken: Decision = c.ok("decision.take", json!({ "id": d.id, "options": ["rerun"] })).await;
    assert_eq!(taken.outcome_runs.len(), 1);
    c.wait_terminal(&taken.outcome_runs[0]).await;
    assert_eq!(wait_for(&dir.join("again.txt")).await.trim(), "rerun");

    // Raising it again does not pile up, and the rule ignores its own question.
    let all: Vec<Decision> = c.ok("decision.list", json!({ "all": true })).await;
    assert_eq!(all.len(), 1, "one question, not a stack");
    h.stop().await;
}

#[tokio::test]
async fn where_narrows_and_test_explains() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    write(
        &home,
        &format!(
            "reactions:\n  \
               only-nightly:\n    \
                 on: run.failed\n    \
                 where: {{ tag: nightly }}\n    \
                 run: 'echo nightly >> matched.txt'\n    \
                 cwd: {d}\n  \
               everything:\n    \
                 on: '*'\n    \
                 run: 'echo any >> any.txt'\n    \
                 throttle: '0'\n    \
                 cwd: {d}\n",
            d = dir.display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // A failure without the tag: the narrow rule stays out of it.
    let untagged: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 1"], "cwd": dir, "title": "adhoc" }),
        )
        .await;
    c.wait_terminal(&untagged.id).await;
    wait_for(&dir.join("any.txt")).await;
    assert!(!dir.join("matched.txt").exists(), "`where` let it through");

    // `reaction.test` says so, on the very event, without doing anything.
    let events: Vec<Event> = c
        .ok("event.list", json!({ "run": untagged.id, "kinds": ["run.failed"] }))
        .await;
    let failed = events.last().expect("a run.failed event");
    let trials: Vec<ReactionTrial> = c.ok("reaction.test", json!({ "event": failed.id })).await;
    let only = trials.iter().find(|t| t.reaction == "only-nightly").unwrap();
    assert!(!only.fires);
    assert!(only.why.contains("tag"), "{}", only.why);
    let every = trials.iter().find(|t| t.reaction == "everything").unwrap();
    assert!(every.action.as_deref().is_some_and(|a| a.contains("any.txt")));

    // With the tag, it fires.
    let tagged: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 1"], "cwd": dir, "title": "nightly", "tags": ["nightly"] }),
        )
        .await;
    c.wait_terminal(&tagged.id).await;
    assert_eq!(wait_for(&dir.join("matched.txt")).await.trim(), "nightly");

    // Pausing stops it; the row says so.
    let paused: Reaction = c.ok("reaction.pause", json!({ "name": "only-nightly" })).await;
    assert!(paused.paused);
    for r in runs_caused_by(&mut c, "reaction:only-nightly").await {
        c.wait_terminal(&r.id).await;
    }
    for r in runs_caused_by(&mut c, "reaction:everything").await {
        c.wait_terminal(&r.id).await;
    }
    h.stop().await;
}

/// A project with a folder inside it, and a run that fails in that folder:
/// `(root, project, the folder it failed in, the failed run)`.
async fn fail_inside_a_project(c: &mut Conn, root: &Path) -> (std::path::PathBuf, std::path::PathBuf, Run) {
    let ws = root.join("proj");
    let deep = ws.join("deep");
    for d in [deep.join("sub"), ws.join("other")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let up = c.call("workspace.up", json!({ "path": ws })).await;
    assert!(up.error.is_none(), "{:?}", up.error);
    let failing: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 1"], "cwd": deep, "title": "deep" }),
        )
        .await;
    assert_eq!(failing.workspace, ws);
    c.wait_terminal(&failing.id).await;
    (ws, deep, failing)
}

fn first(runs: Vec<Run>) -> Run {
    runs.into_iter().next().unwrap()
}

/// A global rule with no `cwd` runs where the event happened: in the folder
/// the failed run ran in, inside that run's project; for an event no run is
/// behind, in the event's workspace. A global schedule still needs a `cwd`:
/// nothing happened for it to follow.
#[tokio::test]
async fn a_global_rule_with_no_cwd_runs_where_the_event_was() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    write(
        &home,
        "reactions:\n  \
           here:\n    \
             on: run.failed\n    \
             run: 'true'\n  \
           emitted:\n    \
             on: probe.found\n    \
             run: 'true'\n",
    );
    std::fs::write(
        home.schedules_file(),
        "schedules:\n  nowhere:\n    every: 1h\n    run: 'true'\n",
    )
    .unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // Both reactions load; the schedule without a `cwd` is refused.
    let listed: Vec<Reaction> = c.ok("reaction.list", json!({})).await;
    assert_eq!(listed.len(), 2, "{listed:?}");
    let report: serde_json::Value = c.ok("schedule.reload", json!(null)).await;
    let errors = report["errors"].to_string();
    assert!(errors.contains("nowhere") && errors.contains("cwd"), "{report}");

    let (ws, deep, _) = fail_inside_a_project(&mut c, &root).await;
    let here = first(wait_caused_by(&mut c, "reaction:here", 1).await);
    assert_eq!(here.cwd, deep, "where the failed run ran");
    assert_eq!(here.workspace, ws, "and in its project");

    // An event no run is behind: its workspace.
    let emit = c
        .call("event.emit", json!({ "kind": "probe.found", "workspace": elsewhere }))
        .await;
    assert!(emit.error.is_none(), "{:?}", emit.error);
    let emitted = first(wait_caused_by(&mut c, "reaction:emitted", 1).await);
    assert_eq!(emitted.cwd, elsewhere);

    for r in [here, emitted] {
        c.wait_terminal(&r.id).await;
    }
    h.stop().await;
}

/// A global rule's `cwd` is filled in like its `run` — `{workspace}`, and
/// `{cwd}` for the folder the event's run ran in — and a relative one is
/// under that folder, joined once.
#[tokio::test]
async fn a_global_rules_cwd_is_filled_in_and_joined_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(
        &home,
        &format!(
            "reactions:\n  \
               under:\n    \
                 on: run.failed\n    \
                 run: 'true'\n    \
                 cwd: sub\n  \
               filled:\n    \
                 on: run.failed\n    \
                 run: 'true'\n    \
                 cwd: '{{workspace}}/other'\n  \
               said:\n    \
                 on: run.failed\n    \
                 run: 'echo \"{{cwd}}\" > said.txt'\n    \
                 cwd: {}\n",
            root.display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    let (ws, deep, _) = fail_inside_a_project(&mut c, &root).await;
    let said = wait_for(&root.join("said.txt")).await;
    assert_eq!(said.trim(), deep.to_string_lossy(), "{{cwd}} is the run's own folder");
    let under = first(wait_caused_by(&mut c, "reaction:under", 1).await);
    assert_eq!(under.cwd, deep.join("sub"), "relative to that folder, joined once");
    let filled = first(wait_caused_by(&mut c, "reaction:filled", 1).await);
    assert_eq!(filled.cwd, ws.join("other"), "{{workspace}} filled in a cwd");

    for name in ["under", "filled", "said"] {
        for r in runs_caused_by(&mut c, &format!("reaction:{name}")).await {
            c.wait_terminal(&r.id).await;
        }
    }
    h.stop().await;
}

/// What a reaction heard is kept: the run it starts names the event in its
/// `run.started`, and so does the question it asks in its `decision.raised`.
/// Two failures a moment apart in one chain each have their own — which the
/// chain and the clock alone could not tell apart.
#[tokio::test]
async fn a_reaction_keeps_the_event_it_fired_on() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    write(
        &home,
        &format!(
            "reactions:\n  \
               told:\n    \
                 on: run.failed\n    \
                 run: 'echo \"$RELAY_EVENT_RUN $RELAY_EVENT_ID\" >> told.txt'\n    \
                 throttle: '0'\n    \
                 cwd: {d}\n  \
               ask-me:\n    \
                 on: run.failed\n    \
                 ask:\n      \
                   title: 'retry?'\n      \
                   options: [{{ key: ignore }}]\n    \
                 throttle: '0'\n    \
                 cwd: {d}\n",
            d = dir.display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // Two runs in one chain fail together.
    let first: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 1"], "cwd": dir, "title": "tests" }),
        )
        .await;
    let second: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 2"], "cwd": dir, "title": "lint", "flow": first.flow }),
        )
        .await;
    assert_eq!(second.flow, first.flow);
    c.wait_terminal(&first.id).await;
    c.wait_terminal(&second.id).await;
    let mut fails = Vec::new();
    for r in [&first, &second] {
        let evs: Vec<Event> = c
            .ok("event.list", json!({ "run": r.id, "kinds": ["run.failed"] }))
            .await;
        assert_eq!(evs.len(), 1, "{evs:?}");
        fails.push(evs[0].clone());
    }

    // One run for each, naming the event it heard, as the command was told.
    let started = wait_caused_by(&mut c, "reaction:told", 2).await;
    assert_eq!(started.len(), 2);
    let mut heard = Vec::new();
    for r in &started {
        assert_eq!(r.flow, first.flow, "in the chain");
        let evs: Vec<Event> = c
            .ok("event.list", json!({ "run": r.id, "kinds": ["run.started"] }))
            .await;
        heard.push(
            evs[0]
                .data
                .as_ref()
                .and_then(|d| d["event"].as_i64())
                .expect("data.event"),
        );
        c.wait_terminal(&r.id).await;
    }
    heard.sort();
    assert_eq!(heard, [fails[0].id, fails[1].id], "each failure has its own run");
    let told = std::fs::read_to_string(dir.join("told.txt")).unwrap();
    let mut told: Vec<&str> = told.lines().collect();
    told.sort();
    let mut want: Vec<String> = fails
        .iter()
        .map(|e| format!("{} {}", e.run.as_deref().unwrap(), e.id))
        .collect();
    want.sort();
    assert_eq!(told, want, "RELAY_EVENT_ID is the same event");

    // The question says who asked and about which, each time it is asked.
    let deadline = Instant::now() + Duration::from_secs(15);
    let asked = loop {
        let evs: Vec<Event> = c.ok("event.list", json!({ "kinds": ["decision.raised"] })).await;
        if evs.len() >= 2 {
            break evs;
        }
        assert!(Instant::now() < deadline, "asked {} times, wanted 2", evs.len());
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let mut about: Vec<i64> = asked
        .iter()
        .map(|e| {
            let d = e.data.as_ref().unwrap();
            assert_eq!(d["reaction"], "ask-me", "{d}");
            d["event"].as_i64().expect("data.event")
        })
        .collect();
    about.sort();
    assert_eq!(about, [fails[0].id, fails[1].id]);
    h.stop().await;
}
