//! Drift, end to end: a stand-in harness that changed is noticed and kept
//! under `$RELAY_HOME/drift`; `rl setup --check` and `rl doctor` say so;
//! `rl report` shows the issue and posts it through a stand-in `gh` only on
//! a yes — never the real one, never the network.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A stand-in `gh`: every run's words appended to `gh.log` (one run a
/// line, words joined by ` | `), a `--body-file`'s text kept in `gh.body`.
/// `gh.unauthed` beside it: `auth status` fails. `gh.existing`: an open
/// issue is found. `gh.nolabel`: the repository has no `drift` label.
const GH: &str = r#"#!/bin/sh
d=$(dirname "$0")
line=""
for a in "$@"; do line="$line | $a"; done
echo "${line# | }" >> "$d/gh.log"
prev=""
for a in "$@"; do
  [ "$prev" = --body-file ] && cat "$a" > "$d/gh.body"
  prev="$a"
done
case "$1 $2" in
  "auth status") [ -e "$d/gh.unauthed" ] && exit 1; exit 0 ;;
  "issue list") [ -e "$d/gh.existing" ] && echo '[{"number":7,"url":"https://github.com/instilled/relay/issues/7"}]' || echo '[]'; exit 0 ;;
  "issue create")
    case "$*" in *"--label drift"*) [ -e "$d/gh.nolabel" ] && { echo "could not add label: 'drift' not found" >&2; exit 1; } ;; esac
    echo "https://github.com/instilled/relay/issues/42"; exit 0 ;;
  "issue comment") echo "https://github.com/instilled/relay/issues/7#issuecomment-1"; exit 0 ;;
esac
exit 1
"#;

