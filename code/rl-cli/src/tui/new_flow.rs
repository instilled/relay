//! A new flow in the panel: what `/flow new …` asks for, and a flow an assistant drafted, shown
//! before anything is written — its `flow.md`, what would stop it loading, and who drafted it.
//! Pure but for drawing it: the words read, a draft checked and said, and how much of it has been
//! on screen; the panel writes it on `enter`, once its last line has been.

use super::theme;
use crate::flow::{NewFile, templates};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::cell::Cell;
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

pub const USAGE: &str = "/flow new <name> [words…] · /flow new <name> --from <template> [key=value…]";

/// What `/flow new` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// The bare template, in `$EDITOR`.
    Bare { name: String },
    /// A template, its blanks given as `key=value` words.
    From {
        name: String,
        template: String,
        given: Vec<String>,
    },
    /// A draft, from the words.
    Words { name: String, words: String },
}

impl Asked {
    pub fn name(&self) -> &str {
        match self {
            Asked::Bare { name } | Asked::From { name, .. } | Asked::Words { name, .. } => name,
        }
    }
}

/// The words after `/flow new`, read.
pub fn asked(words: &[&str]) -> Result<Asked, String> {
    let Some((name, rest)) = words.split_first() else {
        return Err(USAGE.to_string());
    };
    let name = name.to_string();
    Ok(match rest {
        [] => Asked::Bare { name },
        ["--from"] => return Err(format!("/flow new {name} --from which? {}", templates::names())),
        ["--from", template, given @ ..] => Asked::From {
            name,
            template: template.to_string(),
            given: given.iter().map(|g| g.to_string()).collect(),
        },
        words => Asked::Words {
            name,
            words: words.join(" "),
        },
    })
}

/// A template's files as the panel fills them: its blanks from `key=value` words, `in` the folder
/// `here`; what is missing is said as it would be typed.
pub fn template_files(template: &str, given: &[String], here: &str) -> Result<Vec<NewFile>, String> {
    let t = templates::find(template)?;
    let given = templates::given_in(t, given, here)?;
    let missing = templates::missing(t, &given);
    if !missing.is_empty() {
        return Err(templates::needs(t, &missing, ""));
    }
    templates::fill(t, &given)
}

/// A flow an assistant drafted, not written yet.
pub struct Drafted {
    /// Empty when the draft named none and the person did not either.
    pub name: String,
    pub text: String,
    pub agent: String,
    /// Why it would not load, each at its line; none, and enter may write it.
    pub problems: Vec<String>,
    /// The first screen row shown.
    top: Cell<usize>,
    /// Its rows and the pane's, as last drawn: how far it scrolls, and a page.
    rows: Cell<usize>,
    page: Cell<usize>,
    /// Its last line has been on screen: the yes is to what was shown.
    seen_end: Cell<bool>,
}

impl Drafted {
    /// `text` as `name`'s `flow.md`, checked as a reload checks a flow: `known` is the projects
    /// Relay knows; `taken`, why a name may not be a new flow, if it may not.
    pub fn new(
        name: Option<String>,
        text: String,
        agent: String,
        known: &[PathBuf],
        taken: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let name = name.unwrap_or_default();
        let mut problems = Vec::new();
        if name.is_empty() {
            problems.push("it names no flow · /flow new <name> <words> names one".to_string());
        } else if let Some(why) = taken(&name) {
            problems.push(why);
        }
        problems.extend(crate::flow::problems(&name, &text, known));
        Drafted {
            name,
            text,
            agent,
            problems,
            top: Cell::new(0),
            rows: Cell::new(0),
            page: Cell::new(0),
            seen_end: Cell::new(false),
        }
    }

    /// Whether enter may write it.
    pub fn ready(&self) -> bool {
        self.problems.is_empty()
    }

    /// Whether its last line has been on screen.
    pub fn seen(&self) -> bool {
        self.seen_end.get()
    }

    /// `↑↓` a row, `pgup/pgdn` a page, as far as its last row at the foot of the pane. Whether
    /// the key was one of those.
    pub fn scroll(&self, key: KeyCode) -> bool {
        let page = self.page.get().max(1);
        let top = self.top.get();
        let top = match key {
            KeyCode::Up => top.saturating_sub(1),
            KeyCode::Down => top + 1,
            KeyCode::PageUp => top.saturating_sub(page),
            KeyCode::PageDown => top + page,
            _ => return false,
        };
        self.top.set(top.min(self.rows.get().saturating_sub(page)));
        true
    }

