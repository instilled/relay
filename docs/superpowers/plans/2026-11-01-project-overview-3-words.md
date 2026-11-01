# Project overview, PR 3: changing and making flows with words

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the project overview, words typed on a flow's line change that flow, and words typed on `+ new flow` make one. Either way, the assistant's proposal shows in the pane, where more words refine it, `enter` saves it, `^e` opens it in `$EDITOR` first, and `esc` drops it.

**Architecture:**
- **A new pane, `FlowProposal`** (in `tui/flow_proposal.rs`), modelled on the run `Proposal` (#189). It is a field on `App`, not a `Mode`, so the line stays live for more words.
  - It holds the words asked, the wait, the error, and the last draft as a `new_flow::Drafted`, which already does the load check, scrolling, the seen-end rule and the settle rule.
  - For a change, it also holds the flow's name and the `flow.md` it started from (the base), and draws a line diff of the base against the proposal.
- **It replaces `Mode::Draft`, `new_flow::Parked` and `App::parked`.**
  - `/flow new <name> <words>`, a flow answered to plain words (`Want::Any`), and the overview's words all land in this one pane.
  - `/flow draft` brings it back, as `/ask` brings back a run proposal.
- **Below the TUI:**
  - `Want::NewFlow { folder }` lets the assistant name a new flow itself.
  - `interpret::changing_flow` builds the words for a change.
  - `flow::write_changed` writes a changed `flow.md` and gives its yes as `/flow edit` does.

**Tech Stack:** Rust, ratatui/crossterm, tokio; crates `rl-cli` (TUI in `code/rl-cli/src/tui`) and `rl-flow` (`def::valid_name`, `hash`).

**Spec:** `docs/superpowers/specs/2026-11-01-project-overview.md` (on branch `project-overview-design`, PR #193), sections "Keys in the overview" and "Changing and making flows with words".

## Global Constraints

- Gates before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`, all clean.
- **No backward compatibility** (repo memory): the old draft path (`Mode::Draft`, `Parked`) is removed, not kept beside the new one.
- **Copy:** lower case and short, with ` · ` between clauses, as the rest of the TUI writes.
  - Wait line: ` {dots} asking the assistant · {secs}s`, or ` {dots} changing it · {secs}s` once a draft exists. Reuse `proposal.rs`'s `DOTS`.
  - Under the wait: `     its run is in the list · /stop there · esc leaves it asking`.
- **Keys with the flow proposal up and the line empty:**
  - `enter` saves it;
  - `^e` opens it in `$EDITOR`;
  - `esc` drops it (or, while still asking, leaves it asking);
  - `↑↓ pgup pgdn` scroll it.
  - With words on the line, `enter` sends them as a refinement.
- **A save is refused:**
  - until the draft has been on screen `SETTLE` (300ms);
  - until its last line has been drawn;
  - while it has problems (`flow::problems`, or the name check for a new flow).
  The footer and flash say why, as `Drafted::hint` and `draft_key` do today.
- **The TUI never writes under `~/.relay/flows` except through `crate::flow` functions.**
- No attribution lines in commits.

## Review Focus

1. **The flow changed on disk while the proposal was up** (the person edited it, or another proposal was saved). `enter` must refuse with `{name}'s flow.md changed since it was asked · type to ask again` and write nothing. Pinned in Task 1 (`write_changed_refuses_a_moved_base`) and Task 3.
2. **An answer arrives after the pane was left with `esc`:** it must land in the pane, unshown, with a flash `{agent} proposed {what} · /flow draft shows it`. It must never take keys or pop up. Pinned in Task 2 (`an_answer_after_esc_waits_unshown`).
3. **The assistant answers a run where a flow was asked:** the error shows in the pane as `asked for a flow; {agent} answered a run (run {id})`, the pane stays up, and more words ask again. Pinned in Task 2.
4. **A change proposal for a flow that does not load today,** or whose answer names a different flow: the name is the overview's flow, always. Any `name:` the assistant answered is ignored for a change. Pinned in Task 2 (`a_change_keeps_its_flows_name`).
5. **Words on a flow's line that start with a runnable word** (`make it run daily`, where `make` is on `$PATH`): in the overview, on a flow or on `+ new flow`, they are words, not a shell line, unless the line starts with `!`. Pinned in Task 3.

---

### Task 1: The words, the want, and the writer below the TUI

**Files:**
- Modify: `code/rl-cli/src/context.rs` (the `Want` enum, `contract`, `render`)
- Modify: `code/rl-cli/src/interpret.rs` (add `changing_flow`; `ask`'s timeout)
- Modify: `code/rl-cli/src/flow/edit.rs` and `code/rl-cli/src/flow/mod.rs` (re-export `write_changed`)
- Create: `code/rl-cli/src/tui/diff.rs`, with its tests in the same file
- Test: in-file `#[cfg(test)]` modules, plus `code/rl-cli/tests/flow.rs` for `write_changed`, using the `rld` harness the other tests there use.

**Interfaces:**
- Produces:
  - `Want::NewFlow { folder: Option<PathBuf> }`
  - `pub fn interpret::changing_flow(asked: &[String], text: &str) -> String`
  - `pub async fn flow::write_changed(rpc: &mut Client, home: &Home, name: &str, base: &str, text: &str, known: &[PathBuf]) -> Result<Edited>`
  - `pub fn flow::start_changed(home: &Home, name: &str, base: &str, text: &str) -> Result<Editing>`
  - `pub(super) enum diff::Tag { Same, Gone, Added }`
  - `pub(super) fn diff::lines<'a>(old: &'a str, new: &'a str) -> Vec<(Tag, &'a str)>`

- [ ] **Step 1: Write the failing tests.**

  `context.rs`:
  - `a_new_flow_is_named_by_the_assistant`: `contract(&Want::NewFlow { folder: Some("/w".into()) })` contains `name: <a new flow's name: a-z, 0-9 and ->`, contains `its in: /w`, and does not contain `kind: run`.
  - With `folder: None`, it contains `it has no in`.
  - `render(…, &Want::NewFlow{..})` says `You are drafting a new flow`.

  `interpret.rs`:
  - `changing_flow_carries_the_text_and_the_change`: with `asked = ["every day", "only weekdays"]` and `text = "---\nwhen: every 1d\n---\n"`, the output:
    - contains `- every day`;
    - contains `  when: every 1d` (indented two spaces like `changing`);
    - contains `Change it: only weekdays`;
    - ends with `Answer with the whole flow, changed, in the same shape.\n`.
  - With one `asked`, it has no `Asked before:` block.

  `diff.rs`:
  - `same_text_is_all_same`.
  - `a_changed_line_is_gone_then_added`: `lines("a\nb\nc\n", "a\nB\nc\n")` == `[(Same,"a"),(Gone,"b"),(Added,"B"),(Same,"c")]`.
  - `added_at_the_end`.
  - `everything_new_from_empty`.
  - A trailing `\n` makes no extra empty line.

  `tests/flow.rs`:
  - `write_changed_approves_a_changed_flow`: a flow written and approved, then `write_changed` with its text plus a changed `when:` returns `Edited::Approved`, and the file holds the new text.
  - `write_changed_refuses_a_moved_base`: the file differs from `base`; the call errors with `{name}'s flow.md changed since it was asked · type to ask again`, and the file is untouched.
  - `write_changed_refuses_what_would_not_load`: the text has a bad `when:`; it errors with `{name} would not load: …`, and the file is untouched.

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p rl-cli changing_flow` and the others. Expect compile errors for the missing items.

- [ ] **Step 3: Implement.**
  - **`Want::NewFlow { folder }`:**
    - `contract` gives the flow shape with `name: <a new flow's name: a-z, 0-9 and ->`.
    - Then one line: with `Some(f)`, `Name it, and set its in: {f}, unless the words name another folder.`; with `None`, `Name it; it has no in: it works in no one project.`
    - Then `after`.
    - `render`'s `drafting` is `a new flow`.
    - `ask`'s 3m timeout covers `NewFlow` too: `matches!(want, Want::Flow{..} | Want::NewFlow{..})`.
    - Every `match` on `Want` in the crate gets the arm. `summary_ops::ask` treats `NewFlow` like `Flow` for now; Task 2 rewires it.
  - **`changing_flow`** mirrors `changing`, with `Proposed for it:` replaced by `Its flow.md now:` and the text indented two spaces per line.
  - **`diff::lines`:** an LCS over lines, O(n·m), since flows are short. Output order: for a replaced run, the gone lines before the added ones.
  - **`write_changed`**, in order:
    1. `valid_name`, else bail as `start_edit` does.
    2. Read `flow.md`. If it differs from `base`, bail with the copy above.
    3. `problems(name, text, known)`. If there is a first problem, bail with `{name} would not load: {first}`.
    4. `start_edit(home, name)`, then overwrite `editing.file` with `text`, then `finish_edit(rpc, editing)` and return its `Edited`.
    5. If `finish_edit` errors, or returns `NotLoaded`, write `base` back before returning. `NeedsYes` keeps the text, as `/flow edit` does.
  - **`start_changed`** is the `^e` path:
    1. The same base check.
    2. `start_edit`, then overwrite `flow.md` with `text`.
    3. Set `yours = true` on the `Editing`, so a save with no change is still said yes to.
    4. Return it. `was` stays the old text, so `finish_edit` sees a change.
    5. Its doc comment says a caller whose editor fails to open must write `base` back. Expose `pub fn flow::restore(e: &Editing, base: &str) -> Result<()>` for that.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the three gates.

- [ ] **Step 5: Commit:** `Below the overview's words: a new flow the assistant names, the words for a change, a changed flow.md written and said yes to, a line diff`.

### Task 2: The flow proposal pane, in place of the draft mode

**Files:**
- Create: `code/rl-cli/src/tui/flow_proposal.rs` and `code/rl-cli/src/tui/flow_proposal_tests.rs`
- Modify:
  - `code/rl-cli/src/tui/new_flow.rs`: remove `Parked`, `drafted`, `show_parked`, `show_draft`, `draft_key` and `write_draft`, moving what lives on into `flow_proposal.rs`. `Drafted` stays, and gains `pub fn with_base(self, base: String) -> Self` for drawing a diff.
  - `code/rl-cli/src/tui/mod.rs`: remove `Mode::Draft` and `parked`; add `flow_proposal: Option<FlowProposal>`; the draw, footer and title sites move from `Mode::Draft` to `flow_proposal_shown()`; `leave_subpage`/esc order.
  - `code/rl-cli/src/tui/keys.rs`: the `Mode::Draft` arm is removed; `↑↓ pgup pgdn`, `^e` and empty `enter` route to the flow proposal when it is shown; `/ask`.
  - `code/rl-cli/src/tui/summary_ops.rs`: `ask` and `answered`.
  - `code/rl-cli/src/tui/flow_ops.rs`: `/flow draft`.
  - `code/rl-cli/src/tui/flow_new_tests.rs` and the tests in `new_flow.rs`: migrate.
  - `code/rl-cli/src/tui/palette.rs`: the `/flow draft` copy, `bring back the flow proposal`.

**Interfaces:**
- Consumes: Task 1's `Want::NewFlow`, `interpret::changing_flow`, `flow::write_changed`, `flow::start_changed`, `flow::restore` and `diff::lines`.
- Produces, for Task 3:
  - `pub enum Aim { New { named: Option<String>, folder: Option<PathBuf> }, Change { name: String, base: String } }`
  - `pub struct FlowProposal { pub aim: Aim, pub asked: Vec<String>, pub draft: Option<Drafted>, pub since: Option<Instant>, pub error: Option<String>, pub shown: bool }`
  - `App::propose_flow(&mut self, aim: Aim, words: String)`: opens the pane and asks.
  - `App::flow_proposal_shown(&self) -> Option<&FlowProposal>`
  - `App::refine_flow(&mut self, words: String)`
  - `App::save_flow_proposal(&mut self)` (async)
  - `App::edit_flow_proposal(&mut self)`
  - `App::leave_flow_proposal(&mut self)`

- [ ] **Step 1: Write the failing tests in `flow_proposal_tests.rs`.** Use the `panel` harness, and hand answers in through `app.answered(...)` as `proposal_tests.rs` does. Age the draft past `SETTLE` by setting `drawn` back.
  - `asking_for_a_new_flow_shows_the_wait`: `propose_flow(Aim::New{named:None, folder:Some(ws)}, "say hello hourly")`. The drawn pane contains `asked    say hello hourly` and `asking the assistant`, and `asking.want == Want::NewFlow{folder:Some(ws)}`.
  - `a_new_flow_answered_shows_its_text_and_saves`:
    1. Answer `kind: flow\nname: hello\n---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n`.
    2. The pane shows ` hello  drafted by fakeai` and the text.
    3. Draw, age it, then `enter` on an empty line.
    4. `hello` is loaded and approved, the pane is gone, and the flash is `hello approved`.
  - `more_words_refine_it`: after an answer, `type_in("only weekdays")` then `enter`.
    - `asked` has two entries.
    - The sent words contain `Its flow.md now:`.
    - The pane shows `changing it`.
  - `a_change_shows_a_diff_and_saves`:
    1. A flow `hello` exists and is approved.
    2. `propose_flow(Aim::Change{name:"hello", base:<its flow.md>}, "every 2h")`, then answer with `every 2h` in place of `every 1h`, with `name: other`.
    3. The pane shows ` - when: every 1h` and ` + when: every 2h`.
    4. `enter` saves it, the file says `every 2h`, and no flow `other` exists.
  - `a_change_keeps_its_flows_name` is covered by the `name: other` above. Assert it explicitly.
  - `a_proposal_that_would_not_load_is_not_saved`: answer with `when: whenever`. `enter` flashes `it would not load: …`, nothing is written, and the pane stays.
  - `esc_while_asking_leaves_it_asking` flashes `still asking · /flow draft shows it`, and `/flow draft` shows it again.
  - `esc_once_answered_drops_it` flashes `dropped it · nothing written`, and `flow_proposal` is `None`.
  - `an_answer_after_esc_waits_unshown`:
    - esc while asking, then answer;
    - the pane is not drawn and the flash is `fakeai proposed hello · /flow draft shows it`;
    - an `enter` on the list does not save it.
  - `a_run_where_a_flow_was_asked_is_said_in_the_pane`: answer a `kind: run`. The pane shows `✗ asked for a flow; fakeai answered a run (run run00009)` and stays up.
  - `flow_new_with_words_lands_in_the_pane`: `/flow new brief what my team did` gives `Aim::New{named:Some("brief"),..}` and `Want::Flow{name:"brief"}`. Whatever name the answer gives, the person's `brief` wins.
  - `a_flow_answered_to_plain_words_moves_into_the_flow_pane`: `propose("…")` then a `kind: flow` answer.
    - `proposal` becomes `None`;
    - `flow_proposal` is `Aim::New{named:None, folder:Some(<the run proposal's folder>)}`, with the asked words carried and shown.
  - `ctrl_e_opens_the_proposal_in_the_editor`:
    - For a new flow, `app.editor` is set with `yours` (as `write_draft(.., true)` did).
    - For a change, `app.editor` holds `hello`, and `flow.md` holds the proposed text.
  - Migrate every existing `Mode::Draft`/`Parked`/`show_draft` test in `flow_new_tests.rs` and `new_flow.rs` to the pane. Keep their assertions on settle, seen-end, scroll and the escapes `shown()` writes. Keep the pure `Drafted` tests as they are.

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p rl-cli flow_proposal` and expect compile errors.

- [ ] **Step 3: Implement.**
  - **`propose_flow`:**
    - If `self.asking.is_some()`, say `still asking; /stop on its run` and do nothing.
    - Otherwise:
      1. Set `flow_proposal = FlowProposal{aim, asked:[words], since:now, shown:true, ..}`.
      2. Set `show_pane = true`, `pane = Pane::Facts`, `term = None`.
      3. Spawn the ask (factor the spawn out of `summary_ops::ask` into `fn spawn_ask(&mut self, sent: String, folder: PathBuf, want: Want, words: String)`). Use the want, folder and sent words below.

    | aim | want | folder | sent |
    |---|---|---|---|
    | `New{named:Some(n),folder}` | `Want::Flow{name:n}` | `folder` else `folder_here()` | words |
    | `New{named:None,folder}` | `Want::NewFlow{folder}` | `folder` else `folder_here()` | words |
    | `Change{name,..}` | `Want::Flow{name}` | the flow's `in` (from `self.flows`) else `folder_here()` | `changing_flow(&asked, &base)` |
  - **`refine_flow`** uses the same table, with `sent = changing_flow(&asked, <the current draft's text, else base, else no prior text>)`. With no draft and no base, it is the plain words again.
  - **`answered`:**
    - A `Flow`/`NewFlow` want's answer goes to the flow proposal:
      - `Answer::Flow(d)` builds a `Drafted`. The name is the change's name, else `named`, else `d.name`. The `taken` check is skipped for a change. A change gets `.with_base(base)`.
      - `Answer::Run` sets the error copy from Review Focus 3.
      - An `Err` sets `error`.
      - In every case it clears `since`.
      - If the pane is not shown, it flashes as in Review Focus 2. For an error, it flashes the error.
    - A `Want::Any` answer that is a flow moves the run proposal into a flow proposal: `Aim::New{named:None, folder:Some(p.folder)}`, `asked` carried, `shown` carried.
  - **Drawing (`flow_proposal.rs`, `FlowProposal::lines`):**
    1. The `asked`/`then` facts, as `proposal.rs` draws them.
    2. The wait (Global Constraints).
    3. The error (`✗`).
    4. Then the draft's lines: its header and problems as `Drafted::lines` gives them. For a change, the body is `diff::lines(base, text)`, drawn as ` - …` in `theme::bad()`, ` + …` in `theme::accent()` and `   …` in `theme::dim()`. For a new flow it is the text.
    5. Every line goes through `new_flow::shown`; make it `pub(super)`.

    Keep `Drafted`'s scroll, seen and settle working over these lines: move its `draw` to take a `Vec<Line>` from the caller, or give `Drafted` an optional base and let `Drafted::lines` draw the diff. The second is preferred, since one draw path counts the rows.

    The footer is `Drafted::hint()`, with its `e` copy changed to `^e`. While asking, it is `esc leaves it asking`. The title is `flow proposal`.
  - **`save_flow_proposal`:**
    - Refused while asking (`still asking; a moment`), while settling (silently), while not ready (as `draft_key`), and while unseen (`the end is below · ↓ to read it`).
    - New: `flow::write_new` as `write_draft(.., false)` did.
    - Change: `flow::write_changed(rpc, home, name, base, &d.text, &known)`. The flash is `Edited::words(name)`. An `Err` stays in the pane as `error`.
    - On success:
      - drop the pane;
      - `refresh()`;
      - in the overview, select the saved flow's key (`rows::row_key` of its `Row::Flow`, as `show_flow` does);
      - flash last.
  - **`edit_flow_proposal`:** New uses `start_new(.., yours=true)`; Change uses `start_changed`. Then set `self.editor = Some((editing, 1))` and drop the pane. Where the editor's failure is handled (`flow_ops::edited`'s caller, or the editor-run site in `mod.rs`), a change whose editor failed to open calls `flow::restore`. Carry the base in a new `App.editing_base: Option<String>`, set here and taken there.
  - **`leave_flow_proposal`:** while asking, `shown = false` and the flash from Review Focus 2's copy (`still asking · /flow draft shows it`). Otherwise, drop it with `dropped it · nothing written`.
  - **Routing:**
    - `esc` with the flow proposal shown leaves it, before any other subpage's esc.
    - With the flow proposal shown, `line_what` treats a Shell/Agent first word as words, as it does for the run proposal.
    - `What::Ask(words)` with the flow proposal shown calls `refine_flow`, as `propose` does for runs.
    - `What::Empty` with it shown calls `save_flow_proposal`.
    - `^e` with an empty line calls `edit_flow_proposal`.
    - `↑↓ pgup pgdn` scroll the draft.
  - **`/flow draft`** shows the flow proposal (sets `shown`, `show_pane`), else says `no flow proposal · type on + new flow in a project, or /flow new <name> <words>`.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the three gates.

- [ ] **Step 5: Commit:** `Flows proposed in the pane: one place for a new flow and a changed one, refined with words, saved with enter, opened with ^e; the draft mode goes`.

### Task 3: Words in the overview

**Files:**
- Modify:
  - `code/rl-cli/src/tui/keys.rs` (`submit`, `line_what`, the `NEW_FLOW` enter);
  - `code/rl-cli/src/tui/overview.rs` (`fn overview_words`);
  - `code/rl-cli/src/tui/mod.rs` (the line's hint at ~2413, the footer);
  - `code/rl-cli/src/tui/shortcuts.rs` (`Place::Overview` rows);
  - `docs/ARCHITECTURE.md`.
- Test: `code/rl-cli/src/tui/overview_tests.rs`.

**Interfaces:**
- Consumes: Task 2's `Aim`, `propose_flow`, `flow_proposal_shown`.
- Produces: `App::overview_words(&mut self, words: String) -> bool`. It is true when the words were taken as a flow's change or a new flow.

- [ ] **Step 1: Write the failing tests in `overview_tests.rs`.**
  - `words_on_a_flow_change_it`: open a project's overview on flow `hello`, `type_in("every 2h")`, then `enter`.
    - The flow proposal is `Aim::Change{name:"hello", base:<its flow.md>}`.
    - The want is `Want::Flow{name:"hello"}`.
    - The line is clear.
  - `words_on_new_flow_make_one_in_the_project`: select `+ new flow`, type words, then `enter`. The result is `Aim::New{named:None, folder:Some(<project path>)}` and `Want::NewFlow{folder:Some(..)}`.
  - `words_on_new_flow_everywhere_have_no_folder`: `Aim::New{folder:None}`.
  - `a_runnable_first_word_is_still_words`:
    - put `ls` (or another name `complete::runnable` accepts in the harness) first: `ls only on weekdays` on a flow is a change, not a shell run;
    - `!ls` runs as a shell line.
  - `a_question_line_still_answers`: with a flow's question row selected, `1 ok` answers it and makes no proposal (the existing `answer_of` path comes first).
  - `enter_on_new_flow_says_to_type`: an empty `enter` on `+ new flow` flashes `type what the new flow should do, then enter`, and the line stays empty. This replaces PR 2's `/flow new ` on the line; update that test.
  - `the_line_hint_says_what_words_do`: with `hello` selected and words typed, the hint under the line says ` changes hello`. On `+ new flow` it says ` makes a new flow`.
  - `a_change_proposal_does_not_move_on_cursor_moves`: with the pane up, `↑↓` scroll it and do not move the overview's selection.

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**
  - **`overview_words`**, when `view == Overview` and the flow proposal is not shown:
    - On a `Row::Flow(f)`: read `home.flow_dir(&f.name)/flow.md`. If it can't be read, say `{name}'s flow.md can't be read: {e}` and return true. Otherwise call `propose_flow(Aim::Change{name, base}, words)`.
    - On `NEW_FLOW`: `propose_flow(Aim::New{named:None, folder: scope.folder().cloned()}, words)`.
    - Otherwise false.
  - **`submit`:** in the `What::Ask(words)` arm, before `self.ask(words, Want::Any)`, clear the line and `return` if `overview_words(words)` took them. The asking-busy check stays first.
  - **`line_what`:** in the overview, on a flow or `+ new flow`, a Shell/Agent `what` without a leading `!` becomes `What::Ask`, the same rule as the proposal's. Factor the rule into one predicate, `fn words_only(&self) -> bool`, true when a proposal of either kind is shown or the overview cursor is on a flow or `+ new flow`.
  - **The `NEW_FLOW` enter** flashes the copy above.
  - **The line hint** (`mod.rs` ~2413): `What::Ask(_)` in the overview shows ` changes {name}` on a flow and ` makes a new flow` on `+ new flow`. With the flow proposal shown, it shows ` changes the proposal`.
  - **Shortcuts (`Place::Overview`):** `type` changes the flow, or makes one on `+ new flow`. With the flow proposal up: `enter` saves · `^e` opens in $EDITOR · `esc` drops.
  - **`docs/ARCHITECTURE.md`:** where the overview is described, one paragraph on words, the flow proposal pane, and `/flow draft`. Remove any mention of the draft mode.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the three gates.

- [ ] **Step 5: Commit:** `In the overview, words on a flow change it and words on + new flow make one, proposed in the pane`.
