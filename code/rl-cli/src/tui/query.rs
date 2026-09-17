//! What a search says: plain words, and a few `name:value` tokens that pick
//! by what a thing is rather than what it is called. The list and the graph
//! both narrow to what it hits.
//!
//! - words match anywhere — title, project, CLI, summary — and every word
//!   must match somewhere;
//! - `in:` a project, by name or folder;
//! - `is:` a state — `failed`, `going`, `done`, `stopped`, `needs`, `pinned`, `new`;
//! - `kind:` what it is — `agent`, `shell`, `command`, `job`, `reaction`, `question`;
//! - `by:` the schedule or reaction behind it.
//!
//! Values match by prefix (`is:fail`), so the list narrows as you type.
//!
//! Pure: parsing, matching and completing. Nothing about the panel.

/// The token names, in the order completion offers them.
pub const TOKENS: [&str; 4] = ["in:", "is:", "kind:", "by:"];

/// What `is:` takes.
pub const STATES: [&str; 7] = ["failed", "going", "done", "stopped", "needs", "pinned", "new"];

/// What `kind:` takes.
pub const KINDS: [&str; 6] = ["agent", "shell", "command", "job", "reaction", "question"];

/// A search, read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub words: Vec<String>,
    within: Vec<String>,
    is: Vec<String>,
    kind: Vec<String>,
    by: Vec<String>,
}

/// One thing the search is put to, described in the words it matches on.
#[derive(Debug, Default)]
pub struct Subject<'a> {
    /// What it is called and says: title, CLI, summary.
    pub text: Vec<&'a str>,
    /// Its project: the name and the folder.
    pub project: Vec<&'a str>,
    /// Its states, from `STATES`.
    pub is: Vec<&'static str>,
    /// What it is, from `KINDS` (or a graph node's own kinds).
    pub kind: Vec<&'static str>,
    /// The rule behind it.
    pub by: Option<&'a str>,
}

