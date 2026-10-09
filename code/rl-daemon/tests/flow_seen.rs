//! What a firing came to, as a client is sent it: the first line of its last step's out on every
//! firing, all of it on `flow.outcome`.

mod common;
use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_proto::flow::{Firing, Outcome};
use rl_proto::methods::Digest;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

/// A daemon on a fresh home with `flow.md` written for `name`; `None` when `rl-session` is not built.
async fn start(tmp: &Path, name: &str, flow_md: &str) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flow.md"), flow_md).unwrap();
    let Some(h) = Harness::on(home) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return None;
    };
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing of `flow` has ended.
async fn ended(c: &mut Conn, flow: &str) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next().filter(|f| f.ended_at.is_some()) {
            return f;
        }
        assert!(Instant::now() < deadline, "`{flow}` never ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `flow.outcome` of `id`: the outcome, or None (a null result).
async fn outcome(c: &mut Conn, id: &str) -> Option<Outcome> {
    let r = c.call("flow.outcome", json!({ "id": id })).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    serde_json::from_value(r.result.unwrap_or_default()).unwrap()
}

#[tokio::test]
async fn a_firing_comes_to_what_its_last_step_printed() {
    let tmp = tempfile::tempdir().unwrap();
    let md = r#"---
steps:
  gather:
    run: echo gathered
  brief:
    after: gather
    run: 'printf "Morning brief\n\nAnna on the parser\n"'
---
"#;
    let Some((h, mut c)) = start(tmp.path(), "brief", md).await else {
        return;
    };
    common::approve(&mut c, "brief").await;
    let fired: Firing = c.ok("flow.run", json!({ "name": "brief" })).await;
    let f = ended(&mut c, "brief").await;
    let run = f.steps[1].run.clone().expect("brief ran");
    wait_journal(&h.home, &run, "Anna on the parser").await;

    let f = ended(&mut c, "brief").await;
    assert_eq!(f.outcome.as_deref(), Some("Morning brief"), "{f:?}");
    let d: Digest = c.ok("digest.get", json!({})).await;
    let latest = d.firings.iter().find(|x| x.flow == "brief").expect("the latest firing");
    assert_eq!(latest.outcome.as_deref(), Some("Morning brief"), "on the list too");

    let o = outcome(&mut c, &fired.id).await.expect("an outcome");
    assert_eq!(o.step, "brief");
    assert_eq!(o.text, "Morning brief\n\nAnna on the parser", "all of it, asked for");
    assert!(outcome(&mut c, "nope").await.is_none(), "no such firing: nothing");
    h.stop().await;
}

#[tokio::test]
async fn a_failed_firing_comes_to_why() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  gchat:\n    run: \"echo token expired; exit 3\"\n  brief:\n    after: gchat\n    run: echo never\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "brief", md).await else {
        return;
    };
    common::approve(&mut c, "brief").await;
    let fired: Firing = c.ok("flow.run", json!({ "name": "brief" })).await;
    let f = ended(&mut c, "brief").await;
    wait_journal(&h.home, f.steps[0].run.as_deref().unwrap(), "token expired").await;
    let o = outcome(&mut c, &fired.id).await.expect("why");
    assert_eq!((o.step.as_str(), o.text.as_str()), ("gchat", "token expired"));
    assert_eq!(ended(&mut c, "brief").await.outcome.as_deref(), Some("token expired"));
    h.stop().await;
}
