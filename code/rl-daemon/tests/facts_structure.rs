//! `facts.structure`: a run's recap is split into worked-on / questions /
//! next by one assistant call, once; with no assistant set, no recap, or the
//! split already kept, the stored facts come back as they are and no one is
//! asked.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_ledger::{Ledger, facts};
use rl_proto::RunFacts;
use serde_json::json;
use std::path::PathBuf;

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

fn home_with(dir: &std::path::Path, assistant: bool) -> Home {
    let home = Home::at(dir);
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    if assistant {
        std::fs::write(home.config_file(), "assistants:\n  summary: fakeai\n").unwrap();
    }
    home
}

async fn seed(home: &Home, run: &str, recap: Option<&str>) -> RunFacts {
    let f = RunFacts {
        run: run.into(),
        commits: 2,
        recap: recap.map(str::to_string),
        ..RunFacts::default()
    };
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let kept = f.clone();
    ledger.call(move |c| facts::set(c, &kept)).await.unwrap();
    f
}

async fn structure(c: &mut Conn, run: &str) -> RunFacts {
    c.ok("facts.structure", json!({ "run": run })).await
}

#[tokio::test]
async fn structure_is_noop_without_recap_or_when_off_and_splits_once_when_on() {
    let tmp = tempfile::tempdir().unwrap();
    let prompt = tmp.path().join("prompt.txt");
    let answer = tmp.path().join("answer.txt");
    std::fs::write(
        &answer,
        "Worked on:\nThe search index.\nOpen questions:\nKeep the flag?\nNext:\nShip it.\n",
    )
    .unwrap();
    // SAFETY: this test binary has this one test, alone in its process.
    unsafe {
        std::env::set_var("FAKE_AGENT_READS", "1");
        std::env::set_var("FAKE_AGENT_ANSWER", &answer);
        std::env::set_var("FAKE_AGENT_PROMPT", &prompt);
    }

    // Off: no assistant set, a recap is left as it is and nobody is asked.
    {
        let home = home_with(&tmp.path().join("off"), false);
        home.ensure_dirs().unwrap();
        let Some(h) = Harness::on(home.clone()) else {
            eprintln!("skipping: rl-session not built");
            return;
        };
        let mut c = Conn::open(&home).await;
        let seeded = seed(&home, "r1", Some("fixed search")).await;
        assert_eq!(structure(&mut c, "r1").await, seeded);
        // Nothing stored: empty facts of that run, also no call.
        assert_eq!(structure(&mut c, "nope").await.run, "nope");
        assert!(!prompt.exists(), "no assistant was asked");
        h.stop().await;
    }

    // On, but no recap: unchanged, no call.
    {
        let home = home_with(&tmp.path().join("on"), true);
        home.ensure_dirs().unwrap();
        let h = Harness::on(home.clone()).unwrap();
        let mut c = Conn::open(&home).await;
        let bare = seed(&home, "r0", None).await;
        assert_eq!(structure(&mut c, "r0").await, bare);
        assert!(!prompt.exists(), "no recap, no call");

        // On with a recap: one call, split and kept.
        seed(&home, "r2", Some("fixed search")).await;
        let got = structure(&mut c, "r2").await;
        assert_eq!(got.worked_on.as_deref(), Some("The search index."));
        assert_eq!(got.questions.as_deref(), Some("Keep the flag?"));
        assert_eq!(got.next.as_deref(), Some("Ship it."));
        assert_eq!(got.commits, 2, "hard facts stay");
        assert!(prompt.exists(), "the assistant was asked");

        // Cached: asked once, not again.
        std::fs::remove_file(&prompt).unwrap();
        assert_eq!(structure(&mut c, "r2").await, got);
        assert!(!prompt.exists(), "the kept split is not asked again");
        h.stop().await;
    }
    unsafe {
        std::env::remove_var("FAKE_AGENT_READS");
        std::env::remove_var("FAKE_AGENT_ANSWER");
        std::env::remove_var("FAKE_AGENT_PROMPT");
    }
}