impl Query {
    pub fn parse(s: &str) -> Query {
        let mut q = Query::default();
        for w in s.split_whitespace() {
            let w = w.to_lowercase();
            let into = match w.split_once(':') {
                Some(("in", v)) => Some((&mut q.within, v)),
                Some(("is", v)) => Some((&mut q.is, v)),
                Some(("kind", v)) => Some((&mut q.kind, v)),
                Some(("by", v)) => Some((&mut q.by, v)),
                _ => None,
            };
            match into {
                // A token still being typed (`is:`) asks for nothing yet.
                Some((_, "")) => {}
                Some((list, v)) => list.push(v.to_string()),
                None => q.words.push(w),
            }
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.within.is_empty() && !self.picks_rows()
    }

    /// Whether it asks about what things are — state, kind, rule — which a
    /// project header on its own is none of.
    pub fn picks_rows(&self) -> bool {
        !(self.is.is_empty() && self.kind.is_empty() && self.by.is_empty())
    }

    pub fn hits(&self, s: &Subject) -> bool {
        let has = |hay: &str, needle: &str| hay.to_lowercase().contains(needle);
        self.words
            .iter()
            .all(|w| s.text.iter().chain(&s.project).any(|t| has(t, w)))
            && self.within.iter().all(|w| s.project.iter().any(|t| has(t, w)))
            && self.is.iter().all(|v| s.is.iter().any(|x| x.starts_with(v.as_str())))
            && self
                .kind
                .iter()
                .all(|v| s.kind.iter().any(|x| x.starts_with(v.as_str())))
            && self.by.iter().all(|v| s.by.is_some_and(|b| has(b, v)))
    }

    /// A project header with nothing under it that hit: shown when the search
    /// names it and asks nothing about rows.
    pub fn hits_project(&self, project: &[&str]) -> bool {
        !self.picks_rows()
            && self.hits(&Subject {
                project: project.to_vec(),
                ..Subject::default()
            })
    }
}

/// What `tab` offers for the word at the end of a search: the token names, or
/// a token's values — `in:` the projects, `by:` the rules.
pub fn complete(word: &str, projects: &[String], rules: &[String]) -> Vec<String> {
    let w = word.to_lowercase();
    let Some((name, v)) = w.split_once(':') else {
        return TOKENS
            .iter()
            .filter(|t| t.starts_with(&w))
            .map(|t| t.to_string())
            .collect();
    };
    let values: Vec<String> = match name {
        "is" => STATES.iter().map(|s| s.to_string()).collect(),
        "kind" => KINDS.iter().map(|s| s.to_string()).collect(),
        "in" => projects.to_vec(),
        "by" => rules.to_vec(),
        _ => return vec![],
    };
    let mut out: Vec<String> = values
        .into_iter()
        .filter(|x| x.to_lowercase().starts_with(v))
        .map(|x| format!("{name}:{x}"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Where the words of `text` sit, as byte ranges, merged: what a row lights up.
pub fn marks(text: &str, words: &[String]) -> Vec<(usize, usize)> {
    let lower = text.to_lowercase();
    // Lowercasing can change byte lengths; only mark when it did not.
    if lower.len() != text.len() {
        return vec![];
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for w in words.iter().filter(|w| !w.is_empty()) {
        let mut from = 0;
        while let Some(i) = lower[from..].find(w.as_str()) {
            let at = from + i;
            spans.push((at, at + w.len()));
            from = at + w.len();
        }
    }
    spans.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<'a>(title: &'a str, project: &'a str) -> Subject<'a> {
        Subject {
            text: vec![title],
            project: vec![project, "/home/f/api"],
            is: vec!["failed"],
            kind: vec!["command", "job"],
            by: Some("nightly"),
        }
    }

    #[test]
    fn every_word_must_match_somewhere() {
        let s = run("cargo test", "api");
        assert!(Query::parse("cargo api").hits(&s));
        assert!(Query::parse("TEST").hits(&s));
        assert!(!Query::parse("cargo web").hits(&s));
        assert!(Query::parse("").hits(&s));
    }

    #[test]
    fn tokens_pick_by_what_a_thing_is() {
        let s = run("cargo test", "api");
        assert!(Query::parse("is:failed").hits(&s));
        assert!(Query::parse("is:fail kind:job").hits(&s), "values match by prefix");
        assert!(!Query::parse("is:going").hits(&s));
        assert!(!Query::parse("kind:agent").hits(&s));
        assert!(Query::parse("in:api by:night").hits(&s));
        assert!(Query::parse("in:home/f").hits(&s), "the folder counts");
        assert!(!Query::parse("in:cargo").hits(&s), "in: is the project, not the title");
        assert!(!Query::parse("by:deploy").hits(&s));
    }

    #[test]
    fn a_token_still_being_typed_asks_nothing() {
        let q = Query::parse("is: api");
        assert_eq!(q.words, ["api"]);
        assert!(!q.picks_rows());
        assert!(Query::parse("is:").is_empty());
        assert!(
            Query::parse("where:x").words == ["where:x"],
            "an unknown name is a word"
        );
    }

    #[test]
    fn a_project_alone_answers_to_words_and_in_only() {
        assert!(Query::parse("api").hits_project(&["api", "/w/api"]));
        assert!(Query::parse("in:api").hits_project(&["api", "/w/api"]));
        assert!(!Query::parse("api is:failed").hits_project(&["api", "/w/api"]));
        assert!(!Query::parse("web").hits_project(&["api", "/w/api"]));
    }

    #[test]
    fn tab_offers_names_then_values() {
        let projects = vec!["api".to_string(), "web".to_string()];
        let rules = vec!["nightly".to_string()];
        assert_eq!(complete("i", &projects, &rules), ["in:", "is:"]);
        assert_eq!(complete("is:f", &projects, &rules), ["is:failed"]);
        assert_eq!(complete("in:", &projects, &rules), ["in:api", "in:web"]);
        assert_eq!(complete("by:n", &projects, &rules), ["by:nightly"]);
        assert!(complete("where:", &projects, &rules).is_empty());
    }

    #[test]
    fn marks_find_every_word_and_merge() {
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(marks("Cargo test cargo", &w(&["cargo"])), [(0, 5), (11, 16)]);
        assert_eq!(marks("testing", &w(&["test", "sting"])), [(0, 7)]);
        assert!(marks("nothing", &w(&[])).is_empty());
    }
}
