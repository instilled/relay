//! What a search says: plain words, and a few `name:value` tokens that pick
//! by what a thing is rather than what it is called. The list and the graph
//! both narrow to what it hits.
//!
//! - words match anywhere — title, project, CLI, summary — and every word
//!   must match somewhere;
//! - `in:` a project, by name or folder;
//! - `is:` a state — `failed`, `going`, `done`, `stopped`, `needs`, `pinned`, `new`;
//! - `kind:` what it is — `agent`, `shell`, `command`, `flow`, `question`;
//!   in the events view, the event's kind (`run.failed`, `run.` for the family);
//! - `by:` the flow behind it (in the events view, or set off by it);
//! - `on:` a git branch — the one a run started on, the one a folder is on.
//!
//! Values match by prefix (`is:fail`, `kind:run.f`), so the list narrows as
//! you type; names (`in:`, `by:`, `on:`) anywhere in them.
//!
//! As you type, the cursor goes to what the words name best (`ranked`), and
//! `tab` walks on from there (`next_match`); the list keeps its order.
//!
//! Pure: parsing, matching, ranking and completing. Nothing about the panel.

/// The token names, in the order completion offers them.
pub const TOKENS: [&str; 5] = ["in:", "is:", "kind:", "by:", "on:"];

/// What `is:` takes.
pub const STATES: [&str; 7] = ["failed", "going", "done", "stopped", "needs", "pinned", "new"];

/// What `kind:` takes.
pub const KINDS: [&str; 5] = ["agent", "shell", "command", "flow", "question"];

/// A search, read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub words: Vec<String>,
    within: Vec<String>,
    is: Vec<String>,
    kind: Vec<String>,
    by: Vec<String>,
    on: Vec<String>,
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
    /// What it is, from `KINDS` (or a graph node's own kinds, or an
    /// event's).
    pub kind: Vec<&'a str>,
    /// The flows behind it.
    pub by: Vec<&'a str>,
    /// The git branch it is on.
    pub on: Option<&'a str>,
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
                Some(("on", v)) => Some((&mut q.on, v)),
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
        self.words.is_empty() && self.within.is_empty() && self.on.is_empty() && !self.picks_rows()
    }

    /// Whether it asks about what things are — state, kind, flow — which a
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
            && self.by.iter().all(|v| s.by.iter().any(|b| has(b, v)))
            && self.on.iter().all(|v| s.on.is_some_and(|b| has(b, v)))
    }

    /// A project header with nothing under it that hit: shown when the search
    /// names it — or the branch it is on — and asks nothing about rows.
    pub fn hits_project(&self, project: &[&str], on: Option<&str>) -> bool {
        !self.picks_rows()
            && self.hits(&Subject {
                project: project.to_vec(),
                on,
                ..Subject::default()
            })
    }
}

