//! `summary.make` past a single run's brief: a project's, a flow's or
//! everything's runs in one call over their lines, a run read deep — in parts
//! when it is long — and a dry run that only counts the calls. A stretch with
//! nothing in it asks no one. The fake summariser is set by environment
//! variables, which are the process's, so the cases run one after another in
//! one test.

mod common;

use common::{Conn, Harness, wait_journal};
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_proto::flow::Firing;
use rl_proto::methods::Digest;
use rl_proto::types::{Event, Run, Summary};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

/// A daemon whose `assistants.summary` is the fake agent answering `answer`,
/// and a project folder registered with it. The prompt the agent was last
/// given is in the file `FAKE_AGENT_PROMPT` names.
async fn with_fake_summariser(answer: &str) -> Option<(Harness, Conn, PathBuf, tempfile::TempDir)> {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  summary: fakeai\n").unwrap();
    let (answer_file, prompt) = (tmp.path().join("answer.txt"), tmp.path().join("prompt.txt"));
    std::fs::write(&answer_file, answer).unwrap();
    // SAFETY: the cases run one at a time, alone in this binary.
    unsafe {
        std::env::set_var("FAKE_AGENT_ANSWER", &answer_file);
        std::env::set_var("FAKE_AGENT_PROMPT", &prompt);
        std::env::set_var("FAKE_AGENT_READS", "1");
    }
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return None;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": dir })).await;
    Some((h, c, dir, tmp))
}

/// Back from now, in whole milliseconds as the ledger keeps them.
fn ago(d: SignedDuration) -> Timestamp {
    Timestamp::from_millisecond((Timestamp::now() - d).as_millisecond()).unwrap()
}

/// `summary.get`: the one kept, or None (a null result).
async fn kept(c: &mut Conn, params: serde_json::Value) -> Option<Summary> {
    let r = c.call("summary.get", params).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    serde_json::from_value(r.result.unwrap_or_default()).unwrap()
}

#[tokio::test]
async fn cases() {
    a_project_brief_is_one_call_over_its_runs_lines().await;
    nothing_in_the_stretch_asks_no_one().await;
    deep_over_a_long_output_is_read_in_parts().await;
    a_dry_run_says_how_many_calls_and_asks_no_one().await;
    a_flows_runs_are_its_steps_runs().await;
    one_firings_runs_are_its_steps_runs().await;
    a_run_looked_at_just_now_is_told_whole().await;
    a_runs_summary_since_you_looked_stays_once_you_look().await;
    deep_over_a_project_leaves_its_runs_as_they_were().await;
    a_dry_run_counts_a_long_runs_parts().await;
    a_project_moves_on_while_a_run_writes().await;
}

async fn a_project_brief_is_one_call_over_its_runs_lines() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("• api: two runs, both fine\n").await else {
        return;
    };
    let from = ago(SignedDuration::from_mins(5));
    for t in ["one", "two"] {
        let r: Run = c
            .ok(
                "run.start",
                json!({"argv": ["sh", "-c", "echo hi"], "cwd": dir, "title": t}),
            )
            .await;
        c.wait_terminal(&r.id).await;
    }
    let target = json!({"kind": "project", "workspace": dir});
    let s: Summary = c.ok("summary.make", json!({"target": target, "from": from})).await;
    assert_eq!(s.calls, 3, "a brief for each of the two runs, and one to join them");
    assert!(s.text.contains("two runs"), "{s:?}");
    assert_eq!((s.target.clone(), s.from), (format!("p:{}", dir.display()), Some(from)));
    let prompt = std::fs::read_to_string(std::env::var("FAKE_AGENT_PROMPT").unwrap()).unwrap();
    assert!(prompt.contains("## one") && prompt.contains("## two"), "{prompt}");

    // Kept: given back while nothing has moved; asked again, a new one.
    let k = kept(&mut c, json!({"target": target, "from": from})).await;
    assert_eq!(k.as_ref().map(|k| (&k.text, k.stale)), Some((&s.text, false)));
    let same: Summary = c.ok("summary.make", json!({"target": target, "from": from})).await;
    assert_eq!(same.by, s.by);
    let again: Summary = c
        .ok("summary.make", json!({"target": target, "from": from, "again": true}))
        .await;
    assert_ne!(again.by, s.by);

    // A project's needs where to start.
    let r = c.call("summary.make", json!({"target": target})).await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("needs where to start"), "{msg}");
    h.stop().await;
}

