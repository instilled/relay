//! A recap split into its three parts: what was worked on, what is still
//! open, what comes next. The labels are `Worked on:`, `Open questions:` and
//! `Next:`.

/// The parts of a recap written under the three labels. A part that is missing
/// or says only `none` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Brief {
    pub worked_on: Option<String>,
    pub questions: Option<String>,
    pub next: Option<String>,
}

impl Brief {
    /// Reads text with the sections `Worked on:`, `Open questions:`, `Next:`
    /// (any case, optionally bulleted, headed or bold).
    pub fn parse(text: &str) -> Brief {
        const HEADS: [(&str, usize); 3] = [("worked on", 0), ("open questions", 1), ("next", 2)];
        let mut parts: [Vec<&str>; 3] = Default::default();
        let mut at: Option<usize> = None;
        for line in text.lines() {
            let bare = line.trim().trim_start_matches(['#', '*', '-', ' ']);
            let lower = bare.to_ascii_lowercase();
            let hit = HEADS.iter().find_map(|(label, i)| {
                lower.strip_prefix(label)?;
                let after = bare[label.len()..].trim_start_matches(['*', ' ']);
                Some((*i, after.strip_prefix(':')?.trim_start_matches(['*', ' '])))
            });
            match (hit, at) {
                (Some((i, rest)), _) => {
                    at = Some(i);
                    parts[i].push(rest);
                }
                (None, Some(i)) => parts[i].push(line),
                (None, None) => {}
            }
        }
        let [worked_on, questions, next] = parts.map(|lines| {
            let text = lines.join("\n").trim().to_string();
            (!text.is_empty() && !text.eq_ignore_ascii_case("none")).then_some(text)
        });
        Brief {
            worked_on,
            questions,
            next,
        }
    }

    /// Whether no part has anything in it.
    pub fn is_empty(&self) -> bool {
        self.worked_on.is_none() && self.questions.is_none() && self.next.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_sections() {
        let a = "Worked on:\nFixed the search index.\nAdded a test.\n\nOpen questions:\n- Keep the old flag?\n\nNext:\nShip it.";
        let b = Brief::parse(a);
        assert_eq!(b.worked_on.as_deref(), Some("Fixed the search index.\nAdded a test."));
        assert_eq!(b.questions.as_deref(), Some("- Keep the old flag?"));
        assert_eq!(b.next.as_deref(), Some("Ship it."));
        assert!(!b.is_empty());
    }

    #[test]
    fn a_missing_or_none_section_is_none_and_labels_may_be_dressed() {
        let b = Brief::parse("**Worked on:** the parser\n## Next:\nnone");
        assert_eq!(b.worked_on.as_deref(), Some("the parser"));
        assert_eq!(b.questions, None);
        assert_eq!(b.next, None);
    }

    #[test]
    fn no_labels_is_empty() {
        assert!(Brief::parse("no labels here").is_empty());
        assert_eq!(Brief::parse("no labels here"), Brief::default());
    }
}
