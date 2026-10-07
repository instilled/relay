//! A flow's ask steps: the question raised in the firing's chain, and the
//! answer carrying the firing on — the option chosen runs as the step's own.

mod common;
use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::{Firing, FiringStatus, Flow, StepState, StepStatus};
use rl_proto::types::{Cause, Run};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

/// A daemon on a fresh home with each flow's `flow.md` written; `None` when `rl-session` is not built.
async fn start(tmp: &Path, flows: &[(&str, &str)]) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    for (name, md) in flows {
        let dir = home.flow_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), md).unwrap();
    }
    let h = Harness::on(home)?;
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing is `done`.
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
    let _: Flow = c.ok("flow.approve", json!({ "name": flow })).await;
    c.ok("flow.run", json!({ "name": flow })).await
}

/// The one open question, raised by `f`'s step `review`.
async fn the_question(c: &mut Conn, f: &Firing) -> Decision {
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "{open:?}");
    let d = open.into_iter().next().unwrap();
    assert_eq!(step(f, "review").decision.as_deref(), Some(d.id.as_str()), "{f:?}");
    d
}

/// probe prints 502; review asks about it, `page` running a command and `ignore` nothing.
fn pager(extra: &str) -> String {
    format!(
        "---\nsteps:\n  probe:\n    run: echo 502\n  review:\n    after: probe\n    ask: \"page? {{probe.out}}\"\n    \
         options: {{ page: {{ run: \"echo paged $RELAY_NOTE\" }}, ignore: {{}} }}\n{extra}---\n"
    )
}

#[tokio::test]
async fn an_ask_step_waits_and_the_answer_runs_as_the_step() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &pager(""))]).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", waiting).await;
    assert_eq!(f.id, fired.id);
    assert_eq!(step(&f, "probe").status, StepStatus::Done);
    assert_eq!(step(&f, "review").status, StepStatus::Waiting);
    let d = the_question(&mut c, &f).await;
    assert_eq!(d.title, "page? 502");
    assert_eq!(d.chain, f.id, "the question is in the firing's chain");
    let keys: Vec<(&str, bool)> = d.options.iter().map(|o| (o.key.as_str(), o.default)).collect();
    assert_eq!(keys, [("page", false), ("ignore", false)]);

    let taken: Decision = c
        .ok(
            "decision.take",
            json!({ "id": d.id, "options": ["page"], "note": "now" }),
        )
        .await;
    assert!(
        taken.outcome_runs.is_empty(),
        "the engine starts the option, not the question"
    );
    let f = wait_firing(&mut c, "pager", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let review = step(&f, "review");
    assert_eq!(review.status, StepStatus::Done);
    let run: Run = c.get(review.run.as_deref().expect("the option ran as the step")).await;
    wait_journal(&h.home, &run.id, "paged now").await;
    assert_eq!(
        run.cause,
        Cause::Flow {
            flow: "pager".into(),
            step: "review".into()
        }
    );
    assert_eq!(run.chain, f.id);
    h.stop().await;
}

#[tokio::test]
async fn what_was_chosen_is_said_to_what_comes_after() {
    let tmp = tempfile::tempdir().unwrap();
    let md =
        pager("  again:\n    after: review\n    ask: \"{review.choice}: {review.note}\"\n    options: { ok: {} }\n");
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &md)]).await else {
        return;
    };
    approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", waiting).await;
    let d = the_question(&mut c, &f).await;
    let _: Decision = c
        .ok(
            "decision.take",
            json!({ "id": d.id, "options": ["page"], "note": "now" }),
        )
        .await;
    let f = wait_firing(&mut c, "pager", |f| step(f, "again").decision.is_some()).await;
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].title, "page: now", "{f:?}");
    h.stop().await;
}

#[tokio::test]
async fn an_option_that_does_nothing_ends_done() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &pager(""))]).await else {
        return;
    };
    approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", waiting).await;
    let d = the_question(&mut c, &f).await;
    let _: Decision = c
        .ok("decision.take", json!({ "id": d.id, "options": ["ignore"] }))
        .await;
    let f = wait_firing(&mut c, "pager", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    assert_eq!(step(&f, "review").status, StepStatus::Done);
    assert_eq!(step(&f, "review").run, None, "nothing ran");
    h.stop().await;
}