    /// The pane, from the row scrolled to: each line broken at the pane's width, so what is
    /// drawn is counted, and the end said seen once its last row has been drawn. One that fits
    /// is seen at once.
    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let rows = wrap(self.lines(), area.width as usize);
        let page = area.height as usize;
        let top = self.top.get().min(rows.len().saturating_sub(page));
        self.top.set(top);
        self.rows.set(rows.len());
        self.page.set(page);
        if area.width > 0 && page > 0 && top + page >= rows.len() {
            self.seen_end.set(true);
        }
        let shown: Vec<Line> = rows.into_iter().skip(top).take(page).collect();
        f.render_widget(Paragraph::new(shown), area);
    }

    /// The pane: whose it is and where it would go, what would stop it loading, then `flow.md` —
    /// every line of it, a character that does not print shown as its escape.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let name = if self.name.is_empty() {
            "(unnamed)".to_string()
        } else {
            shown(&self.name)
        };
        let mut v = vec![Line::from(vec![
            Span::styled(format!(" {name}"), theme::name()),
            Span::styled(
                format!("  drafted by {} · flows/{name}/flow.md", shown(&self.agent)),
                theme::dim(),
            ),
        ])];
        for p in &self.problems {
            v.push(Line::from(Span::styled(format!(" ! {}", shown(p)), theme::bad())));
        }
        v.push(Line::raw(""));
        // Split at `\n` alone: a `\r` before one is the draft's, and shown.
        let text = self.text.strip_suffix('\n').unwrap_or(&self.text);
        v.extend(text.split('\n').map(|l| Line::raw(format!(" {}", shown(l)))));
        v
    }

    /// What the keys do, for the footer: the scroll keys first while it is taller than the pane.
    pub fn hint(&self) -> String {
        let keys = if self.ready() {
            "enter writes it and says yes · e opens it in $EDITOR first · esc lets it go"
        } else {
            "it would not load · e opens it in $EDITOR · esc lets it go"
        };
        if self.rows.get() > self.page.get() {
            format!("↑↓ pgup/pgdn read it · {keys}")
        } else {
            keys.to_string()
        }
    }
}

/// A line as it is written, a character that does not print — a control character but a tab
/// (shown as spaces), or one that is not seen or turns the text around — as its escape (`\r`,
/// `\u{1b}`).
fn shown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() || unseen(c) => out.extend(c.escape_default()),
            c => out.push(c),
        }
    }
    out
}