async fn nothing_in_the_stretch_asks_no_one() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("should not be asked").await else {
        return;
    };
    // A run from before the stretch is not in it.
    let r: Run = c
        .ok("run.start", json!({"argv": ["true"], "cwd": dir, "title": "before"}))
        .await;
    c.wait_terminal(&r.id).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let from = ago(SignedDuration::ZERO);
    for target in [
        json!({"kind": "project", "workspace": dir}),
        json!({"kind": "all"}),
        json!({"kind": "flow", "name": "nightly"}),
        json!({"kind": "run", "id": r.id}),
    ] {
        for deep in [false, true] {
            let s: Summary = c
                .ok("summary.make", json!({"target": target, "from": from, "deep": deep}))
                .await;
            assert_eq!(s.source, "nothing new", "{target} {deep}: {s:?}");
            assert_eq!((s.calls, s.agent.as_str()), (0, "relay"), "{target}");
            assert!(s.text.starts_with("nothing happened since "), "{}", s.text);
        }
    }
    let prompt = std::env::var("FAKE_AGENT_PROMPT").unwrap();
    assert!(!std::path::Path::new(&prompt).exists(), "no one was asked");
    h.stop().await;
}

async fn deep_over_a_long_output_is_read_in_parts() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("a part").await else {
        return;
    };
    // ~120 KB of output (a little more in the terminal's \r\n): three parts
    // and one join.
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "yes 'a line of output to fill the journal' | head -c 120000"], "cwd": dir, "title": "loud"}),
        )
        .await;
    c.wait_terminal(&r.id).await;
    let target = json!({"kind": "run", "id": r.id});
    let dry: Summary = c
        .ok("summary.make", json!({"target": target, "deep": true, "dry": true}))
        .await;
    assert_eq!((dry.calls, dry.text.as_str()), (4, ""), "{dry:?}");
    let s: Summary = c.ok("summary.make", json!({"target": target, "deep": true})).await;
    assert!(s.deep);
    assert_eq!(s.calls, 4);
    assert_eq!(s.text, "a part");
    let prompt = std::fs::read_to_string(std::env::var("FAKE_AGENT_PROMPT").unwrap()).unwrap();
    assert!(
        prompt.contains("## Part 1 of 3") && prompt.contains("## Part 3 of 3"),
        "the join reads the parts: {prompt}"
    );
    // Kept apart from the run's brief recap, and given back.
    let k = kept(&mut c, json!({"target": target, "deep": true})).await;
    assert_eq!(k.map(|k| (k.by, k.deep, k.calls)), Some((s.by.clone(), true, 4)));
    let brief = kept(&mut c, json!({"target": target})).await;
    assert!(brief.is_none(), "{brief:?}");
    h.stop().await;
}

