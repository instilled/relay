//! Placeholders: `{probe.out}` in an agent's prompt or an `ask:`, filled in
//! when the step is reached.

use serde::{Deserialize, Serialize};

/// A `{head.rest}` reference: `{probe.out}` is ("probe", "out"), `{event.data.x}` is ("event", "data.x").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub head: String,
    pub rest: String,
}

/// What a placeholder stands for, looked up when it is spliced.
pub trait Lookup {
    fn get(&self, r: &Ref) -> Option<String>;
}

/// The references in `text`: `{` + head + `.` + rest + `}`, head `[a-z0-9-]+`, rest `[a-z0-9_.-]+`.
/// Anything else in braces (`{print $1}`, `{}`) is not one.
pub fn refs(text: &str) -> Vec<Ref> {
    found(text).map(|(_, _, r)| r).collect()
}

/// `text` with each reference replaced by what `lookup` says, in one pass — what is spliced in
/// is never read for references again. One `lookup` knows nothing of is left as written.
pub fn fill(text: &str, lookup: &dyn Lookup) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, r) in found(text) {
        out.push_str(&text[at..start]);
        match lookup.get(&r) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&text[start..end]),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// `s` as a whole reference's inside, `head.rest`; `None` if it is not one.
pub(crate) fn parse(s: &str) -> Option<Ref> {
    let (head, rest) = s.split_once('.')?;
    let word = |s: &str, extra: &[char]| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
    };
    (word(head, &['-']) && word(rest, &['_', '.', '-'])).then(|| Ref {
        head: head.to_string(),
        rest: rest.to_string(),
    })
}

/// Each reference with the byte range of its braces, left to right, none overlapping.
fn found(text: &str) -> impl Iterator<Item = (usize, usize, Ref)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        while let Some(i) = text[at..].find('{') {
            let start = at + i;
            let inside = &text[start + 1..];
            if let Some(close) = inside.find('}')
                && let Some(r) = parse(&inside[..close])
            {
                at = start + close + 2;
                return Some((start, at, r));
            }
            at = start + 1;
        }
        None
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub struct Map(BTreeMap<(String, String), String>);
    impl Lookup for Map {
        fn get(&self, r: &Ref) -> Option<String> {
            self.0.get(&(r.head.clone(), r.rest.clone())).cloned()
        }
    }
    pub(crate) fn map(v: &[(&str, &str, &str)]) -> Map {
        Map(v
            .iter()
            .map(|(h, r, x)| ((h.to_string(), r.to_string()), x.to_string()))
            .collect())
    }

    #[test]
    fn references_are_found_and_shell_braces_are_not() {
        let r = refs("a {probe.out} b {event.data.sev} awk '{print $1}' {} {x}");
        assert_eq!(
            r,
            [
                Ref {
                    head: "probe".into(),
                    rest: "out".into()
                },
                Ref {
                    head: "event".into(),
                    rest: "data.sev".into()
                }
            ]
        );
    }

    #[test]
    fn spliced_text_is_not_expanded_again() {
        let m = map(&[("probe", "out", "error near {triage.out}"), ("triage", "out", "NO")]);
        assert_eq!(fill("see {probe.out}", &m), "see error near {triage.out}");
    }

    #[test]
    fn an_unknown_reference_is_left_as_written() {
        assert_eq!(fill("{nope.out}", &map(&[])), "{nope.out}");
    }

    #[test]
    fn a_reference_in_odd_company_is_still_found() {
        let m = map(&[("a", "out", "é")]);
        assert_eq!(fill("{{a.out}} {a.out}é{ {a.out", &m), "{é} éé{ {a.out");
    }
}
