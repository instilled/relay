//! `config.get` says what config.yaml means: absent → defaults, the first
//! available agent; named → resolved to an argv; wrong → an error, never a
//! guess. `agent.list` says what is on this machine.

mod common;

use common::{Conn, Harness, session_bin};
use rl_core::Home;
use rl_proto::methods::{AgentInfo, ConfigReport};
use serde_json::json;

#[tokio::test]
async fn config_is_read_resolved_and_refused_when_wrong() {
    let Some(_) = session_bin() else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    // The recorded `claude` player lives in code/fixtures; put it first on
    // PATH so `claude` is an available agent whatever the machine has.
    let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let fixtures = std::fs::canonicalize(fixtures).unwrap();
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fixtures.clone()];
    paths.extend(std::env::split_paths(&old));
    // SAFETY: this test binary is the only thing in this process.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;

    let agents: Vec<AgentInfo> = c.ok("agent.list", json!(null)).await;
    let claude = agents.iter().find(|a| a.name == "claude").unwrap();
    assert_eq!(claude.bin.as_deref(), Some(fixtures.join("claude").as_path()));
    assert_eq!(
        (claude.headless.clone(), claude.model_flag.clone()),
        (vec!["-p".to_string()], vec!["--model".to_string()])
    );
    assert!(agents.iter().any(|a| a.name == "codex"), "{agents:?}");
    assert!(
        !agents.iter().any(|a| a.name == "sh" || a.name == "generic"),
        "shells are not agents"
    );

    // A fresh home: the seeded file (all comments) means defaults, and the
    // first available agent is the interpreter.
    let r: ConfigReport = c.ok("config.get", json!(null)).await;
    assert!(
        r.present && r.errors.is_empty() && r.config == Default::default(),
        "{r:?}"
    );
    let i = r.interpret.unwrap();
    assert_eq!(
        (i.agent.as_str(), i.source.as_str(), i.available),
        ("claude", "default", true)
    );
    assert_eq!(i.argv[1..], ["-p"]);

    // Named, with a model: the argv carries it through the profile's flag.
    std::fs::write(
        home.config_file(),
        "assistants:\n  interpret:\n    agent: claude\n    model: claude-haiku-4-5-20251001\n    args: [\"--max-turns\", \"1\"]\n",
    )
    .unwrap();
    let r: ConfigReport = c.ok("config.get", json!(null)).await;
    assert!(r.present && r.errors.is_empty(), "{r:?}");
    let i = r.interpret.unwrap();
    assert_eq!(i.source, "config");
    assert_eq!(
        i.argv[1..],
        ["-p", "--model", "claude-haiku-4-5-20251001", "--max-turns", "1"]
    );

    // A typo is refused with the file named; the daemon keeps its defaults.
    std::fs::write(home.config_file(), "assistants:\n  interpet: claude\n").unwrap();
    let r: ConfigReport = c.ok("config.get", json!(null)).await;
    assert_eq!(r.errors.len(), 1, "{r:?}");
    assert!(
        r.errors[0].contains("config.yaml") && r.errors[0].contains("unknown field `interpet`"),
        "{r:?}"
    );
    assert_eq!(
        r.interpret.map(|i| i.source),
        Some("default".into()),
        "the defaults stand"
    );

    // An agent the registry does not know, or a model it cannot pass.
    std::fs::write(home.config_file(), "assistants:\n  interpret: aider\n").unwrap();
    let r: ConfigReport = c.ok("config.get", json!(null)).await;
    assert!(
        r.errors[0].contains("aider") && r.errors[0].contains("detect/aider.yaml"),
        "{r:?}"
    );
    assert!(r.interpret.is_none());
    std::fs::write(
        home.config_file(),
        "assistants:\n  interpret: { agent: cursor, model: x }\n",
    )
    .unwrap();
    let r: ConfigReport = c.ok("config.get", json!(null)).await;
    assert!(r.errors[0].contains("no model_flag"), "{r:?}");

    h.stop().await;
}