async fn a_dry_run_says_how_many_calls_and_asks_no_one() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("x").await else {
        return;
    };
    let from = ago(SignedDuration::from_mins(5));
    for t in ["a", "b", "c"] {
        let r: Run = c
            .ok(
                "run.start",
                json!({"argv": ["sh", "-c", format!("echo said-{t}")], "cwd": dir, "title": t}),
            )
            .await;
        c.wait_terminal(&r.id).await;
        wait_journal(&h.home, &r.id, &format!("said-{t}")).await;
    }
    // One with nothing in it is told without asking anyone.
    let quiet: Run = c
        .ok("run.start", json!({"argv": ["true"], "cwd": dir, "title": "quiet"}))
        .await;
    c.wait_terminal(&quiet.id).await;
    let s: Summary = c
        .ok(
            "summary.make",
            json!({"target": {"kind": "all"}, "deep": true, "from": from, "dry": true}),
        )
        .await;
    assert_eq!((s.calls, s.runs), (4, 4), "three deep reads and one to join");
    assert!(s.text.is_empty());
    let prompt = std::env::var("FAKE_AGENT_PROMPT").unwrap();
    assert!(!std::path::Path::new(&prompt).exists(), "no one was asked");
    let brief: Summary = c
        .ok(
            "summary.make",
            json!({"target": {"kind": "all"}, "from": from, "dry": true}),
        )
        .await;
    // A brief of each run with something in it (the quiet one costs nothing), and one to join.
    assert_eq!((brief.calls, brief.text.as_str()), (4, ""));
    assert!(
        !std::path::Path::new(&prompt).exists(),
        "a dry brief asked no one either"
    );
    h.stop().await;
}

async fn a_flows_runs_are_its_steps_runs() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("nightly: fine").await else {
        return;
    };
    // A flow's runs are those its steps started; here one, set in the
    // ledger as it would be.
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "echo nightly ran"], "cwd": dir, "title": "nightly tests"}),
        )
        .await;
    let other: Run = c
        .ok("run.start", json!({"argv": ["true"], "cwd": dir, "title": "by hand"}))
        .await;
    c.wait_terminal(&other.id).await;
    let r = c.wait_terminal(&r.id).await;
    wait_journal(&h.home, &r.id, "nightly ran").await;
    let ledger = rl_ledger::Ledger::open(&h.home.ledger()).unwrap();
    let id = r.id.clone();
    ledger
        .call_blocking(move |c| {
            c.execute("UPDATE runs SET cause = 'flow:nightly/test' WHERE id = ?1", [id])?;
            Ok(())
        })
        .unwrap();
    // No stretch given: from its oldest run not yet looked at.
    let target = json!({"kind": "flow", "name": "nightly"});
    let s: Summary = c.ok("summary.make", json!({"target": target})).await;
    assert_eq!((s.calls, s.text.as_str()), (2, "nightly: fine"), "{s:?}");
    assert_eq!(s.from, Some(r.started_at), "the unseen run is in the stretch");
    assert_eq!(s.target, "f:nightly");
    let prompt = std::fs::read_to_string(std::env::var("FAKE_AGENT_PROMPT").unwrap()).unwrap();
    assert!(prompt.contains("## nightly tests"), "{prompt}");
    assert!(!prompt.contains("by hand"), "{prompt}");

    // Deep, the same stretch reads the run itself: its output, then the
    // join.
    let deep: Summary = c.ok("summary.make", json!({"target": target, "deep": true})).await;
    assert_ne!(deep.source, "nothing new", "{deep:?}");
    assert_eq!((deep.calls, deep.text.as_str()), (2, "nightly: fine"), "{deep:?}");
    let run: Summary = c
        .ok(
            "summary.get",
            json!({"target": {"kind": "run", "id": r.id}, "deep": true}),
        )
        .await;
    assert_eq!(run.text, "nightly: fine", "its deep read is kept: {run:?}");
    h.stop().await;
}

