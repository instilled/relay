//! `digest.get` and `run.seen`: one line per run, news until looked at,
//! the joiner named.

mod common;

use common::{Conn, Harness, fake_cli};
use rl_core::Home;
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::types::{Run, RunState};
use serde_json::json;

fn by_id<'a>(d: &'a Digest, id: &str) -> &'a DigestItem {
    d.items.iter().find(|i| i.run.id == id).expect("item in digest")
}

#[tokio::test]
async fn digest_summarises_news_and_seen_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();

    let done: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "exit 0"], "cwd": cwd, "title": "quick" }),
        )
        .await;
    let agent: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent", "title": "agent" }),
        )
        .await;
    c.wait_terminal(&done.id).await;
    c.ok::<Run>(
        "run.hook",
        json!({ "id": agent.id, "event": "Notification",
                "payload": { "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash" } }),
    )
    .await;
    // A viewer joins; the digest names them.
    let mut j = Conn::open(&h.home).await;
    let res = j
        .call(
            "run.attach",
            json!({ "id": agent.id, "mode": "join", "client": "bob@desk" }),
        )
        .await;
    assert!(res.error.is_none(), "{:?}", res.error);

    let d: Digest = c.ok("digest.get", json!({})).await;
    let q = by_id(&d, &agent.id);
    assert_eq!(q.run.state, RunState::NeedsInput);
    assert_eq!(q.summary, "Claude needs your permission to use Bash");
    assert!(q.unseen);
    assert_eq!(q.joined_by.as_deref(), Some("bob@desk"));
    let f = by_id(&d, &done.id);
    assert!(f.summary.starts_with("exit 0 · "), "{}", f.summary);
    assert!(f.unseen, "a finish is news");

    // Looking at it clears the news; newer news brings it back.
    let n: usize = c.ok("run.seen", json!({ "ids": [agent.id, done.id] })).await;
    assert_eq!(n, 2);
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(!by_id(&d, &agent.id).unseen);
    assert!(!by_id(&d, &done.id).unseen);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    c.ok::<Run>("run.hook", json!({ "id": agent.id, "event": "Stop", "payload": {} }))
        .await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    let idle = by_id(&d, &agent.id);
    assert_eq!(idle.run.state, RunState::Idle);
    assert_eq!(idle.summary, "finished responding");
    assert!(!idle.unseen, "idle is not news");

    // `since` narrows to runs with events after it, but a waiting run always shows.
    let far: Digest = c.ok("digest.get", json!({ "since": "2999-01-01T00:00:00Z" })).await;
    assert!(far.items.is_empty());
    c.ok::<Run>("run.mark", json!({ "id": agent.id, "mark": "blocked" }))
        .await;
    let far: Digest = c.ok("digest.get", json!({ "since": "2999-01-01T00:00:00Z" })).await;
    assert_eq!(far.items.len(), 1);
    assert_eq!(by_id(&far, &agent.id).summary, "marked blocked");

    c.ok::<Run>("run.stop", json!({ "id": agent.id })).await;
    c.wait_terminal(&agent.id).await;
    h.stop().await;
}

/// A registered git folder comes with its checkout, and the configured badges
/// say their piece beside the branch once they have been asked.
#[tokio::test]
async fn a_git_folder_carries_its_checkout_and_badges() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let ws = tmp.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(&ws).unwrap();
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(&ws)
        .args(["init", "-q", "-b", "trunk"])
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(
        home.config_file(),
        "workspaces:\n  badges: [\"echo PR \\\"#1 on $RELAY_BRANCH\\\"\", \"exit 1\"]\n",
    )
    .unwrap();
    let Some(h) = Harness::on(home) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": ws })).await;
    // The shipped badge is there to be named.
    assert!(h.home.plugins_dir().join("gh-pr").exists());

    let mut badges = vec![];
    for _ in 0..100 {
        let d: Digest = c.ok("digest.get", json!({})).await;
        let co = d
            .checkouts
            .iter()
            .find(|x| x.path == ws)
            .expect("the folder's checkout");
        assert_eq!(co.branch.as_deref(), Some("trunk"));
        badges = co.badges.clone();
        if !badges.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(badges, ["PR #1 on trunk"]);
    h.stop().await;
}
