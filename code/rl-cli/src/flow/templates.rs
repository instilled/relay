//! The flows Relay ships to start from: `rl flow new <name> --from <template>`. Each is a folder
//! — `flow.md` and its scripts, under `rl-cli/templates/<template>/`, compiled in — with named
//! blanks, `{{project}}`, filled when the flow is written. Nothing of anyone's own is in one: what
//! is particular to a person is a blank, and reaches a script through its step's `env:`.

use super::new::NewFile;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a template leaves for the person: `{{key}}` wherever it goes.
#[derive(Debug)]
pub struct Blank {
    pub key: &'static str,
    /// What it is, as a question would ask it.
    pub what: &'static str,
    /// What one looks like.
    pub like: &'static str,
}

/// One of a template's files.
#[derive(Debug)]
pub struct File {
    pub path: &'static str,
    pub text: &'static str,
    /// A script a step runs: written executable.
    pub exec: bool,
}

#[derive(Debug)]
pub struct Template {
    pub name: &'static str,
    /// What it does, in a line: for the context pack.
    pub about: &'static str,
    pub blanks: &'static [Blank],
    /// `flow.md` first.
    pub files: &'static [File],
}

/// The blank a template's `in` takes: the folder it was asked from, unless given.
pub const IN: &str = "in";

pub const ALL: &[Template] = &[PROD_LOGS, MORNING_BRIEF];

const PROD_LOGS: Template = Template {
    name: "prod-logs",
    about: "every 30m, errors a GCP project logged that were never reported; an agent proposes one \
            action, and with your yes an agent does it in the checkout",
    blanks: &[
        Blank {
            key: IN,
            what: "the checkout the agents read and change",
            like: "~/Projects/api",
        },
        Blank {
            key: "project",
            what: "the GCP project whose logs are read",
            like: "my-prod-project",
        },
    ],
    files: &[
        File {
            path: "flow.md",
            text: include_str!("../../templates/prod-logs/flow.md"),
            exec: false,
        },
        File {
            path: "new-log-errors.sh",
            text: include_str!("../../templates/prod-logs/new-log-errors.sh"),
            exec: true,
        },
    ],
};

const MORNING_BRIEF: Template = Template {
    name: "morning-brief",
    about: "weekday mornings, what each teammate did yesterday from Google Chat, Notion and GitHub, \
            briefed by an agent, posted to a Chat space and heading your catch-up",
    blanks: &[
        Blank {
            key: "space",
            what: "the Google Chat space the brief is posted to",
            like: "spaces/AAAAbcdEfgh",
        },
        Blank {
            key: "org",
            what: "the GitHub organisation whose pull requests are read",
            like: "my-org",
        },
    ],
    files: &[
        File {
            path: "flow.md",
            text: include_str!("../../templates/morning-brief/flow.md"),
            exec: false,
        },
        File {
            path: "gchat.sh",
            text: include_str!("../../templates/morning-brief/gchat.sh"),
            exec: true,
        },
        File {
            path: "notion.py",
            text: include_str!("../../templates/morning-brief/notion.py"),
            exec: true,
        },
        File {
            path: "github.sh",
            text: include_str!("../../templates/morning-brief/github.sh"),
            exec: true,
        },
        File {
            path: "days.sh",
            text: include_str!("../../templates/morning-brief/days.sh"),
            exec: true,
        },
        File {
            path: "window.sh",
            text: include_str!("../../templates/morning-brief/window.sh"),
            exec: false,
        },
        File {
            path: "post.sh",
            text: include_str!("../../templates/morning-brief/post.sh"),
            exec: true,
        },
    ],
};

/// The template by its name.
pub fn find(name: &str) -> Result<&'static Template, String> {
    ALL.iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("`{name}` is not a template: {}", names()))
}

/// Every template's name, in the order shipped.
pub fn names() -> String {
    ALL.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

/// The blanks given as `key=value` words, and `in`, when the template has one and it is not
/// given, the folder `here` (as a path is shown: `~/…`). `named`: `here` was named (`--in`), so
/// a template with no `in` refuses it, as it refuses a key that is not one of its blanks.
pub fn given_in<S: AsRef<str>>(
    t: &Template,
    words: &[S],
    here: &str,
    named: bool,
) -> Result<BTreeMap<String, String>, String> {
    if named && !t.blanks.iter().any(|b| b.key == IN) {
        let keys: Vec<&str> = t.blanks.iter().map(|b| b.key).collect();
        return Err(format!(
            "--in: `{IN}` is not a blank of {}: {}",
            t.name,
            keys.join(", ")
        ));
    }
    let mut given = BTreeMap::new();
    for w in words {
        let w = w.as_ref();
        let Some((k, v)) = w.split_once('=') else {
            return Err(format!("`{w}` is not key=value"));
        };
        given.insert(k.trim().to_string(), v.trim().to_string());
    }
    if t.blanks.iter().any(|b| b.key == IN) {
        given.entry(IN.to_string()).or_insert_with(|| here.to_string());
    }
    Ok(given)
}

/// The blanks not given yet.
pub fn missing<'a>(t: &'a Template, given: &BTreeMap<String, String>) -> Vec<&'a Blank> {
    t.blanks.iter().filter(|b| !given.contains_key(b.key)).collect()
}