async fn a_run_looked_at_just_now_is_told_whole() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("It said one, then two.").await else {
        return;
    };
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "echo one; read go; echo two; sleep 30"], "cwd": dir, "title": "waits"}),
        )
        .await;
    wait_journal(&h.home, &r.id, "one").await;
    // Selected in the panel: looked at now, and it has done nothing since —
    // the whole of it, not "nothing happened".
    let _: usize = c.ok("run.seen", json!({"ids": [r.id]})).await;
    let target = json!({"kind": "run", "id": r.id});
    let s: Summary = c.ok("summary.make", json!({"target": target})).await;
    assert_eq!((s.from, s.text.as_str()), (None, "It said one, then two."), "{s:?}");
    let prompt = std::fs::read_to_string(std::env::var("FAKE_AGENT_PROMPT").unwrap()).unwrap();
    assert!(prompt.contains("one"), "{prompt}");

    // It goes on after the look: since then.
    let seen = c.get(&r.id).await.seen_at;
    let res = c
        .call("run.input", json!({"id": r.id, "bytes": b"go\n".to_vec()}))
        .await;
    assert!(res.error.is_none(), "{:?}", res.error);
    wait_journal(&h.home, &r.id, "two").await;
    let s: Summary = c.ok("summary.make", json!({"target": target})).await;
    assert_eq!(s.from, seen, "{s:?}");
    let _: serde_json::Value = c.ok("run.stop", json!({"id": r.id, "force": true})).await;
    h.stop().await;
}

async fn a_runs_summary_since_you_looked_stays_once_you_look() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("It said two.").await else {
        return;
    };
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "echo one; read go; echo two; sleep 30"], "cwd": dir, "title": "waits"}),
        )
        .await;
    wait_journal(&h.home, &r.id, "one").await;
    let _: usize = c.ok("run.seen", json!({"ids": [r.id]})).await;
    let res = c
        .call("run.input", json!({"id": r.id, "bytes": b"go\n".to_vec()}))
        .await;
    assert!(res.error.is_none(), "{:?}", res.error);
    wait_journal(&h.home, &r.id, "two").await;
    let target = json!({"kind": "run", "id": r.id});
    let s: Summary = c.ok("summary.make", json!({"target": target})).await;
    assert!(s.from.is_some(), "since you looked: {s:?}");
    // Looked at again, with nothing new: the one made is still what the
    // pane shows, not nothing.
    let _: usize = c.ok("run.seen", json!({"ids": [r.id]})).await;
    let k = kept(&mut c, json!({"target": target})).await;
    assert_eq!(k.map(|k| (k.text, k.from)), Some((s.text.clone(), s.from)));
    let _: serde_json::Value = c.ok("run.stop", json!({"id": r.id, "force": true})).await;
    h.stop().await;
}

async fn deep_over_a_project_leaves_its_runs_as_they_were() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("read closely").await else {
        return;
    };
    let from = ago(SignedDuration::from_mins(5));
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "echo hi"], "cwd": dir, "title": "one"}),
        )
        .await;
    c.wait_terminal(&r.id).await;
    wait_journal(&h.home, &r.id, "hi").await;
    let _: usize = c.ok("run.seen", json!({"ids": [r.id]})).await;
    let line = |d: &Digest| {
        d.items
            .iter()
            .find(|i| i.run.id == r.id)
            .map(|i| (i.unseen, i.summary.clone()))
    };
    let before: Digest = c.ok("digest.get", json!({})).await;
    let target = json!({"kind": "project", "workspace": dir});
    let s: Summary = c
        .ok("summary.make", json!({"target": target, "from": from, "deep": true}))
        .await;
    assert_eq!(s.calls, 2, "{s:?}");
    // Reading a run for a catch-up is not news on it.
    let evs: Vec<Event> = c
        .ok("event.list", json!({"run": r.id, "kinds": ["run.summarised"]}))
        .await;
    assert!(evs.is_empty(), "{evs:?}");
    let after: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(line(&after), line(&before));
    assert_eq!(line(&after).map(|l| l.0), Some(false), "still seen");
    h.stop().await;
}

