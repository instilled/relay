//! The example flows in `examples/flows/`, held to the code: each loads, runs only scripts of its
//! own, every shell script parses, and nothing of anyone's own is in them — only placeholders.

use rl_flow::def::Does;
use rl_proto::action::Task;
use std::path::{Path, PathBuf};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/flows")
}

/// Each example's folder and its files, by name.
fn each() -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(examples()).unwrap() {
        let dir = e.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        for f in std::fs::read_dir(&dir).unwrap() {
            let p = f.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            files.push((name, std::fs::read_to_string(&p).unwrap()));
        }
        files.sort();
        out.push((dir.file_name().unwrap().to_string_lossy().into_owned(), files));
    }
    out.sort();
    out
}

#[test]
fn there_are_the_two() {
    let names: Vec<String> = each().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["morning-brief", "prod-logs"]);
}

#[test]
fn each_loads_and_runs_only_its_own_scripts() {
    for (name, files) in each() {
        let md = &files.iter().find(|(f, _)| f == "flow.md").expect("a flow.md").1;
        let def = rl_flow::def::check(&name, rl_flow::doc::read(md).unwrap(), &|_| true)
            .unwrap_or_else(|p| panic!("{name} does not load: {p:?}"));
        for s in &def.steps {
            let Does::Run(a) = &s.does else { continue };
            let Some(Task::Shell(line)) = &a.run else { continue };
            if line.ends_with(".sh") || line.ends_with(".py") {
                assert!(files.iter().any(|(f, _)| f == line), "{name}: {line} is not its own");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(examples().join(&name).join(line))
                        .unwrap()
                        .permissions()
                        .mode();
                    assert!(mode & 0o111 != 0, "{name}/{line} is not executable: {mode:o}");
                }
            }
        }
        for (f, text) in &files {
            assert!(!text.contains("{{"), "{name}/{f}: a blank left");
        }
    }
}

/// Parsed by the `/bin/sh` their first line names, and by bash too: macOS's `/bin/sh` is a bash,
/// which reads a `'` inside `"${x:?…}"` as a quote where dash does not.
#[test]
fn its_shell_scripts_parse() {
    for (name, files) in each() {
        for (f, _) in files.iter().filter(|(f, _)| f.ends_with(".sh")) {
            let p = examples().join(&name).join(f);
            for sh in ["/bin/sh", "bash"] {
                let out = std::process::Command::new(sh).arg("-n").arg(&p).output().unwrap();
                assert!(
                    out.status.success(),
                    "{name}/{f} under {sh}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }
}

#[test]
fn an_example_holds_nobodys_ids() {
    for (name, files) in each() {
        for (f, text) in &files {
            // A space is named through $BRIEF_SPACE, or as the placeholder.
            for (i, _) in text.match_indices("spaces/") {
                let id: String = text[i + "spaces/".len()..]
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                assert!(id.is_empty() || id == "AAAA", "{name}/{f}: the space spaces/{id}");
            }
            // A project and an organisation come from the step's env, or are the placeholder.
            for (key, placeholder) in [("GCP_PROJECT: ", "my-gcp-project"), ("GITHUB_ORG: ", "my-org")] {
                for (i, _) in text.match_indices(key) {
                    assert!(
                        text[i + key.len()..].starts_with(placeholder),
                        "{name}/{f}: {key} is not {placeholder}"
                    );
                }
            }
            for (flag, placeholder) in [("--project ", "my-gcp-project"), ("--owner ", "my-org")] {
                for (i, _) in text.match_indices(flag) {
                    let rest = &text[i + flag.len()..];
                    assert!(
                        rest.starts_with("\"$") || rest.starts_with(placeholder),
                        "{name}/{f}: {flag}{}",
                        &rest[..rest.len().min(20)]
                    );
                }
            }
            assert!(
                !text.contains("/Users/") && !text.contains("/home/"),
                "{name}/{f}: a path of someone's"
            );
        }
    }
}
