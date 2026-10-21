//! `rl tag <source> <name>` and `rl tag --clear <source>`: the tag shows up on the report, and goes.

use std::path::PathBuf;
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn tag_sets_replaces_and_clears_a_sources_tag() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
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
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let tags = || {
        let (ok, out, err) = rl(&["report", "--json"]);
        assert!(ok, "{out}{err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["tags"].clone()
    };

    let (ok, out, err) = rl(&["tag", "claude", "dev"]);
    assert!(ok, "{out}{err}");
    assert_eq!(tags(), serde_json::json!([{"source": "claude", "tag": "dev"}]));

    let (ok, out, err) = rl(&["tag", "claude", "ops"]);
    assert!(ok, "{out}{err}");
    assert_eq!(tags(), serde_json::json!([{"source": "claude", "tag": "ops"}]));

    let (ok, out, err) = rl(&["tag", "--clear", "claude"]);
    assert!(ok, "{out}{err}");
    assert_eq!(tags(), serde_json::json!([]));

    // Neither a name nor --clear is a mistake, not a silent no-op.
    let (ok, _, err) = rl(&["tag", "claude"]);
    assert!(!ok);
    assert!(err.contains("give a name"), "{err}");

    assert!(rl(&["daemon", "stop"]).0);
}
