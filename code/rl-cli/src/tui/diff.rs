//! What a change to a flow's `flow.md` does to it, line by line: the lines both keep, the ones
//! that go, and the ones that come. A longest common run of lines, found the plain way — a flow
//! is short, so the table of every pair of lines costs nothing worth saving.

/// `s`'s lines, split at `\n` alone: a `\r` before one is the text's, kept to be shown.
fn split(s: &str) -> Vec<&str> {
    match s.strip_suffix('\n').unwrap_or(s) {
        "" if !s.starts_with('\n') => Vec::new(),
        s => s.split('\n').collect(),
    }
}

/// What became of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tag {
    /// In both.
    Same,
    /// In the old text only.
    Gone,
    /// In the new text only.
    Added,
}

/// `old` to `new`, a line at a time, in order. Where a run of lines is replaced, the ones that go
/// come before the ones that come in their place. A last `\n` makes no empty line of its own.
pub(super) fn lines<'a>(old: &'a str, new: &'a str) -> Vec<(Tag, &'a str)> {
    let a = split(old);
    let b = split(new);
    // `kept[i][j]`: how many lines `a[i..]` and `b[j..]` can keep in common.
    let mut kept = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            kept[i][j] = if a[i] == b[j] {
                kept[i + 1][j + 1] + 1
            } else {
                kept[i + 1][j].max(kept[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((Tag::Same, a[i]));
            i += 1;
            j += 1;
        } else if kept[i + 1][j] >= kept[i][j + 1] {
            out.push((Tag::Gone, a[i]));
            i += 1;
        } else {
            out.push((Tag::Added, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| (Tag::Gone, *l)));
    out.extend(b[j..].iter().map(|l| (Tag::Added, *l)));
    out
}

#[cfg(test)]
mod tests {
    use super::Tag::{Added, Gone, Same};
    use super::*;

    #[test]
    fn same_text_is_all_same() {
        let text = "---\nwhen: every 1d\n---\n";
        assert_eq!(
            lines(text, text),
            vec![(Same, "---"), (Same, "when: every 1d"), (Same, "---")]
        );
    }

    #[test]
    fn a_changed_line_is_gone_then_added() {
        assert_eq!(
            lines("a\nb\nc\n", "a\nB\nc\n"),
            vec![(Same, "a"), (Gone, "b"), (Added, "B"), (Same, "c")]
        );
    }

    #[test]
    fn added_at_the_end() {
        assert_eq!(
            lines("a\nb\n", "a\nb\nc\nd\n"),
            vec![(Same, "a"), (Same, "b"), (Added, "c"), (Added, "d")]
        );
    }

    #[test]
    fn everything_new_from_empty() {
        assert_eq!(lines("", "a\nb\n"), vec![(Added, "a"), (Added, "b")]);
    }

    #[test]
    fn a_last_newline_makes_no_empty_line() {
        assert_eq!(lines("a\n", "a"), vec![(Same, "a")]);
        assert_eq!(lines("a\n", "a\nb\n"), vec![(Same, "a"), (Added, "b")]);
    }

    #[test]
    fn a_run_of_lines_replaced_goes_whole_then_comes_whole() {
        assert_eq!(
            lines("a\nb\nc\nd\n", "a\nX\nY\nZ\nd\n"),
            vec![
                (Same, "a"),
                (Gone, "b"),
                (Gone, "c"),
                (Added, "X"),
                (Added, "Y"),
                (Added, "Z"),
                (Same, "d")
            ]
        );
    }

    #[test]
    fn blank_lines_are_lines() {
        // One kept, one gone, one come: each blank line counted where it stands.
        assert_eq!(
            lines("a\n\nb\n\nc\n", "a\n\nb\nc\n\n"),
            vec![
                (Same, "a"),
                (Same, ""),
                (Same, "b"),
                (Gone, ""),
                (Same, "c"),
                (Added, "")
            ]
        );
        assert_eq!(lines("a\n", "\na\n"), vec![(Added, ""), (Same, "a")]);
    }

    #[test]
    fn a_carriage_return_is_the_lines_own() {
        assert_eq!(lines("a\n", "a\r\n"), vec![(Gone, "a"), (Added, "a\r")]);
        assert_eq!(lines("\n", "\n"), vec![(Same, "")]);
    }
}
