//! `setup.get` says how far the guide got and what it starts from;
//! `setup.save` writes what it settled into config.yaml, leaving the rest of
//! the file alone, and keeps how far it got in the ledger.

mod common;

use common::{Conn, Harness, session_bin};
use rl_core::Home;
use rl_proto::methods::{AgentInfo, ConfigReport, SETUP_STEPS, SetupState};
use serde_json::json;

#[tokio::test]
async fn the_guide_is_asked_once_and_writes_what_it_settled() {
    let Some(_) = session_bin() else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;

    let s: SetupState = c.ok("setup.get", json!(null)).await;
    assert_eq!(s.seen, 0, "never gone through");
    assert!(s.agents.iter().all(|a| a.offered), "unset: every agent offered");
    let gemini = s.agents.iter().find(|a| a.name == "gemini").unwrap();
    assert_eq!(gemini.small_model.as_deref(), Some("flash-lite"));

    let r: ConfigReport = c
        .ok(
            "setup.save",
            json!({
                "seen": SETUP_STEPS,
                "agents": ["claude", "gemini"],
                "assistant": { "agent": "claude", "model": "haiku" },
            }),
        )
        .await;
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.config.agents, ["claude", "gemini"]);
    assert_eq!(
        r.config.assistants.interpret.as_ref().unwrap().model.as_deref(),
        Some("haiku")
    );
    let text = std::fs::read_to_string(home.config_file()).unwrap();
    assert!(
        text.starts_with("# Your Relay defaults."),
        "the seed's comments stay:\n{text}"
    );

    let s: SetupState = c.ok("setup.get", json!(null)).await;
    assert_eq!(s.seen, SETUP_STEPS);
    let agents: Vec<AgentInfo> = c.ok("agent.list", json!(null)).await;
    let offered: Vec<&str> = agents.iter().filter(|a| a.offered).map(|a| a.name.as_str()).collect();
    assert_eq!(offered, ["claude", "gemini"]);

    // Nothing to write, only that it was gone through: the file is untouched.
    let _: ConfigReport = c.ok("setup.save", json!({ "seen": SETUP_STEPS })).await;
    assert_eq!(std::fs::read_to_string(home.config_file()).unwrap(), text);
    drop(h);
}
