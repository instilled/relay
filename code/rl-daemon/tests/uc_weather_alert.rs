//! The source→reaction→decision wiring end to end on Relay's own primitives: a
//! schedule runs a probe, the probe `rl emit`s a `weather.alert`, a reaction
//! narrows by `where.data` and asks, and approving the question runs the action.
//! The probe here emits a canned alert (no network); what is tested is the
//! wiring Relay provides between a probe and an approved action, and that the
//! real `rl emit` CLI works.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::types::Run;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The `rl` binary sits beside `rld` in the target dir.
fn rl_bin() -> String {
    PathBuf::from(env!("CARGO_BIN_EXE_rld"))
        .with_file_name("rl")
        .to_string_lossy()
        .into_owned()
}

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

async fn wait_for_question(c: &mut Conn) -> Decision {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
        if let Some(d) = open.into_iter().next() {
            return d;
        }
        assert!(Instant::now() < deadline, "no question was raised");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_probe_emits_an_alert_that_asks_and_approving_it_acts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();

    // A schedule whose probe emits one alert (a stand-in for check-weather,
    // canned so the test needs no network).
    std::fs::write(
        home.schedules_file(),
        format!(
            "schedules:\n  \
               check-weather:\n    \
                 every: 1h\n    \
                 no_tty: true\n    \
                 cwd: {dir}\n    \
                 run:\n      \
                   - {rl}\n      \
                   - emit\n      \
                   - weather.alert\n      \
                   - --data\n      \
                   - condition=sunny\n      \
                   - --data\n      \
                   - place=Zurich\n",
            dir = dir.display(),
            rl = rl_bin(),
        ),
    )
    .unwrap();

    // On a sunny/rain-likely alert, ask; approving `note` runs the action.
    std::fs::write(
        home.reactions_file(),
        format!(
            "reactions:\n  \
               weather-advisory:\n    \
                 on: weather.alert\n    \
                 where: {{ data: {{ condition: [sunny, rain-likely] }} }}\n    \
                 throttle: '0'\n    \
                 cwd: {dir}\n    \
                 ask:\n      \
                   title: 'weather in {{data.place}}: {{data.condition}}'\n      \
                   options:\n        \
                     - key: note\n          \
                       label: Add a note\n          \
                       run: 'echo \"{{data.condition}} in {{data.place}}\" >> weather-notes.txt'\n        \
                     - key: skip\n          \
                       label: Skip\n          \
                       default: true\n",
            dir = dir.display(),
        ),
    )
    .unwrap();

    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // Fire the schedule now instead of waiting; the probe emits via the real CLI.
    let probe: Run = c.ok("schedule.run", json!({ "name": "check-weather" })).await;
    let done = c.wait_terminal(&probe.id).await;
    assert_eq!(done.exit_code, Some(0), "the probe (rl emit) ran clean");

    // The emitted alert reached the reaction, which asked, wearing the payload.
    let q = wait_for_question(&mut c).await;
    assert_eq!(q.title, "weather in Zurich: sunny", "the reaction filled {{data.*}}");
    assert_eq!(q.options.len(), 2);

    // Approving `note` runs the action.
    let taken: Decision = c.ok("decision.take", json!({ "id": q.id, "options": ["note"] })).await;
    assert_eq!(taken.outcome_runs.len(), 1);
    c.wait_terminal(&taken.outcome_runs[0]).await;
    assert_eq!(wait_for(&dir.join("weather-notes.txt")).await.trim(), "sunny in Zurich");

    h.stop().await;
}
