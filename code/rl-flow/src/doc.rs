//! `flow.md` read: the frontmatter that says what the flow is, and the prompt
//! under each `## ` heading that says what an agent step is asked.

use rl_proto::flow::FlowSpec;
use std::collections::BTreeMap;

/// `flow.md` as written: the frontmatter, and the prompt under each `## ` heading.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub spec: FlowSpec,
    /// `triage`, `review.apply` → the text under the heading, trimmed.
    pub sections: BTreeMap<String, String>,
    /// The line (1-based, in flow.md) each step's key is on, for errors.
    pub lines: BTreeMap<String, usize>,
}

/// What did not read, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl Problem {
    fn at(line: usize, message: &str) -> Self {
        Problem {
            line: Some(line),
            column: None,
            message: message.to_string(),
        }
    }
}

/// Reads `flow.md`: `---`, the frontmatter, `---`, then prose and sections.
pub fn read(text: &str) -> Result<Doc, Problem> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return Err(Problem::at(1, "a flow file starts with ---, then its frontmatter"));
    }
    let Some(end) = lines.iter().skip(1).position(|l| l.trim_end() == "---").map(|i| i + 1) else {
        return Err(Problem::at(1, "the frontmatter has no closing ---"));
    };
    // `lines[1..end]` is the frontmatter, so its line n is the file's line n + 1.
    let front = lines[1..end].join("\n");
    let spec: FlowSpec = serde_norway::from_str(&front).map_err(|e| yaml_problem(&e))?;
    let lines_of = step_lines(&lines[1..end], &spec);
    Ok(Doc {
        spec,
        sections: sections(&lines[end + 1..]),
        lines: lines_of,
    })
}

/// A YAML mistake as the file's line and column, with the locations taken out
/// of the words so they are not said twice (an error can name two: where it
/// was found and where the thing it was parsing began).
fn yaml_problem(e: &serde_norway::Error) -> Problem {
    let message = unplace(&e.to_string());
    match e.location() {
        Some(l) => Problem {
            line: Some(l.line() + 1),
            column: Some(l.column()),
            message,
        },
        None => Problem {
            line: None,
            column: None,
            message,
        },
    }
}

/// `text` without its ` at line N column M` marks.
fn unplace(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(" at line ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + " at line ".len()..];
        let digits = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (n, after) = (digits(tail), &tail[digits(tail)..]);
        match after.strip_prefix(" column ") {
            Some(c) if n > 0 && digits(c) > 0 => rest = &c[digits(c)..],
            _ => {
                out.push_str(" at line ");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The line each step's key is on: a two-space-indented `<name>:` under the
/// top-level `steps:`.
fn step_lines(front: &[&str], spec: &FlowSpec) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    let Some(start) = front.iter().position(|l| l.trim_end().strip_prefix("steps:").is_some()) else {
        return found;
    };
    for (i, l) in front.iter().enumerate().skip(start + 1) {
        let body = l.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if !l.starts_with(' ') {
            break;
        }
        let Some(name) = l.strip_prefix("  ").filter(|r| !r.starts_with(' ')) else {
            continue;
        };
        let key = name.split(':').next().unwrap_or("").trim_matches(['"', '\'']);
        if name.contains(':') && spec.steps.iter().any(|(s, _)| s == key) {
            found.entry(key.to_string()).or_insert(i + 2);
        }
    }
    found
}

/// The text under each `## <name>` line, trimmed. Text before the first
/// heading describes the flow and is not kept.
fn sections(body: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut open: Option<(&str, Vec<&str>)> = None;
    let mut close = |open: Option<(&str, Vec<&str>)>| {
        if let Some((name, text)) = open {
            out.insert(name.to_string(), text.join("\n").trim().to_string());
        }
    };
    for l in body {
        match heading(l) {
            Some(name) => {
                close(open.take());
                open = Some((name, Vec::new()));
            }
            None => {
                if let Some((_, text)) = open.as_mut() {
                    text.push(l);
                }
            }
        }
    }
    close(open);
    out
}

/// The name in a `## name` line, if it is one: `[a-z0-9][a-z0-9.-]*`.
fn heading(line: &str) -> Option<&str> {
    let name = line.strip_prefix("## ")?.trim_end();
    let mut chars = name.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-');
    ok.then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROD_LOGS: &str = "---\nin: ~/w\nwhen: every 30m\nsteps:\n  probe:\n    run: new-log-errors.sh\n  triage:\n    after: probe\n    run: { agent: claude }\n---\n\nAbout this flow.\n\n## triage\nAnalyse:\n\n{probe.out}\n\n## review.apply\nDo it.\n";

    #[test]
    fn the_frontmatter_and_the_sections_are_read() {
        let d = read(PROD_LOGS).unwrap();
        assert_eq!(d.spec.when.as_deref(), Some("every 30m"));
        assert_eq!(d.sections["triage"], "Analyse:\n\n{probe.out}");
        assert_eq!(d.sections["review.apply"], "Do it.");
        assert_eq!(d.lines["probe"], 5);
        assert_eq!(d.lines["triage"], 7);
    }

    #[test]
    fn a_yaml_mistake_is_said_at_its_line_in_the_file() {
        let p = read("---\nsteps:\n  probe:\n    run: [x\n---\n").unwrap_err();
        assert!(p.line.unwrap() >= 4, "{p:?}");
        assert!(!p.message.contains("line "), "the location is not said twice: {p:?}");
    }

    #[test]
    fn no_frontmatter_or_no_end_is_said() {
        assert!(read("## triage\nx\n").unwrap_err().message.contains("starts with ---"));
        assert!(read("---\nsteps: {}\n").unwrap_err().message.contains("closing ---"));
    }

    #[test]
    fn only_a_well_formed_heading_opens_a_section() {
        let d = read("---\nsteps: {}\n---\n## a-1\none\n## Not A Name\nstill one\n##nospace\n").unwrap();
        assert_eq!(d.sections.len(), 1);
        assert_eq!(d.sections["a-1"], "one\n## Not A Name\nstill one\n##nospace");
    }
}