/// What `tab` offers for the word at the end of a search: the token names, or
/// a token's values — `in:` the projects, `by:` the flows, `on:` the branches,
/// `kind:` the `kinds` given (the events view's), else `KINDS`.
pub fn complete(
    word: &str,
    projects: &[String],
    flows: &[String],
    branches: &[String],
    kinds: &[String],
) -> Vec<String> {
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
        "kind" if kinds.is_empty() => KINDS.iter().map(|s| s.to_string()).collect(),
        "kind" => kinds.to_vec(),
        "in" => projects.to_vec(),
        "by" => flows.to_vec(),
        "on" => branches.to_vec(),
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

/// What a row is called, field by field, for `ranked`. A project header
/// gives its name as `head`; anything else its title, its project's name and
/// its summary. `rest` is whatever else a search hits — a CLI, a folder.
#[derive(Debug, Default, Clone)]
pub struct Named {
    pub head: String,
    pub title: String,
    pub project: String,
    pub summary: String,
    pub rest: Vec<String>,
}

/// How close `text` comes to the words: 0 it is them, 1 it starts with them,
/// 2 they are a whole word in it, 3 they are in it somewhere. `None` when
/// they are not all in it.
fn closeness(text: &str, words: &[String]) -> Option<u8> {
    let text = text.to_lowercase();
    let phrase = words.join(" ");
    let bare = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
    if text == phrase {
        Some(0)
    } else if text.starts_with(&phrase) {
        Some(1)
    } else if text
        .match_indices(&phrase)
        .any(|(i, m)| bare(text[..i].chars().next_back()) && bare(text[i + m.len()..].chars().next()))
    {
        Some(2)
    } else if words.iter().all(|w| text.contains(w.as_str())) {
        Some(3)
    } else {
        None
    }
}

/// The rows the words name, best first, as indices into `rows`. A project
/// whose name they say leads; then by where they are found — title, then
/// project, then summary, then the rest — and how closely. Words found only
/// across fields come last. Ties keep list order.
pub fn ranked(words: &[String], rows: &[Named]) -> Vec<usize> {
    if words.is_empty() {
        return vec![];
    }
    let mut scored: Vec<((u8, u8), usize)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let fields: Vec<(u8, &str)> = [&n.head, &n.title, &n.project, &n.summary]
                .into_iter()
                .enumerate()
                .map(|(f, t)| (f as u8, t.as_str()))
                .chain(n.rest.iter().map(|t| (4, t.as_str())))
                .collect();
            let best = fields
                .iter()
                .filter_map(|&(f, t)| closeness(t, words).map(|c| (f, c)))
                .min();
            let spread = || {
                words
                    .iter()
                    .all(|w| fields.iter().any(|(_, t)| t.to_lowercase().contains(w.as_str())))
            };
            best.or_else(|| spread().then_some((5, 0))).map(|s| (s, i))
        })
        .collect();
    scored.sort();
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Where `tab` goes from `at` in `order`: the next one, round to the first
/// after the last; `back`, the one before. From off the order, the first — or
/// the last, going back.
pub fn next_match<'a, T: PartialEq>(order: &'a [T], at: Option<&T>, back: bool) -> Option<&'a T> {
    let n = order.len();
    if n == 0 {
        return None;
    }
    let i = match (at.and_then(|a| order.iter().position(|x| x == a)), back) {
        (Some(i), false) => (i + 1) % n,
        (Some(i), true) => (i + n - 1) % n,
        (None, false) => 0,
        (None, true) => n - 1,
    };
    order.get(i)
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
            kind: vec!["command", "flow"],
            by: vec!["nightly"],
            on: Some("feat/search"),
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
        assert!(Query::parse("is:fail kind:fl").hits(&s), "values match by prefix");
        assert!(!Query::parse("is:going").hits(&s));
        assert!(!Query::parse("kind:agent").hits(&s));
        assert!(Query::parse("in:api by:night").hits(&s));
        assert!(Query::parse("in:home/f").hits(&s), "the folder counts");
        assert!(!Query::parse("in:cargo").hits(&s), "in: is the project, not the title");
        assert!(!Query::parse("by:deploy").hits(&s));
        assert!(Query::parse("on:search").hits(&s), "a branch by any part of its name");
        assert!(!Query::parse("on:main").hits(&s));
        assert!(!Query::parse("on:main").hits(&Subject::default()), "no branch, no hit");
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
        assert!(Query::parse("api").hits_project(&["api", "/w/api"], None));
        assert!(Query::parse("in:api").hits_project(&["api", "/w/api"], None));
        assert!(!Query::parse("api is:failed").hits_project(&["api", "/w/api"], None));
        assert!(!Query::parse("web").hits_project(&["api", "/w/api"], None));
        assert!(Query::parse("on:ma").hits_project(&["api", "/w/api"], Some("main")));
        assert!(!Query::parse("on:main").hits_project(&["api", "/w/api"], None));
    }

    #[test]
    fn tab_offers_names_then_values() {
        let projects = vec!["api".to_string(), "web".to_string()];
        let flows = vec!["nightly".to_string()];
        let branches = vec!["main".to_string(), "feat".to_string(), "main".to_string()];
        let c = |w| complete(w, &projects, &flows, &branches, &[]);
        assert_eq!(c("i"), ["in:", "is:"]);
        assert_eq!(c("is:f"), ["is:failed"]);
        assert_eq!(c("in:"), ["in:api", "in:web"]);
        assert_eq!(c("by:n"), ["by:nightly"]);
        assert_eq!(c("on:"), ["on:feat", "on:main"]);
        assert!(c("where:").is_empty());
        assert_eq!(c("kind:").len(), KINDS.len(), "the list's kinds");
        let kinds = vec![
            "run.failed".to_string(),
            "run.finished".to_string(),
            "log.issue".to_string(),
        ];
        assert_eq!(
            complete("kind:run.f", &projects, &flows, &branches, &kinds),
            ["kind:run.failed", "kind:run.finished"],
            "the events view's kinds, when it gives them"
        );
    }

    #[test]
    fn marks_find_every_word_and_merge() {
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(marks("Cargo test cargo", &w(&["cargo"])), [(0, 5), (11, 16)]);
        assert_eq!(marks("testing", &w(&["test", "sting"])), [(0, 7)]);
        assert!(marks("nothing", &w(&[])).is_empty());
    }

    fn row(title: &str, project: &str, summary: &str) -> Named {
        Named {
            title: title.into(),
            project: project.into(),
            summary: summary.into(),
            ..Named::default()
        }
    }

    fn head(name: &str) -> Named {
        Named {
            head: name.into(),
            rest: vec![format!("~/w/{name}")],
            ..Named::default()
        }
    }

    fn rank(search: &str, rows: &[Named]) -> Vec<usize> {
        ranked(&Query::parse(search).words, rows)
    }

    #[test]
    fn a_project_by_its_name_beats_a_row_that_mentions_it() {
        let rows = [
            row("which way?", "api", "asks about relay"),
            row("relay", "api", ""),
            head("api"),
            row("sleep", "relay", ""),
            head("relay"),
        ];
        assert_eq!(rank("relay", &rows), [4, 1, 3, 0]);
        assert_eq!(rank("rel", &rows)[0], 4, "a prefix of the name will do");
    }

    #[test]
    fn exact_then_prefix_then_whole_word_then_anywhere() {
        let rows = [
            row("retests", "", ""),
            row("run test now", "", ""),
            row("testing", "", ""),
            row("test", "", ""),
        ];
        assert_eq!(rank("test", &rows), [3, 2, 1, 0]);
        assert_eq!(rank("TEST", &rows), [3, 2, 1, 0], "whatever the case");
        let rows = [row("make the test run", "", ""), row("test run", "", "")];
        assert_eq!(rank("test run", &rows), [1, 0], "several words as one");
    }

    #[test]
    fn title_then_project_then_summary_then_the_rest() {
        let rows = [
            Named {
                rest: vec!["deploy".into()],
                ..Named::default()
            },
            row("x", "", "deploy"),
            row("x", "deploy", ""),
            row("a deploying step", "", ""),
            row("x", "api", "nothing"),
        ];
        assert_eq!(
            rank("deploy", &rows),
            [3, 2, 1, 0],
            "anywhere in a title beats an exact project"
        );
        let spread = [row("cargo", "api", ""), row("cargo api", "", "")];
        assert_eq!(
            rank("cargo api", &spread),
            [1, 0],
            "words in one field beat words across them"
        );
    }

    #[test]
    fn ties_keep_list_order_and_no_words_rank_nothing() {
        let rows = [row("b", "api", ""), row("a", "api", ""), row("c", "api", "")];
        assert_eq!(rank("api", &rows), [0, 1, 2]);
        assert!(rank("", &rows).is_empty());
        assert!(
            rank("is:failed", &rows).is_empty(),
            "tokens alone are not words to match"
        );
    }

    #[test]
    fn tab_walks_the_matches_round_and_back() {
        let order = ["a", "b", "c"].map(String::from);
        let next = |at: Option<&str>, back| {
            next_match(&order, at.map(String::from).as_ref(), back)
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(next(Some("a"), false), "b");
        assert_eq!(next(Some("c"), false), "a", "round to the first");
        assert_eq!(next(Some("a"), true), "c", "and back round to the last");
        assert_eq!(next(Some("b"), true), "a");
        assert_eq!(next(Some("zz"), false), "a", "from off the matches, the first");
        assert_eq!(next(None, true), "c");
        assert_eq!(next_match::<String>(&[], None, false), None);
    }
}