/// A stand-in `claude` that changed: `--version` says 2.1.999, its agents
/// listing is no longer a list.
const CLAUDE: &str =
    "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.999 (Claude Code)'; exit 0; }\necho '{\"agents\": []}'\n";

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        Env { _tmp: tmp, home, bin }
    }

    fn put(&self, name: &str, script: &str) {
        let p = self.bin.join(name);
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn relay(&self) -> PathBuf {
        self.home.join(".relay")
    }

    fn drift(&self) -> PathBuf {
        self.relay().join("drift")
    }

    fn rl(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .env("HOME", &self.home)
            .env("RELAY_HOME", self.relay())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("RL_SESSION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn gh_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("gh.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// One drift kept, as Relay would have noticed it.
    fn seed(&self) {
        let n = rl_detect::drift::Noticed::new(
            "codex",
            Some(Path::new("/opt/codex")),
            "codex.schema.thread/name/set",
            "thread/name/set gone, rename stays in Relay",
        );
        rl::drift::record_as(&self.drift(), &n, "0.170.0", "2026-11-02T10:00:00Z".parse().unwrap()).unwrap();
    }

    fn reported(&self) -> Option<String> {
        rl::drift::load(&self.drift())[0].reported.clone()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_harness_that_changed_is_kept_and_said_by_setup_check_and_doctor() {
    let e = Env::new();
    e.put("claude", CLAUDE);
    let ls = e.rl(&["ls"], "");
    assert!(ls.status.success(), "{ls:?}");
    let events = rl::drift::load(&e.drift());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].check, "claude.agents_json");
    assert_eq!(events[0].version, "2.1.999");
    assert_eq!(events[0].evidence.as_deref(), Some("not a list"));
    e.rl(&["ls"], "");
    assert_eq!(rl::drift::load(&e.drift())[0].count, 2, "the same, counted");

    let check = e.rl(&["setup", "--check", "--harness", "codex"], "");
    assert_eq!(check.status.code(), Some(1), "the exit status stays setup's");
    assert!(
        out(&check).contains(
            "drift:\n  claude 2.1.999: `claude agents --json` changed shape; sessions it lists may be missing [claude.agents_json] · rl report\n"
        ),
        "{}",
        out(&check)
    );
    let doctor = e.rl(&["doctor"], "");
    assert!(doctor.status.success(), "{doctor:?}");
    let said = out(&doctor);
    assert!(said.contains("claude 2.1.999 · "), "{said}");
    assert!(
        said.contains("[claude.agents_json] ×2 · not reported · rl report"),
        "{said}"
    );
}

#[test]
fn a_yes_posts_the_issue_through_gh_and_marks_it_reported() {
    let e = Env::new();
    e.put("gh", GH);
    e.seed();
    let o = e.rl(&["report"], "y\n");
    assert!(o.status.success(), "{o:?}");
    let said = out(&o);
    assert!(
        said.contains(
            "drift: codex 0.170.0 — thread/name/set gone, rename stays in Relay [codex.schema.thread/name/set]"
        ),
        "{said}"
    );
    assert!(
        said.contains("Post this as an issue on instilled/relay? [y/N]"),
        "{said}"
    );
    assert!(
        said.contains("posted: https://github.com/instilled/relay/issues/42"),
        "{said}"
    );
    let log = e.gh_log();
    assert_eq!(log[0], "auth | status");
    assert_eq!(
        log[1],
        "issue | list | --repo | instilled/relay | --search | codex.schema.thread/name/set 0.170.0 in:title | --state | open | --json | number,url"
    );
    assert!(
        log[2].starts_with("issue | create | --repo | instilled/relay | --title | drift: codex 0.170.0"),
        "{log:?}"
    );
    assert!(log[2].ends_with("--label | drift"), "{log:?}");
    assert_eq!(log.len(), 3);
    let body = std::fs::read_to_string(e.bin.join("gh.body")).unwrap();
    assert!(body.contains("### What Relay expected"), "{body}");
    assert!(body.ends_with("Reported from `rl report`.\n"), "{body}");
    assert_eq!(
        e.reported().as_deref(),
        Some("https://github.com/instilled/relay/issues/42")
    );

    let again = e.rl(&["report"], "y\n");
    assert!(out(&again).contains("nothing to report"), "reported: not offered again");
}

#[test]
fn no_or_nothing_posts_nothing() {
    for answer in ["n\n", "\n", ""] {
        let e = Env::new();
        e.put("gh", GH);
        e.seed();
        let o = e.rl(&["report"], answer);
        assert!(o.status.success(), "{o:?}");
        assert!(out(&o).contains("nothing posted"), "{answer:?}: {}", out(&o));
        assert!(
            !e.gh_log().iter().any(|l| l.starts_with("issue | create")),
            "{answer:?}"
        );
        assert_eq!(e.reported(), None, "still to report");
    }
}

#[test]
fn a_repository_without_the_label_gets_the_issue_without_it() {
    let e = Env::new();
    e.put("gh", GH);
    std::fs::write(e.bin.join("gh.nolabel"), "").unwrap();
    e.seed();
    let o = e.rl(&["report"], "yes\n");
    assert!(
        out(&o).contains("posted: https://github.com/instilled/relay/issues/42"),
        "{o:?}"
    );
    let creates: Vec<String> = e
        .gh_log()
        .into_iter()
        .filter(|l| l.starts_with("issue | create"))
        .collect();
    assert_eq!(creates.len(), 2);
    assert!(!creates[1].contains("--label"));
}

#[test]
fn an_open_issue_for_the_same_gets_a_comment_instead() {
    let e = Env::new();
    e.put("gh", GH);
    std::fs::write(e.bin.join("gh.existing"), "").unwrap();
    e.seed();
    let o = e.rl(&["report"], "y\n");
    let said = out(&o);
    assert!(
        said.contains("An open issue reports this already: https://github.com/instilled/relay/issues/7"),
        "{said}"
    );
    assert!(
        said.contains("Add a comment with this count to #7 on instilled/relay? [y/N]"),
        "{said}"
    );
    let log = e.gh_log();
    assert!(
        log.iter()
            .any(|l| l.starts_with("issue | comment | 7 | --repo | instilled/relay")),
        "{log:?}"
    );
    assert!(!log.iter().any(|l| l.starts_with("issue | create")), "{log:?}");
    let body = std::fs::read_to_string(e.bin.join("gh.body")).unwrap();
    assert!(body.starts_with("Seen again with relay"), "{body}");
    assert_eq!(
        e.reported().as_deref(),
        Some("https://github.com/instilled/relay/issues/7#issuecomment-1")
    );
}

#[test]
fn without_gh_logged_in_a_link_is_shown() {
    let e = Env::new();
    e.put("gh", GH);
    std::fs::write(e.bin.join("gh.unauthed"), "").unwrap();
    e.seed();
    let o = e.rl(&["report"], "y\n");
    let said = out(&o);
    assert!(said.contains("gh is not set up here"), "{said}");
    assert!(
        said.contains("https://github.com/instilled/relay/issues/new?title=drift%3A%20codex%200.170.0"),
        "{said}"
    );
    assert_eq!(e.gh_log(), ["auth | status"], "nothing else asked of gh");
    assert_eq!(e.reported().as_deref(), Some("link shown"));
}

#[test]
fn a_dry_run_shows_the_issue_and_runs_nothing() {
    let e = Env::new();
    e.put("gh", GH);
    e.seed();
    let o = e.rl(&["report", "--dry-run"], "y\n");
    let said = out(&o);
    assert!(
        said.contains("### What happened\n\nthread/name/set gone, rename stays in Relay"),
        "{said}"
    );
    assert!(said.contains("(dry run: nothing posted)"), "{said}");
    assert!(!said.contains("[y/N]"), "nothing asked");
    assert!(e.gh_log().is_empty(), "gh not run at all");
    assert_eq!(e.reported(), None);

    let d = e.rl(&["report", "--dismiss"], "");
    assert_eq!(out(&d), "dismissed 1\n");
    assert!(out(&e.rl(&["report"], "y\n")).contains("nothing to report"));
}