/// A character that takes no room or turns the text's direction: one a draft could hide behind.
fn unseen(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// Lines broken into the rows a pane `width` wide draws, each piece keeping its style.
fn wrap(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in lines {
        let mut row: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for span in line.spans {
            let mut piece = String::new();
            for c in span.content.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > width && used > 0 {
                    row.push(Span::styled(std::mem::take(&mut piece), span.style));
                    rows.push(Line::from(std::mem::take(&mut row)));
                    used = 0;
                }
                piece.push(c);
                used += w;
            }
            if !piece.is_empty() {
                row.push(Span::styled(piece, span.style));
            }
        }
        rows.push(Line::from(row));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n";

    #[test]
    fn what_flow_new_asks_for() {
        assert_eq!(asked(&[]).unwrap_err(), USAGE);
        assert_eq!(asked(&["pulse"]).unwrap(), Asked::Bare { name: "pulse".into() });
        assert_eq!(
            asked(&["logs", "--from", "prod-logs", "project=p"]).unwrap(),
            Asked::From {
                name: "logs".into(),
                template: "prod-logs".into(),
                given: vec!["project=p".into()],
            }
        );
        assert_eq!(
            asked(&["logs", "--from"]).unwrap_err(),
            "/flow new logs --from which? prod-logs, morning-brief"
        );
        let brief = asked(&["brief", "what", "my", "team", "did"]).unwrap();
        assert_eq!(
            brief,
            Asked::Words {
                name: "brief".into(),
                words: "what my team did".into()
            }
        );
        assert_eq!(brief.name(), "brief");
        assert_eq!(
            template_files("prod-logs", &[], "~/w/api").unwrap_err(),
            "prod-logs needs project=… (the GCP project whose logs are read, like my-prod-project)"
        );
        let files = template_files("prod-logs", &["project=p".into()], "~/w/api").unwrap();
        assert!(String::from_utf8_lossy(&files[0].bytes).contains("in: \"~/w/api\""));
    }

    #[test]
    fn a_draft_says_what_would_stop_it_loading() {
        let ok = Drafted::new(Some("hello".into()), HELLO.into(), "fakeai".into(), &[], &|_| None);
        assert!(ok.ready(), "{:?}", ok.problems);
        let text: Vec<String> = ok.lines().iter().map(|l| l.to_string()).collect();
        assert_eq!(text[0], " hello  drafted by fakeai · flows/hello/flow.md");
        assert!(text.contains(&"     run: echo hello".to_string()), "{text:#?}");
        assert_eq!(
            ok.hint(),
            "enter writes it and says yes · e opens it in $EDITOR first · esc lets it go"
        );

        let bad = Drafted::new(
            Some("x".into()),
            "---\nin: ~/nowhere\nsteps:\n  a:\n    after: nope\n    run: x\n---\n".into(),
            "fakeai".into(),
            &[],
            &|_| None,
        );
        assert_eq!(
            bad.problems,
            [
                "flow.md:2 · in: ~/nowhere is not a project Relay knows — `rl add ~/nowhere`",
                "flow.md:4 · `a` comes after `nope`, which is no step",
            ]
        );
        assert_eq!(bad.hint(), "it would not load · e opens it in $EDITOR · esc lets it go");
        let lines: Vec<String> = bad.lines().iter().map(|l| l.to_string()).collect();
        assert_eq!(
            lines[1],
            " ! flow.md:2 · in: ~/nowhere is not a project Relay knows — `rl add ~/nowhere`"
        );

        let unnamed = Drafted::new(None, HELLO.into(), "fakeai".into(), &[], &|_| None);
        assert_eq!(
            unnamed.problems,
            ["it names no flow · /flow new <name> <words> names one"]
        );
        assert!(unnamed.lines()[0].to_string().starts_with(" (unnamed)"));
        let taken = Drafted::new(Some("hello".into()), HELLO.into(), "fakeai".into(), &[], &|n| {
            Some(format!("`{n}` is a flow already"))
        });
        assert_eq!(taken.problems, ["`hello` is a flow already"]);
        let broken = Drafted::new(
            Some("y".into()),
            "no frontmatter\n".into(),
            "fakeai".into(),
            &[],
            &|_| None,
        );
        assert_eq!(
            broken.problems,
            ["flow.md:1 · a flow file starts with ---, then its frontmatter"]
        );
    }

    /// `d` drawn on a `w`×`h` screen, its rows.
    fn drawn(d: &Drafted, w: u16, h: u16) -> Vec<String> {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        t.draw(|f| d.draw(f, f.area())).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn a_draft_is_seen_once_its_last_line_has_been_drawn() {
        let fits = Drafted::new(Some("hello".into()), HELLO.into(), "fakeai".into(), &[], &|_| None);
        assert!(!fits.seen(), "not drawn yet");
        drawn(&fits, 80, 20);
        assert!(fits.seen(), "it fits: seen at once");
        assert!(!fits.hint().starts_with("↑↓"), "nothing to scroll");

        let mut md = HELLO.to_string();
        for i in 1..=30 {
            md.push_str(&format!("Line {i}.\n"));
        }
        let tall = Drafted::new(Some("tall".into()), md, "fakeai".into(), &[], &|_| None);
        let first = drawn(&tall, 80, 10);
        assert!(first[0].starts_with(" tall  drafted by fakeai"), "{first:#?}");
        assert!(!tall.seen(), "its end is below");
        assert_eq!(
            tall.hint(),
            "↑↓ pgup/pgdn read it · enter writes it and says yes · e opens it in $EDITOR first · esc lets it go"
        );
        assert!(tall.scroll(KeyCode::Down) && tall.scroll(KeyCode::PageDown));
        assert!(!tall.scroll(KeyCode::Enter), "not a scroll key");
        assert_eq!(drawn(&tall, 80, 10)[0].trim_end(), " Line 2.", "a row and a page down");
        assert!(!tall.seen());
        for _ in 0..5 {
            tall.scroll(KeyCode::PageDown);
        }
        let last = drawn(&tall, 80, 10);
        assert!(tall.seen());
        assert_eq!(last[9].trim_end(), " Line 30.", "no further than its last row");
        tall.scroll(KeyCode::PageUp);
        assert!(tall.seen(), "seen stays seen");

        // A line wider than the pane is broken, not cut: its end is drawn, and counted.
        let wide = format!("{HELLO}{}END\n", "x".repeat(200));
        let wide = Drafted::new(Some("wide".into()), wide, "fakeai".into(), &[], &|_| None);
        let rows = drawn(&wide, 40, 30);
        assert!(wide.seen());
        assert!(rows.iter().any(|r| r.contains("END")), "{rows:#?}");
    }

    #[test]
    fn what_does_not_print_is_shown_as_its_escape() {
        let md = "---\nwhen: every 1h\r\nsteps:\n  say:\n    run: \"echo \u{1b}[2J\u{202e}hi\"\n---\n";
        let d = Drafted::new(Some("odd".into()), md.into(), "fakeai".into(), &[], &|_| None);
        let text: Vec<String> = d.lines().iter().map(|l| l.to_string()).collect();
        assert!(text.contains(&" when: every 1h\\r".to_string()), "{text:#?}");
        assert!(
            text.contains(&"     run: \"echo \\u{1b}[2J\\u{202e}hi\"".to_string()),
            "{text:#?}"
        );
        assert_eq!(shown("a\tb"), "a    b");
    }
}