#[tokio::test]
async fn a_dismissed_ask_ends_the_firing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &pager(""))]).await else {
        return;
    };
    approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", waiting).await;
    let d = the_question(&mut c, &f).await;
    let _: Decision = c.ok("decision.dismiss", json!({ "id": d.id })).await;
    let f = wait_firing(&mut c, "pager", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    assert_eq!(step(&f, "review").status, StepStatus::Skipped);
    assert_eq!(step(&f, "review").run, None);
    h.stop().await;
}

#[tokio::test]
async fn an_ask_expiring_takes_its_default() {
    let tmp = tempfile::tempdir().unwrap();
    let md = pager("    default: page\n    expires: 1s\n");
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &md)]).await else {
        return;
    };
    approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let review = step(&f, "review");
    assert_eq!(review.status, StepStatus::Done);
    let d: Decision = c.ok("decision.get", json!({ "id": review.decision })).await;
    assert_eq!(
        (d.status, d.chosen.as_slice()),
        (DecisionStatus::Taken, ["page".to_string()].as_slice())
    );
    wait_journal(&h.home, review.run.as_deref().unwrap(), "paged").await;
    h.stop().await;
}

#[tokio::test]
async fn an_ask_expiring_with_no_default_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let md = pager("    expires: 1s\n  next:\n    after: review\n    run: echo never\n");
    let Some((h, mut c)) = start(tmp.path(), &[("pager", &md)]).await else {
        return;
    };
    approve_and_run(&mut c, "pager").await;
    let f = wait_firing(&mut c, "pager", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    assert_eq!(step(&f, "review").status, StepStatus::Skipped);
    assert_eq!(step(&f, "next").status, StepStatus::Skipped);
    h.stop().await;
}

#[tokio::test]
async fn a_stopped_step_dismisses_its_firings_question() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  slow:\n    run: sleep 30\n  review:\n    ask: sure?\n    options: { go: { run: echo go } }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("both", md)]).await else {
        return;
    };
    approve_and_run(&mut c, "both").await;
    let f = wait_firing(&mut c, "both", |f| {
        step(f, "slow").run.is_some() && step(f, "review").decision.is_some()
    })
    .await;
    let d = the_question(&mut c, &f).await;
    let _: Run = c.ok("run.stop", json!({ "id": step(&f, "slow").run })).await;
    let f = wait_firing(&mut c, "both", ended).await;
    assert_eq!(f.status, FiringStatus::Stopped, "{f:?}");
    assert_eq!(step(&f, "slow").status, StepStatus::Stopped);
    assert_eq!(step(&f, "review").status, StepStatus::Skipped);
    let d: Decision = c.ok("decision.get", json!({ "id": d.id })).await;
    assert_eq!(
        d.status,
        DecisionStatus::Dismissed,
        "no option runs in a stopped firing"
    );
    h.stop().await;
}

#[tokio::test]
async fn the_same_group_asks_once() {
    let tmp = tempfile::tempdir().unwrap();
    let md = |who: &str| {
        format!("---\nsteps:\n  review:\n    ask: \"{who} asks\"\n    group: same\n    options: {{ ok: {{}} }}\n---\n")
    };
    let (a, b) = (md("a"), md("b"));
    let Some((h, mut c)) = start(tmp.path(), &[("a", &a), ("b", &b)]).await else {
        return;
    };
    approve_and_run(&mut c, "a").await;
    let fa = wait_firing(&mut c, "a", waiting).await;
    let asked = the_question(&mut c, &fa).await;

    approve_and_run(&mut c, "b").await;
    let fb = wait_firing(&mut c, "b", |f| step(f, "review").decision.is_some()).await;
    assert_eq!(step(&fb, "review").decision.as_deref(), Some(asked.id.as_str()));
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].id, asked.id, "restated, not asked again");
    assert_eq!(open[0].title, "b asks");
    assert_eq!(open[0].chain, fb.id, "it belongs to the newest firing now");

    let fa = wait_firing(&mut c, "a", ended).await;
    assert_eq!(fa.status, FiringStatus::Done, "{fa:?}");
    assert_eq!(step(&fa, "review").status, StepStatus::Skipped);

    let _: Decision = c
        .ok("decision.take", json!({ "id": asked.id, "options": ["ok"] }))
        .await;
    let fb = wait_firing(&mut c, "b", ended).await;
    assert_eq!(fb.status, FiringStatus::Done, "{fb:?}");
    assert_eq!(step(&fb, "review").status, StepStatus::Done);
    h.stop().await;
}
