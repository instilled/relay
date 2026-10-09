//! A new flow in the panel: what `/flow new …` asks for, and a flow an assistant drafted, shown
//! before anything is written — its `flow.md`, what would stop it loading, and who drafted it.
//! Pure: the words read and a draft checked and said; the panel writes it on `enter`.

use super::theme;
use crate::flow::{NewFile, templates};
use ratatui::text::{Line, Span};
use std::path::Path;

pub const USAGE: &str = "/flow new <name> [words…] · /flow new <name> --from <template> [key=value…]";

/// What `/flow new` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// The bare template, in `$EDITOR`.
    Bare { name: String },
    /// A template, its blanks given as `key=value` words.
    From {
        name: String,
        template: String,
        given: Vec<String>,
    },
    /// A draft, from the words.
    Words { name: String, words: String },
}

impl Asked {
    pub fn name(&self) -> &str {
        match self {
            Asked::Bare { name } | Asked::From { name, .. } | Asked::Words { name, .. } => name,
        }
    }
}

/// The words after `/flow new`, read.
pub fn asked(words: &[&str]) -> Result<Asked, String> {
    let Some((name, rest)) = words.split_first() else {
        return Err(USAGE.to_string());
    };
    let name = name.to_string();
    Ok(match rest {
        [] => Asked::Bare { name },
        ["--from"] => return Err(format!("/flow new {name} --from which? {}", templates::names())),
        ["--from", template, given @ ..] => Asked::From {
            name,
            template: template.to_string(),
            given: given.iter().map(|g| g.to_string()).collect(),
        },
        words => Asked::Words {
            name,
            words: words.join(" "),
        },
    })
}

/// A template's files as the panel fills them: its blanks from `key=value` words, `in` the folder
/// `here`; what is missing is said as it would be typed.
pub fn template_files(template: &str, given: &[String], here: &str) -> Result<Vec<NewFile>, String> {
    let t = templates::find(template)?;
    let given = templates::given_in(t, given, here)?;
    let missing = templates::missing(t, &given);
    if !missing.is_empty() {
        return Err(templates::needs(t, &missing, ""));
    }
    templates::fill(t, &given)
}

/// A flow an assistant drafted, not written yet.
pub struct Drafted {
    /// Empty when the draft named none and the person did not either.
    pub name: String,
    pub text: String,
    pub agent: String,
    /// Why it would not load, each at its line; none, and enter may write it.
    pub problems: Vec<String>,
}

impl Drafted {
    /// `text` as `name`'s `flow.md`, checked as a reload checks a flow: `known` says whether a
    /// folder is a project Relay knows; `taken`, why a name may not be a new flow, if it may not.
    pub fn new(
        name: Option<String>,
        text: String,
        agent: String,
        known: &dyn Fn(&Path) -> bool,
        taken: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let name = name.unwrap_or_default();
        let mut problems = Vec::new();
        if name.is_empty() {
            problems.push("it names no flow · /flow new <name> <words> names one".to_string());
        } else if let Some(why) = taken(&name) {
            problems.push(why);
        }
        match rl_flow::doc::read(&text) {
            Err(p) => problems.push(at(p.line, &p.message)),
            Ok(doc) => {
                if let Err(found) = rl_flow::def::check(&name, doc, known) {
                    problems.extend(found.iter().map(|p| at(p.line, &p.message)));
                }
            }
        }
        Drafted {
            name,
            text,
            agent,
            problems,
        }
    }

    /// Whether enter may write it.
    pub fn ready(&self) -> bool {
        self.problems.is_empty()
    }

    /// The pane: whose it is and where it would go, what would stop it loading, then `flow.md`.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let name = if self.name.is_empty() {
            "(unnamed)"
        } else {
            self.name.as_str()
        };
        let mut v = vec![Line::from(vec![
            Span::styled(format!(" {name}"), theme::name()),
            Span::styled(
                format!("  drafted by {} · flows/{name}/flow.md", self.agent),
                theme::dim(),
            ),
        ])];
        for p in &self.problems {
            v.push(Line::from(Span::styled(format!(" ! {p}"), theme::bad())));
        }
        v.push(Line::raw(""));
        v.extend(self.text.lines().map(|l| Line::raw(format!(" {l}"))));
        v
    }

    /// What the keys do, for the footer.
    pub fn hint(&self) -> String {
        if self.ready() {
            "enter writes it and says yes · e opens it in $EDITOR first · esc lets it go".into()
        } else {
            "it would not load · e opens it in $EDITOR · esc lets it go".into()
        }
    }
}