/// What a template still needs, each as it would be given: `prefix` is `--set ` on the command
/// line and nothing in the panel.
pub fn needs(t: &Template, missing: &[&Blank], prefix: &str) -> String {
    let each: Vec<String> = missing
        .iter()
        .map(|b| format!("{prefix}{}=… ({}, like {})", b.key, b.what, b.like))
        .collect();
    format!("{} needs {}", t.name, each.join(" · "))
}

/// A blank's value: something, and nothing that would end the YAML string it goes into (`"`,
/// `\`), make a placeholder (`{`, `}`) or break a line.
pub fn value(key: &str, v: &str) -> Result<(), String> {
    let bad = v.trim().is_empty() || v.chars().any(|c| c.is_control() || matches!(c, '"' | '\\' | '{' | '}'));
    if bad {
        return Err(format!(
            "`{key}`: a blank takes a word or a path, without \" \\ {{ or }}"
        ));
    }
    Ok(())
}

/// The template's files, every blank filled from `given` — each blank given, each value good,
/// and nothing given that is not a blank — or why not, before anything is filled.
pub fn fill(t: &Template, given: &BTreeMap<String, String>) -> Result<Vec<NewFile>, String> {
    if let Some(k) = given.keys().find(|k| !t.blanks.iter().any(|b| b.key == k.as_str())) {
        let keys: Vec<&str> = t.blanks.iter().map(|b| b.key).collect();
        return Err(format!("`{k}` is not a blank of {}: {}", t.name, keys.join(", ")));
    }
    let gone = missing(t, given);
    if !gone.is_empty() {
        return Err(needs(t, &gone, ""));
    }
    for (k, v) in given {
        value(k, v)?;
    }
    Ok(t.files
        .iter()
        .map(|f| {
            let mut text = f.text.to_string();
            for (k, v) in given {
                text = text.replace(&format!("{{{{{k}}}}}"), v);
            }
            NewFile {
                path: PathBuf::from(f.path),
                bytes: text.into_bytes(),
                exec: f.exec,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_flow::def::Does;
    use rl_proto::action::Task;
    use std::path::Path;

    /// Each blank filled with what one looks like.
    fn examples(t: &Template) -> BTreeMap<String, String> {
        t.blanks
            .iter()
            .map(|b| (b.key.to_string(), b.like.to_string()))
            .collect()
    }

    #[test]
    fn every_template_filled_loads_and_runs_only_its_own_scripts() {
        assert_eq!(names(), "prod-logs, morning-brief");
        for t in ALL {
            let files = fill(t, &examples(t)).unwrap();
            let md = files
                .iter()
                .find(|f| f.path == Path::new("flow.md"))
                .expect("a flow.md");
            let text = std::str::from_utf8(&md.bytes).unwrap();
            assert!(!text.contains("{{"), "{}: a blank left in flow.md", t.name);
            let def = rl_flow::def::check(t.name, rl_flow::doc::read(text).unwrap(), &|_| true)
                .unwrap_or_else(|p| panic!("{} does not load: {p:?}", t.name));
            for s in &def.steps {
                let Does::Run(a) = &s.does else { continue };
                let Some(Task::Shell(line)) = &a.run else { continue };
                if line.ends_with(".sh") || line.ends_with(".py") {
                    let script = files.iter().find(|f| f.path == Path::new(line));
                    assert!(
                        script.is_some_and(|f| f.exec),
                        "{}: {line} is not its own, executable",
                        t.name
                    );
                }
            }
            for f in &files {
                assert!(
                    !String::from_utf8_lossy(&f.bytes).contains("{{"),
                    "{}: a blank left in {}",
                    t.name,
                    f.path.display()
                );
            }
        }
    }

    #[test]
    fn a_template_holds_nobodys_ids() {
        for t in ALL {
            for f in t.files {
                // A space is named only through $BRIEF_SPACE or a blank, never written out.
                for (i, _) in f.text.match_indices("spaces/") {
                    let next = f.text[i + "spaces/".len()..].chars().next();
                    assert!(
                        !next.is_some_and(|c| c.is_ascii_alphanumeric()),
                        "{}/{}: a space's id",
                        t.name,
                        f.path
                    );
                }
                // A project and an organisation come from the step's env.
                for flag in ["--project ", "--owner "] {
                    for (i, _) in f.text.match_indices(flag) {
                        let rest = &f.text[i + flag.len()..];
                        assert!(
                            rest.starts_with("\"$") || rest.starts_with("{{"),
                            "{}/{}: {flag}{}",
                            t.name,
                            f.path,
                            &rest[..rest.len().min(20)]
                        );
                    }
                }
                assert!(
                    !f.text.contains("/Users/") && !f.text.contains("/home/"),
                    "{}/{}: a path of someone's",
                    t.name,
                    f.path
                );
            }
        }
    }

    /// Parsed by the `/bin/sh` their first line names, and by bash too: macOS's `/bin/sh` is a
    /// bash, which reads a `'` inside `"${x:?…}"` as a quote where dash does not.
    #[test]
    fn its_shell_scripts_parse() {
        let dir = tempfile::tempdir().unwrap();
        for t in ALL {
            for f in t.files.iter().filter(|f| f.path.ends_with(".sh")) {
                let p = dir.path().join(f.path);
                std::fs::write(&p, f.text).unwrap();
                for sh in ["/bin/sh", "bash"] {
                    let out = std::process::Command::new(sh).arg("-n").arg(&p).output().unwrap();
                    assert!(
                        out.status.success(),
                        "{}/{} under {sh}: {}",
                        t.name,
                        f.path,
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }
    }

    #[test]
    fn a_blank_is_checked_before_anything_is_filled() {
        let t = find("prod-logs").unwrap();
        assert_eq!(
            find("nope").unwrap_err(),
            "`nope` is not a template: prod-logs, morning-brief"
        );
        let mut given = given_in(t, &["project=my-prod"], "~/w/api", false).unwrap();
        assert_eq!(
            given.get("in").map(String::as_str),
            Some("~/w/api"),
            "in: the folder here"
        );
        assert_eq!(
            given_in(t, &["in=/w/x", "project=p"], "~/w/api", false).unwrap()["in"],
            "/w/x",
            "in, given, wins"
        );
        assert!(fill(t, &given).is_ok());
        assert_eq!(
            given_in(t, &["project"], "~", false).unwrap_err(),
            "`project` is not key=value"
        );
        // A folder named for a template with no `in` is refused, as a key not its blank is.
        assert_eq!(given_in(t, &["project=p"], "/w/x", true).unwrap()["in"], "/w/x");
        let brief = find("morning-brief").unwrap();
        assert_eq!(
            given_in(brief, &["space=s", "org=o"], "/w/x", true).unwrap_err(),
            "--in: `in` is not a blank of morning-brief: space, org"
        );
        assert!(
            !given_in(brief, &["space=s", "org=o"], "/w/x", false)
                .unwrap()
                .contains_key("in"),
            "here, not named: no in"
        );

        given.remove("project");
        let gone = missing(t, &given);
        assert_eq!(
            needs(t, &gone, "--set "),
            "prod-logs needs --set project=… (the GCP project whose logs are read, like my-prod-project)"
        );
        assert_eq!(fill(t, &given).unwrap_err(), needs(t, &gone, ""));

        for bad in ["my \"prod\"", "", "  ", "a\\b", "{probe.out}", "a\nb"] {
            given.insert("project".into(), bad.into());
            let e = fill(t, &given).unwrap_err();
            assert!(e.starts_with("`project`:"), "{bad:?}: {e}");
        }
        given.insert("project".into(), "p".into());
        given.insert("zone".into(), "x".into());
        assert_eq!(
            fill(t, &given).unwrap_err(),
            "`zone` is not a blank of prod-logs: in, project"
        );
    }

    #[test]
    fn a_blank_goes_wherever_it_is_named() {
        let t = find("morning-brief").unwrap();
        let given = BTreeMap::from([
            ("space".to_string(), "spaces/AAAA1".to_string()),
            ("org".to_string(), "acme".to_string()),
        ]);
        let files = fill(t, &given).unwrap();
        let md = String::from_utf8(files[0].bytes.clone()).unwrap();
        assert_eq!(
            files[0].path,
            Path::new("flow.md"),
            "flow.md first: what the editor opens"
        );
        assert_eq!(md.matches("BRIEF_SPACE: \"spaces/AAAA1\"").count(), 2, "{md}");
        assert!(
            md.contains("GITHUB_ORG: \"acme\"") && md.contains("GitHub (acme)"),
            "{md}"
        );
        assert!(
            !files.iter().find(|f| f.path == Path::new("window.sh")).unwrap().exec,
            "sourced, not run"
        );
    }
}
