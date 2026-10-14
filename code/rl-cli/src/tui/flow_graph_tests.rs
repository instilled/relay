//! A flow's steps drawn: by rank, with `if` labels, its last firing's marks and the caret.

use super::*;
use rl_flow::{def, doc};
use rl_proto::flow::{FiringStatus, StepState};

/// A flow read from its frontmatter's `steps:`.
fn flow(steps: &str) -> FlowDef {
    let text = format!("---\nsteps:\n{steps}---\n");
    def::check("t", doc::read(&text).unwrap(), &|_| true).unwrap()
}

/// `fetch`, then `summarise` and `approve` after it, the ask only if `fetch` found something.
fn fetched() -> FlowDef {
    flow(
        "  fetch:\n    run: fetch.sh\n  summarise:\n    after: fetch\n    run: echo s\n  \
         approve:\n    after: fetch\n    if: fetch.found\n    ask: ship it?\n    options: { yes: {} }\n",
    )
}

fn firing(steps: &[(&str, StepStatus)]) -> Firing {
    Firing {
        id: "fi1".into(),
        flow: "t".into(),
        trigger: "when".into(),
        event: None,
        status: FiringStatus::Waiting,
        started_at: "2026-09-10T11:00:00Z".parse().unwrap(),
        ended_at: None,
        outcome: None,
        steps: steps
            .iter()
            .map(|(name, status)| StepState {
                name: name.to_string(),
                status: *status,
                run: None,
                decision: None,
            })
            .collect(),
    }
}

fn plain(def: &FlowDef, firing: Option<&Firing>, at: Option<usize>) -> String {
    draw(def, firing, at).expect("it draws").plain()
}

#[test]
fn steps_after_one_go_right_of_it_stacked_with_their_if_over_them() {
    assert_eq!(
        plain(&fetched(), None, None),
        [
            " ╭─────────╮    ╭─────────────╮",
            " │ ○ fetch │─┬─▶│ ○ summarise │",
            " ╰─────────╯ │  ╰─────────────╯",
            "             │",
            "             │  if fetch.found",
            "             │  ╭─────────────╮",
            "             ╰─▶│ ○ approve   │",
            "                ╰─────────────╯",
        ]
        .join("\n")
    );
}

#[test]
fn each_box_has_its_last_firings_mark() {
    use StepStatus as S;
    let x = firing(&[("fetch", S::Done), ("summarise", S::Failed), ("approve", S::Waiting)]);
    let c = draw(&fetched(), Some(&x), None).unwrap();
    assert_eq!(
        c.plain(),
        [
            " ╭─────────╮    ╭─────────────╮",
            " │ ✓ fetch │─┬─▶│ × summarise │",
            " ╰─────────╯ │  ╰─────────────╯",
            "             │",
            "             │  if fetch.found",
            "             │  ╭─────────────╮",
            "             ╰─▶│ ? approve   │",
            "                ╰─────────────╯",
        ]
        .join("\n")
    );
    let (w, h) = c.size();
    let lines = c.lines(0, 0, w, h);
    assert!(
        lines[1]
            .spans
            .iter()
            .any(|s| s.content == "×" && s.style == theme::bad()),
        "a failure's mark is red"
    );
}

#[test]
fn the_step_under_the_caret_has_its_box_lit() {
    use StepStatus as S;
    let x = firing(&[("fetch", S::Done), ("summarise", S::Running), ("approve", S::Pending)]);
    let c = draw(&fetched(), Some(&x), Some(1)).unwrap();
    let (w, h) = c.size();
    let lines = c.lines(0, 0, w, h);
    let lit: Vec<String> = lines
        .iter()
        .flat_map(|l| &l.spans)
        .filter(|s| s.style == theme::selected())
        .map(|s| s.content.to_string())
        .collect();
    assert_eq!(lit, ["╭─────────────╮", "│", "│", "╰─────────────╯"]);
    assert!(c.plain().contains("│ … summarise │"));
}

#[test]
fn a_fan_in_shares_one_lane_and_a_chain_runs_on() {
    let def = flow(
        "  gchat:\n    run: a\n  github:\n    run: b\n  brief:\n    after: [gchat, github]\n    run: c\n  \
         post:\n    after: brief\n    run: d\n",
    );
    assert_eq!(
        plain(&def, None, None),
        [
            " ╭──────────╮    ╭─────────╮    ╭────────╮",
            " │ ○ gchat  │─┬─▶│ ○ brief │───▶│ ○ post │",
            " ╰──────────╯ │  ╰─────────╯    ╰────────╯",
            "              │",
            " ╭──────────╮ │",
            " │ ○ github │─╯",
            " ╰──────────╯",
        ]
        .join("\n")
    );
}