/// A problem at its line of `flow.md`, when it has one.
fn at(line: Option<usize>, message: &str) -> String {
    match line {
        Some(l) => format!("flow.md:{l} · {message}"),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";

    #[test]
    fn what_flow_new_asks_for() {
        assert_eq!(asked(&[]).unwrap_err(), USAGE);
        assert_eq!(asked(&["pulse"]).unwrap(), Asked::Bare { name: "pulse".into() });
        assert_eq!(
            asked(&["logs", "--from", "prod-logs", "project=p"]).unwrap(),
            Asked::From {
                name: "logs".into(),
                template: "prod-logs".into(),
                given: vec!["project=p".into()],
            }
        );
        assert_eq!(
            asked(&["logs", "--from"]).unwrap_err(),
            "/flow new logs --from which? prod-logs, morning-brief"
        );
        let brief = asked(&["brief", "what", "my", "team", "did"]).unwrap();
        assert_eq!(
            brief,
            Asked::Words {
                name: "brief".into(),
                words: "what my team did".into()
            }
        );
        assert_eq!(brief.name(), "brief");
        assert_eq!(
            template_files("prod-logs", &[], "~/w/api").unwrap_err(),
            "prod-logs needs project=… (the GCP project whose logs are read, like my-prod-project)"
        );
        let files = template_files("prod-logs", &["project=p".into()], "~/w/api").unwrap();
        assert!(String::from_utf8_lossy(&files[0].bytes).contains("in: \"~/w/api\""));
    }

    #[test]
    fn a_draft_says_what_would_stop_it_loading() {
        let ok = Drafted::new(Some("hello".into()), HELLO.into(), "fakeai".into(), &|_| true, &|_| {
            None
        });
        assert!(ok.ready(), "{:?}", ok.problems);
        let text: Vec<String> = ok.lines().iter().map(|l| l.to_string()).collect();
        assert_eq!(text[0], " hello  drafted by fakeai · flows/hello/flow.md");
        assert!(text.contains(&"     run: echo hello".to_string()), "{text:#?}");
        assert_eq!(
            ok.hint(),
            "enter writes it and says yes · e opens it in $EDITOR first · esc lets it go"
        );

        let bad = Drafted::new(
            Some("x".into()),
            "---\nin: ~/nowhere\nsteps:\n  a:\n    after: nope\n    run: x\n---\n".into(),
            "fakeai".into(),
            &|_| false,
            &|_| None,
        );
        assert_eq!(
            bad.problems,
            [
                "flow.md:2 · in: ~/nowhere is not a project Relay knows — `rl add ~/nowhere`",
                "flow.md:4 · `a` comes after `nope`, which is no step",
            ]
        );
        assert_eq!(bad.hint(), "it would not load · e opens it in $EDITOR · esc lets it go");
        let lines: Vec<String> = bad.lines().iter().map(|l| l.to_string()).collect();
        assert_eq!(
            lines[1],
            " ! flow.md:2 · in: ~/nowhere is not a project Relay knows — `rl add ~/nowhere`"
        );

        let unnamed = Drafted::new(None, HELLO.into(), "fakeai".into(), &|_| true, &|_| None);
        assert_eq!(
            unnamed.problems,
            ["it names no flow · /flow new <name> <words> names one"]
        );
        assert!(unnamed.lines()[0].to_string().starts_with(" (unnamed)"));
        let taken = Drafted::new(Some("hello".into()), HELLO.into(), "fakeai".into(), &|_| true, &|n| {
            Some(format!("`{n}` is a flow already"))
        });
        assert_eq!(taken.problems, ["`hello` is a flow already"]);
        let broken = Drafted::new(
            Some("y".into()),
            "no frontmatter\n".into(),
            "fakeai".into(),
            &|_| true,
            &|_| None,
        );
        assert_eq!(
            broken.problems,
            ["flow.md:1 · a flow file starts with ---, then its frontmatter"]
        );
    }
}
