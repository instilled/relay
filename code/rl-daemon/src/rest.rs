//! Telling a session's rest as asks or finished from what the agent last said.
//!
//! Pure: a message in, a kind and its hints out.

use rl_proto::queue::{Hint, RestKind};

/// Phrases that make a last paragraph a request, wherever they fall in it.
const ASKING: [&str; 7] = [
    "should i",
    "want me to",
    "shall i",
    "which ",
    "do you want",
    "let me know if",
    "let me know whether",
];

/// Whether the agent is waiting on you or is done, by its last message. No
/// message, or one that is only code, cannot be told: it asks, unsure.
pub fn classify(message: Option<&str>) -> (RestKind, Vec<Hint>) {
    let Some(last) = message.and_then(last_paragraph) else {
        return (RestKind::Asks, vec![Hint::Unsure]);
    };
    let lower = last.to_lowercase();
    if last.ends_with('?') || ASKING.iter().any(|p| lower.contains(p)) {
        (RestKind::Asks, vec![Hint::Question])
    } else {
        (RestKind::Finished, vec![])
    }
}

/// The text after the last blank line, code fences and what is inside them
/// removed; `None` when nothing else is left.
fn last_paragraph(message: &str) -> Option<String> {
    let mut prose = String::new();
    let mut fenced = false;
    for line in message.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    prose
        .trim()
        .rsplit("\n\n")
        .next()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(m: &str) -> (RestKind, Vec<Hint>) {
        classify(Some(m))
    }

    const ASKS: (RestKind, fn() -> Vec<Hint>) = (RestKind::Asks, || vec![Hint::Question]);

    #[test]
    fn ends_on_a_question_asks() {
        assert_eq!(
            kind("Removed 23 worktrees.\n\nShould I prune the branches too?"),
            (ASKS.0, ASKS.1())
        );
    }

    #[test]
    fn want_me_to_asks() {
        assert_eq!(kind("Done with the parser. Want me to add tests."), (ASKS.0, ASKS.1()));
    }

    #[test]
    fn let_me_know_whether_asks() {
        assert_eq!(
            kind("Two options exist.\n\nLet me know whether to keep the old one."),
            (ASKS.0, ASKS.1())
        );
    }

    #[test]
    fn which_asks() {
        assert_eq!(kind("Which of the two do you prefer."), (ASKS.0, ASKS.1()));
    }

    #[test]
    fn a_plain_wrap_up_is_finished() {
        assert_eq!(kind("All 8 findings fixed; gates green."), (RestKind::Finished, vec![]));
    }

    #[test]
    fn a_question_in_an_earlier_paragraph_is_finished() {
        assert_eq!(
            kind("Why did it fail? A stale lock.\n\nFixed and pushed."),
            (RestKind::Finished, vec![])
        );
    }

    #[test]
    fn a_question_inside_code_does_not_count() {
        assert_eq!(
            kind("Fixed and pushed.\n\n```\nwhich cargo?\n```"),
            (RestKind::Finished, vec![])
        );
    }

    #[test]
    fn no_message_is_unsure_asks() {
        let unsure = (RestKind::Asks, vec![Hint::Unsure]);
        assert_eq!(classify(None), unsure);
        assert_eq!(kind(""), unsure);
        assert_eq!(kind("  \n\n "), unsure);
    }

    #[test]
    fn only_code_is_unsure_asks() {
        assert_eq!(kind("```rust\nfn main() {}\n```"), (RestKind::Asks, vec![Hint::Unsure]));
    }
}