#[test]
fn a_long_name_or_if_is_cut_and_a_chain_runs_level() {
    let def = flow(
        "  a:\n    run: x\n  a-step-with-a-very-long-name:\n    after: a\n    \
         if: a.something_rather_long_to_say == yes\n    run: y\n",
    );
    let text = plain(&def, None, None);
    assert_eq!(
        text,
        [
            "            if a.something_rather_lon…",
            " ╭─────╮    ╭────────────────────────╮",
            " │ ○ a │───▶│ ○ a-step-with-a-very-… │",
            " ╰─────╯    ╰────────────────────────╯",
        ]
        .join("\n")
    );
}

#[test]
fn a_wide_glyph_in_an_if_takes_two_columns() {
    use unicode_width::UnicodeWidthStr;
    let def = flow(
        "  a:\n    run: x\n  b:\n    after: a\n    if: a.state == \"完了\"\n    run: y\n  \
         c:\n    after: b\n    run: z\n",
    );
    let c = draw(&def, None, None).unwrap();
    assert_eq!(
        c.plain(),
        [
            "            if a.state == 完了",
            " ╭─────╮    ╭────────────────╮    ╭─────╮",
            " │ ○ a │───▶│ ○ b            │───▶│ ○ c │",
            " ╰─────╯    ╰────────────────╯    ╰─────╯",
        ]
        .join("\n")
    );
    let (w, h) = c.size();
    for l in c.lines(0, 0, w, h) {
        assert_eq!(l.to_string().width(), w, "{l}");
    }
    // Cut, a wide glyph that would straddle the edge gives way.
    let def = flow("  a:\n    run: x\n  b:\n    after: a\n    if: a.state == \"完了完了完了完了完了\"\n    run: y\n");
    let text = draw(&def, None, None).unwrap().plain();
    assert!(text.starts_with("            if a.state == 完了完了…\n"), "{text}");
}

#[test]
fn lines_that_would_read_as_joined_are_not_drawn() {
    // `c` after `a` only, `d` after both: in one gap, `a` would seem to go to `d` by `c`'s lane.
    let def = flow(
        "  a:\n    run: x\n  b:\n    run: y\n  c:\n    after: a\n    run: z\n  d:\n    after: [a, b]\n    run: w\n",
    );
    assert!(draw(&def, None, None).is_none());
    // `c` after `a` and `b`, `b` after `a`: `a`'s line to `c` would run into its line to `b`.
    let def = flow("  a:\n    run: x\n  b:\n    after: a\n    run: y\n  c:\n    after: [a, b]\n    run: z\n");
    assert!(draw(&def, None, None).is_none());
    // Two pairs apart are drawn, each on its own lane.
    let def =
        flow("  a:\n    run: x\n  b:\n    run: y\n  c:\n    after: a\n    run: z\n  d:\n    after: b\n    run: w\n");
    assert_eq!(
        plain(&def, None, None),
        [
            " ╭─────╮     ╭─────╮",
            " │ ○ a │────▶│ ○ c │",
            " ╰─────╯     ╰─────╯",
            "",
            " ╭─────╮     ╭─────╮",
            " │ ○ b │────▶│ ○ d │",
            " ╰─────╯     ╰─────╯",
        ]
        .join("\n")
    );
}

#[test]
fn ranks_go_by_the_furthest_step_waited_on() {
    let def = flow("  a:\n    run: x\n  b:\n    after: a\n    run: y\n  c:\n    after: [b]\n    run: z\n");
    assert_eq!(ranks(&def), [0, 1, 2]);
    let def = flow(
        "  a:\n    run: x\n  b:\n    run: y\n  c:\n    after: [a]\n    run: z\n  d:\n    after: [b, c]\n    run: w\n",
    );
    assert_eq!(ranks(&def), [0, 0, 1, 2]);
}

#[test]
fn a_diamond_fans_out_and_back_in() {
    let def = flow(
        "  a:\n    run: x\n  b:\n    after: a\n    run: y\n  c:\n    after: a\n    run: z\n  d:\n    after: [b, c]\n    run: w\n",
    );
    assert_eq!(
        plain(&def, None, None),
        [
            " ╭─────╮    ╭─────╮    ╭─────╮",
            " │ ○ a │─┬─▶│ ○ b │─┬─▶│ ○ d │",
            " ╰─────╯ │  ╰─────╯ │  ╰─────╯",
            "         │          │",
            "         │  ╭─────╮ │",
            "         ╰─▶│ ○ c │─╯",
            "            ╰─────╯",
        ]
        .join("\n")
    );
}