async fn a_dry_run_counts_a_long_runs_parts() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("x").await else {
        return;
    };
    let from = ago(SignedDuration::from_mins(5));
    // ~120 KB: three parts and their join, then one more to join the runs.
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "yes 'a line of output to fill the journal' | head -c 120000"], "cwd": dir, "title": "loud"}),
        )
        .await;
    c.wait_terminal(&r.id).await;
    let target = json!({"kind": "project", "workspace": dir});
    let dry: Summary = c
        .ok(
            "summary.make",
            json!({"target": target, "from": from, "deep": true, "dry": true}),
        )
        .await;
    assert_eq!((dry.calls, dry.runs), (5, 1), "{dry:?}");
    let prompt = std::env::var("FAKE_AGENT_PROMPT").unwrap();
    assert!(!std::path::Path::new(&prompt).exists(), "no one was asked");
    h.stop().await;
}

async fn a_project_moves_on_while_a_run_writes() {
    let Some((h, mut c, dir, _tmp)) = with_fake_summariser("it waits").await else {
        return;
    };
    let from = ago(SignedDuration::from_mins(5));
    let r: Run = c
        .ok(
            "run.start",
            json!({"argv": ["sh", "-c", "echo one; read go; echo two; sleep 30"], "cwd": dir, "title": "waits"}),
        )
        .await;
    wait_journal(&h.home, &r.id, "one").await;
    let target = json!({"kind": "project", "workspace": dir});
    let _: Summary = c.ok("summary.make", json!({"target": target, "from": from})).await;
    let k = kept(&mut c, json!({"target": target, "from": from})).await;
    assert_eq!(k.map(|k| k.stale), Some(false));
    // It writes on, in the same state: what was said has moved on.
    let res = c
        .call("run.input", json!({"id": r.id, "bytes": b"go\n".to_vec()}))
        .await;
    assert!(res.error.is_none(), "{:?}", res.error);
    wait_journal(&h.home, &r.id, "two").await;
    let k = kept(&mut c, json!({"target": target, "from": from})).await;
    assert_eq!(k.map(|k| k.stale), Some(true));
    let _: serde_json::Value = c.ok("run.stop", json!({"id": r.id, "force": true})).await;
    h.stop().await;
}

/// Polls `flow.firings` until the firing `id` of `flow` has ended.
async fn firing_ended(c: &mut Conn, flow: &str, id: &str) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let fs: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = fs.into_iter().find(|f| f.id == id && f.ended_at.is_some()) {
            return f;
        }
        assert!(Instant::now() < deadline, "{id} never ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn one_firings_runs_are_its_steps_runs() {
    let Some((h, mut c, _dir, _tmp)) = with_fake_summariser("twice: fine").await else {
        return;
    };
    let dir = h.home.flow_dir("twice");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("flow.md"),
        "---\nsteps:\n  a:\n    run: echo one\n  b:\n    after: a\n    run: echo two\n---\n",
    )
    .unwrap();
    let _: serde_json::Value = c.ok("flow.reload", json!(null)).await;
    common::approve(&mut c, "twice").await;
    let mut done = Vec::new();
    for _ in 0..2 {
        let f: Firing = c.ok("flow.run", json!({ "name": "twice" })).await;
        done.push(firing_ended(&mut c, "twice", &f.id).await);
    }
    let first = &done[0];
    let target = json!({"kind": "firing", "id": first.id});
    let s: Summary = c.ok("summary.make", json!({"target": target})).await;
    assert_eq!(
        (s.calls, s.runs, s.text.as_str()),
        (3, 2, "twice: fine"),
        "its two runs, not the next firing's: {s:?}"
    );
    assert_eq!(s.target, format!("fi:{}", first.id));
    assert_eq!(s.from, Some(first.started_at), "all of it");
    let prompt = std::fs::read_to_string(std::env::var("FAKE_AGENT_PROMPT").unwrap()).unwrap();
    assert!(prompt.contains("one firing of the flow `twice`"), "{prompt}");
    let k = kept(&mut c, json!({"target": target})).await;
    assert_eq!(k.map(|k| k.text), Some(s.text.clone()), "kept, and given back");

    let r = c
        .call("summary.make", json!({"target": {"kind": "firing", "id": "nope"}}))
        .await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("no firing nope"), "{msg}");
    h.stop().await;
}
