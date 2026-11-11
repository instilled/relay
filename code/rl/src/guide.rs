//! What a newly set-up Relay can do: the few lines `rl setup`, the first
//! start's setup and the list's `/setup` end with, when they set something
//! up. Never after `nothing to change`, nor for `--check` or `--remove`.

/// Where the messaging guide is.
pub const MESSAGING_GUIDE: &str = "https://github.com/instilled/relay/blob/main/docs/messaging.md";

/// The block itself, a line each.
pub fn next() -> Vec<String> {
    vec![
        "What you can do now: your agents can message each other (relay's who, send and inbox tools).".into(),
        "Ask one in plain words, for example:".into(),
        "  \"ask the codex session in shop-api to review my diff and send me what it finds\"".into(),
        "  \"when the tests pass, tell the claude session 'web' to rebase on main\"".into(),
        format!("More: {MESSAGING_GUIDE}"),
    ]
}

/// Whether what `setup::run` said is that it set something up: anything
/// but its `nothing to change`.
pub fn set_up_something(lines: &[String]) -> bool {
    !lines.is_empty() && !lines.iter().all(|l| l == "nothing to change")
}

/// What `setup::run` said, with the block after it when it set something
/// up (a blank line between).
pub fn with_next(mut lines: Vec<String>) -> Vec<String> {
    if set_up_something(&lines) {
        lines.push(String::new());
        lines.extend(next());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_change_gets_no_block() {
        let said = vec!["nothing to change".to_string()];
        assert_eq!(with_next(said.clone()), said);
        assert_eq!(with_next(Vec::new()), Vec::<String>::new());
    }

    #[test]
    fn a_change_ends_with_the_block_and_the_link() {
        let said = with_next(vec!["claude: hooks installed".to_string()]);
        assert_eq!(said[0], "claude: hooks installed");
        assert_eq!(said[1], "");
        assert!(said[2].starts_with("What you can do now"));
        assert_eq!(said.last().unwrap(), &format!("More: {MESSAGING_GUIDE}"));
        assert!(said.len() <= 2 + 5, "the block stays short: {said:#?}");
    }
}
