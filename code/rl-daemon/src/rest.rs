//! Telling a session's rest as asks or finished from what the agent last said.
//!
//! Pure: a message in, a kind and its hints out.

use rl_proto::queue::{Hint, RestKind};

/// Phrases that make a last paragraph a request, wherever they fall in it, as
/// whole words.
const ASKING: [&str; 6] = [
    "should i",
    "want me to",
    "shall i",
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
    let closing = last.trim_end_matches(|c: char| c.is_whitespace() || ")\"'*`".contains(c));
    if closing.ends_with('?') || ASKING.iter().any(|p| has_phrase(&lower, p)) || opens_a_sentence(&lower, "which") {
        (RestKind::Asks, vec![Hint::Question])
    } else {
        (RestKind::Finished, vec![])
    }
}

/// Whether `phrase` is in `text` and not run on into a longer word.
fn has_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase)
        .any(|(at, _)| word_ends(text, at + phrase.len()))
}

/// Whether a sentence of `text` starts with the word `word`: at the start, or
/// after `. `, `! `, `? ` or a line break.
fn opens_a_sentence(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = &text[..at];
        let starts =
            before.is_empty() || before.ends_with('\n') || [". ", "! ", "? "].iter().any(|end| before.ends_with(end));
        starts && word_ends(text, at + word.len())
    })
}

/// Whether a word ends at byte `end` of `text`: what follows is not a letter
/// or digit.
fn word_ends(text: &str, end: usize) -> bool {
    text[end..].chars().next().is_none_or(|c| !c.is_alphanumeric())
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
    fn which_opening_the_paragraph_asks() {
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
    fn a_phrase_inside_a_longer_word_is_finished() {
        assert_eq!(
            kind("Caching the index should improve cold starts."),
            (RestKind::Finished, vec![])
        );
    }

    #[test]
    fn which_mid_sentence_is_finished() {
        assert_eq!(
            kind("Kept the parser which handles both formats."),
            (RestKind::Finished, vec![])
        );
    }

    #[test]
    fn which_after_a_full_stop_asks() {
        assert_eq!(kind("Both build. Which do you prefer."), (ASKS.0, ASKS.1()));
    }

    #[test]
    fn a_question_in_bold_or_brackets_asks() {
        assert_eq!(kind("Gates green.\n\n**Ready to merge?**"), (ASKS.0, ASKS.1()));
        assert_eq!(kind("Pushed the fix (anything else?)"), (ASKS.0, ASKS.1()));
        assert_eq!(kind("Shipped. `Tag it?` "), (ASKS.0, ASKS.1()));
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
