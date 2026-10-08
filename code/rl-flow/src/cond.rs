//! A step's `if:`: one test of what an earlier step left.

use crate::template::{self, Lookup, Ref};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cond {
    Set(Ref),
    Unset(Ref),
    Is(Ref, String),
    IsNot(Ref, String),
}

const ALLOWED: &str = "an if: is `step.key`, `!step.key`, `step.key == value` or `step.key != value`";

/// `probe.out`, `!probe.out`, `review.choice == apply`, `probe.severity != low` (a value may be
/// quoted). Anything else is refused, saying what is allowed.
pub fn parse(s: &str) -> Result<Cond, String> {
    let s = s.trim();
    if s.contains("&&") || s.contains("||") {
        return Err(format!("an if: holds one condition, not several; {ALLOWED}"));
    }
    let bad = || format!("{ALLOWED}; not `{s}`");
    if let Some(rest) = s.strip_prefix('!') {
        return template::parse(rest.trim()).map(Cond::Unset).ok_or_else(bad);
    }
    let op = ["==", "!="].iter().filter_map(|op| s.find(op).map(|i| (i, *op))).min();
    let Some((i, op)) = op else {
        return template::parse(s).map(Cond::Set).ok_or_else(bad);
    };
    let subject = template::parse(s[..i].trim()).ok_or_else(bad)?;
    let value = value(s[i + 2..].trim()).ok_or_else(bad)?;
    Ok(if op == "==" {
        Cond::Is(subject, value)
    } else {
        Cond::IsNot(subject, value)
    })
}

/// A value to compare with: a bare word, or text in matching quotes.
fn value(s: &str) -> Option<String> {
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return Some(inner.to_string());
        }
    }
    (!s.is_empty() && !s.contains(char::is_whitespace) && !s.contains(['"', '\''])).then(|| s.to_string())
}

impl Cond {
    /// The step and key it asks about.
    pub fn subject(&self) -> &Ref {
        match self {
            Cond::Set(r) | Cond::Unset(r) | Cond::Is(r, _) | Cond::IsNot(r, _) => r,
        }
    }

    /// Set: there is a value and it is not empty once trimmed. Is: there is a value and, trimmed,
    /// it is the word. The negations hold exactly when those do not, so a value that was never
    /// made (a step that was skipped) is not set and is not anything.
    pub fn holds(&self, lookup: &dyn Lookup) -> bool {
        let value = lookup.get(self.subject());
        let set = value.as_deref().is_some_and(|v| !v.trim().is_empty());
        let is = |want: &str| value.as_deref().is_some_and(|v| v.trim() == want);
        match self {
            Cond::Set(_) => set,
            Cond::Unset(_) => !set,
            Cond::Is(_, want) => is(want),
            Cond::IsNot(_, want) => !is(want),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::tests::map;

    #[test]
    fn conditions_read_and_hold() {
        let m = map(&[
            ("probe", "out", " x "),
            ("review", "choice", "apply"),
            ("p", "empty", "  "),
        ]);
        assert!(parse("probe.out").unwrap().holds(&m));
        assert!(!parse("p.empty").unwrap().holds(&m));
        assert!(parse("!p.empty").unwrap().holds(&m));
        assert!(parse("review.choice == apply").unwrap().holds(&m));
        assert!(parse("review.choice != \"ignore\"").unwrap().holds(&m));
        assert!(!parse("missing.out").unwrap().holds(&m));
        assert!(parse("a.out && b.out").unwrap_err().contains("one"));
        assert!(parse("probe").unwrap_err().contains("step.key"));
    }

    #[test]
    fn a_value_is_compared_trimmed_and_a_missing_one_is_nothing() {
        let m = map(&[("probe", "severity", "low\n")]);
        assert!(parse("probe.severity == low").unwrap().holds(&m));
        assert!(!parse("probe.severity != 'low'").unwrap().holds(&m));
        assert!(!parse("gone.severity == low").unwrap().holds(&m));
        assert!(parse("gone.severity != low").unwrap().holds(&m));
        assert_eq!(
            parse("review.choice == \"a b\"").unwrap(),
            Cond::Is(
                Ref {
                    head: "review".into(),
                    rest: "choice".into()
                },
                "a b".into()
            )
        );
    }

    #[test]
    fn a_malformed_condition_is_refused() {
        for s in [
            "",
            "!",
            "a.out ==",
            "a.out = b",
            "a.out == b c",
            "A.out",
            "!!a.out",
            "a.out == 'b",
        ] {
            assert!(parse(s).is_err(), "{s:?} should be refused");
        }
    }
}
