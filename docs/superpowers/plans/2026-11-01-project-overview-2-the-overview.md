# The Project Overview, PR 2: the overview page — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `enter` on a project line (or `everywhere`) opens that project's overview: what was said of the project on top, its flows below as rows you move through and open, and a last `+ new flow` line.

**Architecture:** The overview is a new view, `View::Overview`, beside `Work`, `Events`, `CatchUp` and `Report`. The folder it is of lives in `App.overview: Option<Scope>`, since `View` is `Copy`. Its rows come from `rows::build` with flows on, cut to that project's block and kept to `Row::Flow` and the `Row::Question`s under them. A synthetic key `NEW_FLOW` stands for `+ new flow`, the way `TOP` stands for the catch-up's top. Drawing reuses `draw_list`'s row rendering, and opening a flow reuses the flow page as the list used to (`show_pane`, `Mode::Walk`, `FlowPane`).

**Tech Stack:** Rust, ratatui, crossterm, tokio; crate `rl-cli` (`code/rl-cli/src/tui`).

**Spec:** `docs/superpowers/specs/2026-11-01-project-overview.md`. It is on branch `project-overview-design` (PR #193) and not yet on main. Read it with `git show origin/project-overview-design:docs/superpowers/specs/2026-11-01-project-overview.md`. The sections this plan implements are "The overview" and "Keys in the overview".

## Global Constraints

- The spec's word order and copy rules: plain, short, lowercase UI text, in the style of the strings already in `tui/` (e.g. `enter its firings · /flow run · /edit`).
- No backward compatibility shims (memory: no-backward-compat).
- Gates before every commit that ends a task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. A clippy warning is a failure.
- Commit messages are plain sentences in the repo's style, with no attribution lines.
- Do not touch `rl-daemon`. This PR is TUI-only.
- **Out of scope:**
  - words changing or making a flow (PR 3; typing on a flow's line keeps today's meaning);
  - parking with `^x` (PR 4; `^x` on a flow keeps today's "archives its runs");
  - writing the summary by itself when the overview opens (PR 4; this PR shows the kept one and `/summary` writes it).

## Rulings carried in (made by the controller, against the spec)

- **The summary waits for PR 4.** The spec allows it: "4. the summary at the top, if it does not fit in step 2". Asking for it when the overview opens can stop on a cost question (`Mode::SummaryCost`) and needs a "recent enough" rule, so it goes with PR 4. Here the top shows the project's kept summary, or `no summary yet · /summary writes it`. While one is being written it shows `summarising… <elapsed>`. `/summary` in the overview targets the project. Cost if wrong: one more task in PR 4.
- **Looking at a flow is landing on it.** The cursor landing on a flow's row in the overview marks its runs seen (`saw_selected`, as rows on the list do), which is what lets a failed-unseen light go out. Cost if wrong: one condition.
- **`→ ←` stay.** In the overview they open and fold a flow's latest-firing steps line, as they did on the list; `open_flows` serves them. The prune in `refresh` is now checked against the loaded flows' keys (`rows::flow_key` of each `self.flows` entry not removed) instead of a whole `rows::build` with flows on.
- **`+ new flow` puts `/flow new ` on the line.** That reuses today's drafting path. PR 3 turns it into words in the pane.
- **`^e` with an empty line on a flow's row in the overview is `/flow edit <it>`.** A proposal up keeps `^e` for itself, as today.
- **`show_flow` opens the overview** of the flow's project, or of `everywhere` for a flow with no `in`, with the cursor on the flow. If a search hides it there, the search lets go, as `show_run` does.

## Review Focus

1. **A project with no flows.** The overview still opens. It shows the top and `+ new flow` only, and the cursor lands on `+ new flow`.
2. **The flow under the cursor disappears.** If the flow is removed or reloaded away while the overview is up, `settle` moves the cursor to the first row, or `+ new flow`, and nothing panics.
3. **`esc` from inside the flow page goes back to the overview, not the list.** A second `esc` reaches the list, landing on the same project line it left from. `ctrl-\` goes straight to the list.
4. **A flow question in the overview is answered by typing `1 and a note`**, exactly as on the list (`question_here` must include the overview).
5. **The headline and the toggles.** The overview's headline is the project's name and its tally. The agents/shells/hidden toggles are not drawn or applied there, and `⇧↑↓` says it does not move rows here.

---

### Task 1: The overview view: its rows, its keys to open and leave it, and how it is drawn

**Files:**
- Modify: `code/rl-cli/src/tui/mod.rs`:
  - `View`;
  - the `App` fields (`overview`);
  - the headline in `draw` (around lines 2120–2130);
  - the list dispatch (around line 2211);
  - `draw_list`'s callers;
  - the footer (around lines 3255–3300);
  - `NEW_FLOW` next to `TOP` (line 324).
- Create: `code/rl-cli/src/tui/overview.rs`, the `impl App` cluster:
  - `open_overview`, `overview_rows`, `draw_overview`, `overview_top_lines`;
  - add `mod overview;` in `mod.rs`.
- Modify: `code/rl-cli/src/tui/select.rs`: the `keys`, `matches`, `selected` and `question_here` arms for `View::Overview`.
- Modify: `code/rl-cli/src/tui/keys.rs`: `enter_row`'s `Row::Group` arm opens the overview, and the `View::Overview` arm of `enter_row`.
- Modify: `code/rl-cli/src/tui/act.rs`: the `⇧↑↓` refusal at line 478 includes `View::Overview`.
- Modify: `code/rl-cli/src/tui/shortcuts.rs`: `Place::Overview`, with its keys.
- Test: `code/rl-cli/src/tui/overview_tests.rs` (new; `#[cfg(test)] mod overview_tests;` in `mod.rs`). Build its fixtures the way `flow_tests.rs::flow_in` does.

**Interfaces:**
- Produces:
  - `pub(super) enum Scope { Project(PathBuf), Everywhere }` in `overview.rs`, with `fn folder(&self) -> Option<&Path>`.
  - `App.overview: Option<Scope>`.
  - `View::Overview`.
  - `const NEW_FLOW: &str = "+new";` in `mod.rs`.
  - `App::open_overview(&mut self, scope: Scope, at: Option<String>)`: opens the view with the cursor on `at` when it is one of its keys, else on the first flow, else on `NEW_FLOW`.
  - `App::overview_rows(&self) -> Vec<Row<'_>>`: the scope's `Row::Flow`s, each followed by its `Row::Question`s. Flows that need you (`rows::flow_needs`) come first, then by name. Hidden flows are included (`show_hidden: true`), and the search narrows them as on the list.
  - `App::on_new_flow(&self) -> bool`.

- [ ] **Step 1: Write the failing tests** in `overview_tests.rs`:
  - `enter_on_a_project_line_opens_its_overview_with_its_flows_and_new_flow_last`: two flows `in` the folder, one waiting for a yes. `enter` on the project's line gives `app.view == View::Overview`. `app.keys()` is `["f:<needs-yes one>", "f:<other>", NEW_FLOW]`, and `!app.show_pane`.
  - `a_project_with_no_flows_opens_on_new_flow`: `app.keys() == [NEW_FLOW]` and `app.sel.as_deref() == Some(NEW_FLOW)`.
  - `enter_on_everywhere_opens_the_flows_with_no_in`: a flow with no `in` and one with one. Only the first is in `keys()`.
  - `esc_leaves_the_overview_for_the_project_line`: after `esc`, `app.view == View::Work` and `app.sel == Some("g:<folder>")`.
  - `the_overview_draws_its_top_its_flows_and_new_flow`: render to a `TestBackend`, as the existing draw tests in `mod.rs` do. Assert the buffer contains:
    - the project name;
    - `no summary yet · /summary writes it`;
    - each flow's name;
    - `+ new flow`.
  - `a_flows_question_stands_under_it_and_the_line_answers_it`: a decision from the flow's firing (build it as the restored catch-up test `a_firing_owned_question…` in `rows/tests.rs` does). It is a key right after its flow's. `question_here()` is `Some` with it selected.
  - `the_flow_under_the_cursor_going_away_moves_the_cursor`: remove the flow from `app.flows`, call `settle()`, and the cursor is on the remaining first key.

- [ ] **Step 2: Run them, expecting a compile failure.** `cargo test -p rl-cli overview_tests` fails with `View::Overview` not found.

- [ ] **Step 3: Implement.**
  - `overview_rows`:
    - `rows::build` with `show_flows: true, show_agents: true, show_shells: true, show_hidden: true`, the current `filter`, and `relay`.
    - Take the block after the `Row::Group` whose `workspace` is the scope's folder, or the `everywhere` head for `Scope::Everywhere` (`Head::is_everywhere`), up to the next `Row::Group`.
    - Keep `Row::Flow`, and the `Row::Question`s that follow a kept flow.
    - Reorder whole flow-plus-questions blocks: needs first, then name.
  - `keys()` for the view: the rows' keys, then `NEW_FLOW`. `settle()` already falls back to the first key.
  - `draw_overview(f, area)` draws, top to bottom:
    - the lines of `overview_top_lines(width)`:
      - the kept summary, through `pane::summary_section` with `summary_of_row` on the scope's `Row::Group`;
      - else `summarising… <elapsed>` (`self.summarising` with that target);
      - else ` no summary yet · /summary writes it`, dimmed;
    - one blank line;
    - the rows, through the same per-row rendering `draw_list` uses. Factor `draw_list`'s per-row loop into `fn row_lines(&self, rows: &[Row], width) -> (Vec<Line>, Vec<Option<String>>, usize)` if that is the least code;
    - the line ` + new flow`, accented when selected;
    - all of it through `draw_window` so it scrolls and the mouse hits work.
  - Headline: `<project name> · <tally words>`. Use `names.workspace(folder)`, or `rows::EVERYWHERE` for everywhere, and `rows::tally(...).words()`, leaving out ` · ` when the tally is empty. Add `"esc to the list"` to the notes, as the catch-up does.
  - Open it from `enter_row`'s `Row::Group` arm: `self.open_overview(scope, None)`. Remember the list's selection with `open_view`, which `switch_to` keeps per view.
  - `esc` with no subpage up, in `View::Overview`: `self.overview = None; self.go_back();`. Check where `esc` returns from the catch-up in `keys.rs` (around line 685) and add the arm there.
  - Footer for the view:
    - on a flow: `enter its firings · /flow run · ^e edits · ^x archives · /summary · esc the list`. Keep the needs-a-yes `/flow approve` variant, since `/flow approve` still exists in `flow_command`;
    - on a question: as on the list;
    - on `NEW_FLOW`: `enter writes one · esc the list`.
  - `shortcuts::Place::Overview` lists `↑↓ moves · enter opens · → ← its steps · ^e edits · /summary · esc the list`.

- [ ] **Step 4: Run the tests and the gates.** `cargo test -p rl-cli overview_tests` passes, then the three gates.

- [ ] **Step 5: Commit.**
  ```
  git add -A code/rl-cli/src/tui
  git commit -m "The project overview: enter on a project line lists its flows under what was said of it, and + new flow"
  ```

### Task 2: Inside the overview: open, walk, edit, new, and the flow tests back

**Files:**
- Modify: `code/rl-cli/src/tui/keys.rs`:
  - `enter_row`: in `View::Overview`, a flow opens its page as the list's flow arm used to (`Pane::Facts`, `show_pane`, `load_pane`), and `NEW_FLOW` puts `/flow new ` on the line;
  - `→ ←`: the condition becomes `matches!(self.view, View::Work | View::Overview)`, or just `View::Overview`, since the list has no flows;
  - `^e`: on an empty line in `View::Overview` with a flow selected and no proposal up, `self.edit_flow(&name)`;
  - `esc` with the flow page up in `View::Overview`: `leave_subpage()` only. The view stays.
- Modify: `code/rl-cli/src/tui/flow_ops.rs`: `show_flow` opens the overview. Drop its list-line logic.
- Modify: `code/rl-cli/src/tui/catch_ops.rs`, `events` enter: they already call `show_flow`; only their hint texts change.
- Modify: `code/rl-cli/src/tui/mod.rs`:
  - the `refresh` prune of `open_flows` is checked against the loaded flows' keys (see Rulings);
  - the dead non-catch-up `Row::Flow` footer arm is deleted, since Task 1's overview footer replaces it;
  - the catch-up/events hints: `enter its project on the list` becomes `enter it in its project`, and `enter its flow on the list` becomes `enter it in its project`.
- Modify: `code/rl-cli/src/tui/flow_tests.rs`: restore the tests deleted in #194, from `git show c61770f^:code/rl-cli/src/tui/flow_tests.rs`, opening the overview first instead of finding the flow on the list:
  - `enter_twice_on_a_flow_walks_its_steps_and_enter_on_one_reads_its_run`
  - `ctrl_backslash_leaves_a_walked_flow_for_the_list_in_one_go`: now asserts `app.view == View::Work`
  - `summary_on_a_flow_showing_an_older_firing_tells_that_firing` (with `pick_older`)
  - `the_firing_picked_goes_when_the_cursor_leaves_the_row`
  - `right_opens_a_flows_row_to_its_latest_firings_steps_and_left_folds_it`
  - `a_flow_opened_stays_open_while_a_search_hides_it`
  - `a_flow_with_a_yes_that_never_fired_is_a_row_like_any_flows`
  - `a_flow_whose_runs_were_archived_has_fired_all_the_same`
- Modify: `code/rl-cli/src/tui/mod.rs` tests: restore from `git show c61770f^:code/rl-cli/src/tui/mod.rs`, adapted the same way:
  - `a_flows_summary_starts_at_the_run_you_had_not_seen`
  - `a_flows_row_says_where_its_latest_firing_stands`
  - `edit_opens_the_selected_flows_file`
  - `a_flows_row_stands_in_its_project_however_its_in_is_spelled`
  - `a_flow_in_the_events_view_is_found_…`: the current version asserts the stopgap; it now asserts `View::Overview`, with the flow selected.
  - `enter_on_a_flow_in_the_catch_up_…`: the same.
  - Only restore tests whose subject still exists. `toggle_flows`, `group_count` and `the_header_count_…` stay gone.

**Interfaces:**
- Consumes: Task 1's `Scope`, `open_overview(scope, at)`, `NEW_FLOW`, `on_new_flow()`, `View::Overview`.
- Produces: nothing new for later tasks.

- [ ] **Step 1: Write the new failing tests** in `overview_tests.rs`:
  - `enter_on_a_flow_opens_its_page_and_esc_comes_back_to_the_overview`: `enter`, then `app.show_pane`. `esc`, then `!app.show_pane && app.view == View::Overview` with the same `sel`. `esc` again, then `View::Work`.
  - `enter_on_new_flow_puts_flow_new_on_the_line`: `app.line.text() == "/flow new "`.
  - `ctrl_e_on_a_flow_opens_its_file`: `app.editor.is_some()`, with the editing file being `<home>/flows/<name>/flow.md`.
  - `show_flow_opens_its_projects_overview_on_it`: from the events view; `app.view == View::Overview`, `app.sel == Some("f:<name>")`.
  - `show_flow_for_a_flow_with_no_in_opens_everywhere`.

- [ ] **Step 2: Restore the deleted tests listed above, adapted.** Run `cargo test -p rl-cli`. Expect the new ones and the adapted restored ones to fail.

- [ ] **Step 3: Implement the key and `show_flow` changes, the prune, and the hint texts.**

- [ ] **Step 4: Run all tests and the gates.**

- [ ] **Step 5: Commit.**
  ```
  git commit -am "In the overview: enter opens a flow and walks it, ^e edits it, + new flow starts one; flows found elsewhere open there; the flow page's tests are back"
  ```

### Task 3: `/summary` in the overview, and the docs

**Files:**
- Modify: `code/rl-cli/src/tui/summary_ops.rs`: `summary_target()` in `View::Overview`:
  - on `NEW_FLOW`, or with nothing selected: the scope's target (`SummaryTarget::Project { workspace }`, or `All` for everywhere);
  - on a flow or a question: as today.
  - `summary_start`: in `View::Overview` with the scope's target, do not open the pane (`show_pane` stays false); the top shows it.
  - `summarised`: whatever the selection, set `self.summary` when the target is the overview scope's. Check that `summary_of_row` on the scope's group finds it. If `self.summary` is one slot, keep the overview's in `self.made` and have `overview_top_lines` read `self.made.get(&target.key())` first.
- Modify: `code/rl-cli/src/tui/palette.rs`: `/summary`'s help mentions the overview.
- Modify: `docs/ARCHITECTURE.md`: the TUI section gets a paragraph on the overview:
  - `View::Overview` and `Scope`;
  - its rows;
  - `NEW_FLOW`;
  - that flows are reached only there and from the catch-up/events.

  Remove any sentence still saying the project line's `enter` shows its stats.
- Test: `overview_tests.rs`.

- [ ] **Step 1: Write the failing tests:**
  - `summary_on_new_flow_in_the_overview_tells_the_project`: `app.summary_target() == Some(SummaryTarget::Project { workspace: folder })`.
  - `a_project_summary_made_shows_at_the_overviews_top`: insert a `Summary` for the project target into `app.made`, as the existing summary tests in `mod.rs` build one. Render, and assert its text is in the buffer and `no summary yet` is not.
  - `summarising_the_project_says_so_at_the_top`: set `app.summarising` to a project target. Render, and the buffer contains `summarising`.

- [ ] **Step 2: Run them, expecting failures.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and the gates.**
- [ ] **Step 5: Commit.**
  ```
  git commit -am "/summary in the overview tells the project and shows at its top; ARCHITECTURE says where flows are now"
  ```
