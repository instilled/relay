# Project overview, PR 1: flows off the list — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The list shows only runs and agents' own sessions. Each project line sums up its flows (`3 flows · 1 running · 1 needs you`), in amber when one needs you. The `f` toggle goes.

**Architecture:** `rows::build` already takes flows off the list when `Context.show_flows` is false. The list now always builds with it false, and that path is tightened: every flow's run leaves, an agent step too, and so do its questions. The catch-up keeps building with it true. The header's flow chips get a `FlowTally` computed in `rows::head` from the loaded flows and the digest. `header_with` draws the tally where `group_count` drew the bare count.

**Tech Stack:** Rust, ratatui; crate `rl-cli` (`code/rl-cli/src/tui`).

**Spec:** `docs/superpowers/specs/2026-11-01-project-overview.md` (on branch `project-overview-design`, PR #193). This plan covers its "The list" and "The project line" sections. The overview, word-editing and parking come in later plans.

## Global Constraints

- Gates before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. A clippy warning is a CI error.
- No backward compatibility: no fallback for old saved toggles or old row orders. An `f:` key in a saved order is simply ignored.
- Match the surrounding code: doc comments in the codebase's plain, terse style; names in its idiom (`tally`, `needs`, not `FlowSummaryInfo`).
- Tally wording, exactly:
  - `N flow` or `N flows`;
  - then `· N running`, left out when it is 0;
  - then `· N needs you`, left out when it is 0;
  - nothing at all when the project has no flows.
- "Needs you", for a flow:
  - its status is `FlowStatus::NeedsApproval`; or
  - an open question (`Decision`) belongs to one of its firings or runs; or
  - one of its runs is `NeedsInput` or `Blocked`; or
  - one of its runs is `Failed` and still `unseen`.
- "Running", for a flow: a firing of it in the digest is `FiringStatus::Running` or `Waiting`.

## Review Focus

- **A flow's agent step that asks a question:** its run is an agent run, and `keeps` lets agents through. It must still leave the list, and its question must leave too. Its project lights up instead.
- **A flow `in` a folder with no other runs:** the project's line still shows, carrying the tally.
- **A flow with no `in`:**
  - the `everywhere` block leaves the list;
  - the headline's `everywhere` note carries the same "needs you" signal;
  - a failed, unseen run of such a flow lights the headline amber.
- **A flow run that is pinned or hidden-then-come-through:** a flow's run never reaches the pinned band or the come-through band. Only the light says it.
- **A search with `kind:flow` on the list:** it finds nothing (no flow rows). The catch-up still finds them.

---

### Task 1: The list builds without any flow content

**Files:**
- Modify: `code/rl-cli/src/tui/rows/group.rs` (`keeps`)
- Modify: `code/rl-cli/src/tui/rows/build.rs` (`build`: the question loop, the band)
- Test: `code/rl-cli/src/tui/rows/tests.rs`

**Interfaces:**
- Produces:
  - `rows::Context.show_flows` keeps its name and meaning: "flows and their runs have rows". The list passes `false` (Task 3); the catch-up passes `true`, as now.
  - `keeps(r, show_agents, show_flows, show_shells) -> bool`, same signature, new rule: a run with `Cause::Flow` is kept only when `show_flows`, whatever its kind.

- [ ] **Step 1: Write the failing tests** in `rows/tests.rs`, using `toggles_build`, `fired`, `flow`, `question` and `digest`:
  - `with_flows_off_no_flow_run_nor_its_question_is_on_the_list`:
    - one `fired("a1", "pulse", RunState::NeedsInput, …)` whose `run.kind` is set to `RunKind::Agent`;
    - one plain run `r:w` in the same folder;
    - a `question("q1", ws, Some("a1"), …)`;
    - `flow("pulse", Some(ws))`.
    - Build with `(false, true, true, false)`.
    - Assert `keys(&rows) == ["[relay]", "r:w"]`: no `r:a1`, no `f:pulse`, no `q:q1`.
  - `with_flows_off_a_pinned_or_come_through_flow_run_is_not_in_the_band`: a pinned `fired` run and one hidden earlier that has since failed. With `(false, true, true, false)`, neither key appears anywhere.
  - In `the_toggles_keep_a_run_by_what_it_is_or_what_started_it`: change the existing expectation for a flow's agent step with flows off. It is now not kept. Update its message to say a flow's run is the flow's, whatever its kind.

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p rl-cli --lib rows::tests::with_flows_off`
  Expected: FAIL. The agent step `r:a1` and `q:q1` are still listed.

- [ ] **Step 3: Implement**
  - **`keeps`:** `is_flow` short-circuits. When `is_flow`, return `show_flows`; otherwise use today's rule over agent, shell and other. Update its doc comment: a flow's run is the flow's, whatever its kind.
  - **`build`:**
    - In the question loop, when `!cx.show_flows` and the question's `by` is `Some(_)` (a flow's), skip it.
    - The pinned branch (`r.pinned_at.is_some()`) checks `keeps` first, so a flow's pinned run is skipped when flows are off.
    - The come-through branch already checks `keeps`.

- [ ] **Step 4: Run the rows tests**

  Run: `cargo test -p rl-cli --lib rows::`
  Expected: PASS. Fix any older test whose flows-off expectation relied on an agent flow step showing, and say why in its message.

- [ ] **Step 5: Commit**: `git commit -am "The list without flows: a flow's run and its question leave with the flows toggle off, whatever their kind"`

### Task 2: The project line's flow tally and its light

**Files:**
- Modify: `code/rl-cli/src/tui/rows/model.rs`:
  - add `FlowTally`;
  - `Head.flows: Vec<Chip>` becomes `Head.tally: FlowTally`;
  - `Head::everywhere()` gets a default tally.
- Modify: `code/rl-cli/src/tui/rows/group.rs`: add `tally`.
- Modify: `code/rl-cli/src/tui/rows/build.rs`: `head()` fills `tally`.
- Modify: `code/rl-cli/src/tui/rows/order.rs`:
  - `everywhere` and `global_parts` take a `FlowTally`;
  - the `/flow approve` hint goes. That command is gone, as a palette test says.
- Modify: `code/rl-cli/src/tui/mod.rs`:
  - `header_with` draws `tally.words()`, styled `theme::needs()` when `tally.needs > 0`, else `theme::dim()`;
  - delete `group_count`;
  - the headline's caller passes the `None`-scope tally.
- Modify: any other reader of `Head.flows` or `chips` that the compiler points to. `pane.rs::flows_section` keeps using `chips`; that is fine until PR 2.
- Test: `code/rl-cli/src/tui/rows/tests.rs`

**Interfaces:**
- Consumes: Task 1's `keeps`.
- Produces, used by PR 2:

```rust
/// A project's flows in three numbers, as its line says them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlowTally { pub total: usize, pub running: usize, pub needs: usize }
impl FlowTally {
    /// `3 flows · 1 running · 1 needs you`; the zero parts left out; empty with no flows.
    pub fn words(&self) -> String;
}
/// The flows `in` one folder (`None`: those with no `in`), tallied from the digest.
pub fn tally(flows: &[Flow], scope: Option<&Path>, d: &Digest) -> FlowTally;
/// Whether one flow needs you (Global Constraints' rule).
pub fn flow_needs(f: &Flow, d: &Digest) -> bool;
```

- [ ] **Step 1: Write the failing tests**
  - `a_flow_tally_in_words`:
    - `FlowTally{total:3,running:1,needs:1}.words() == "3 flows · 1 running · 1 needs you"`;
    - `{1,0,0}` gives `"1 flow"`;
    - `{2,0,2}` gives `"2 flows · 2 needs you"`;
    - `{0,0,0}` gives `""`.
  - `a_project_line_tallies_its_flows_and_what_needs_you`, with `flow("a", Some(ws))`, `flow("b", Some(ws))`, `flow("c", Some(ws))` and `flow("x", None)`:
    - `a` has a firing `FiringStatus::Running`;
    - `b`'s status is `NeedsApproval` (no `approved_hash`);
    - `c` has a `fired("c1","c",RunState::Failed,…, unseen=true)`.
    - Build the list (default toggles, flows off).
    - `head_of(&rows, "relay").tally == FlowTally{total:3, running:1, needs:2}`.
    - Mark `c1` `unseen=false`: `needs == 1`.
    - Replace that with a `question` asked by `c1`: `needs == 2`.
  - Update `the_headline_says_the_flows_with_no_in_and_a_yes_they_wait_for` to the new `everywhere(tally)` text, e.g. `"↻ 2 everywhere · 1 needs you"`, amber. Assert the `/flow approve` hint is gone.

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -p rl-cli --lib rows::tests::a_flow_tally rows::tests::a_project_line_tallies`
  Expected: compile error, because `FlowTally` does not exist yet.

- [ ] **Step 3: Implement** `FlowTally`, `words`, `tally` and `flow_needs` in `group.rs` (the struct in `model.rs`).
  - Skip removed flows (`removed_at`), as `chips` does.
  - A flow's runs are `d.items` with `Cause::Flow { flow, .. } == f.name`.
  - A question belongs to it when `firing_of(q, &d.firings)` names it, or its asking run is one of the flow's runs.
  - Fill `Head.tally` in `head()` via `tally(cx.flows, Some(workspace), d)`.
  - Then change `order.rs::everywhere` and `global_parts`: `↻ N everywhere`, then the running and needs parts as in `words()`, and `Tone::Needs` when `needs > 0`.
  - Draw it in `header_with`.

- [ ] **Step 4: Run the TUI lib tests**

  Run: `cargo test -p rl-cli --lib`
  Expected: PASS. Fix header-draw snapshot assertions that expected the old `N flows` text or the chip amber, keeping their intent.

- [ ] **Step 5: Commit**: `git commit -am "The project line tallies its flows: running, and what needs you, in amber"`

### Task 3: The list without the flows toggle

**Files:**
- Modify: `code/rl-cli/src/tui/mod.rs`:
  - the `App.show_flows` field and its initialisation;
  - delete `toggle_flows`;
  - `toggles`, `reset_toggles` and `set_toggles` become `(agents, shells, hidden)`;
  - `toggles_line` drops the `⚑ f` entry;
  - the empty-list message drops `f flows` and the `self.show_flows` checks.
- Modify: `code/rl-cli/src/tui/keys.rs`: remove the `KeyCode::Char('f')` arm. A bare `f` on an empty line now just types.
- Modify: `code/rl-cli/src/tui/select.rs`:
  - every `rows::Context` built for the list passes `show_flows: false`;
  - the `(bool,bool,bool,bool)` toggles become `(bool,bool,bool)`;
  - the "everything shown at once" sets become `[(true,true,false),(true,true,true)]`.
- Modify: `code/rl-cli/src/tui/palette.rs`:
  - remove the `flows` toggle command;
  - update the palette tests that name it (`names("flow") == ["flow"]`, and the shared-prefix comments).
- Modify: `code/rl-cli/src/tui/shortcuts.rs`: drop both `key("f", "flows")`.
- Modify:
  - every other `rows::Context` the list path builds (the compiler lists them) passes `show_flows: false`, for example `act.rs:496` if it is the list's;
  - catch-up callers (`catchup.rs`, `catch_ops.rs`) keep `true`;
  - `act.rs` and the views code that saved four toggles: the compiler points to them.
- Modify: `docs/ARCHITECTURE.md`:
  - the list paragraph says flows are not on the list, and the project line tallies them;
  - the toggles paragraph drops `f`.
- Test: `code/rl-cli/src/tui/mod.rs` tests, `rows/tests.rs` helpers.

**Interfaces:**
- Consumes: Task 1 (`show_flows: false` takes all flow content off) and Task 2 (the header shows the tally).
- Produces: `App::toggles() -> (bool, bool, bool)` as `(agents, shells, hidden)`.

- [ ] **Step 1: Write the failing test** in `tui/mod.rs`: `the_list_has_no_flows_and_f_types_on_the_line`.
  - Build an `App` from the existing test helper, with a digest holding a flow run and a plain run in one project.
  - The drawn list has no flow row.
  - Pressing `f` on an empty line puts `f` on the line.
  - `app.toggles()` is unchanged.

- [ ] **Step 2: Run it to see it fail**

  Run: `cargo test -p rl-cli --lib the_list_has_no_flows`
  Expected: FAIL. The flow row is drawn, and `f` toggles.

- [ ] **Step 3: Implement** the removals listed under Files. The `rows/tests.rs` helpers keep their 4-tuple. That is `rows::Context`'s, not `App`'s, so `DEFAULT` there becomes `(false, true, true, false)`. Fix every older rows test that expected flow rows on the default list:
  - Tests that are about flow rows (marks, folding, `a_flows_runs_are_one_row`, `every_flow_stands_at_the_bottom…`) build with flows on explicitly, because the catch-up still uses them.
  - Tests about the list's default say there are no flows.

- [ ] **Step 4: Run the gates**

  Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  Expected: all clean.

- [ ] **Step 5: Commit**: `git commit -am "The list: flows leave it, and the f toggle with them; the project line says how they stand"`
