//! `rl report`: the written summary, as the report's `w` shows it.

use std::path::PathBuf;
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn rl_report_prints_the_written_summary() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let folder = tmp.path().join("relay");
    std::fs::create_dir(&folder).unwrap();
    let rl = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    let (ok, out, err) = rl(&[
        "lead",
        "post",
        "--source",
        "feedback",
        "--key",
        "pick",
        "--band",
        "needs_you",
        "--title",
        "pick a db",
        "--in",
        folder.to_str().unwrap(),
    ]);
    assert!(ok, "{out}{err}");

    let (ok, out, err) = rl(&["report", "--since", "3h"]);
    assert!(ok, "{out}{err}");
    let paras: Vec<&str> = out.trim_end().split("\n\n").collect();
    assert_eq!(paras[0], "While you were out (3h): 1 needs you, 1 to review.", "{out}");
    assert!(
        paras[1].starts_with("Needs you: pick a db (relay) waits for you, waiting since "),
        "{out}"
    );
    assert_eq!(paras.last().copied(), Some("relay: 1 needs you."), "{out}");

    // The flags of the retired prose are gone.
    let (ok, _, _) = rl(&["report", "--prose"]);
    assert!(!ok);

    assert!(rl(&["daemon", "stop"]).0);
}
