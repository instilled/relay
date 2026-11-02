# Overview cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the leftovers from the project overview work (#194-#197): nine small defects in the TUI overview, the proposals, parking, reloads, outside sessions and one flaky test.

**Architecture:** Four independent tasks grouped by file overlap: daemon parking and reload (with the `flow.unpark` result made structured), a daemon test fix plus outside-session pruning, the TUI overview's own state, and TUI keys (esc settle guard, `^x` on outside rows).

**Tech Stack:** Rust workspace in `code/` (rl-cli, rl-daemon, rl-proto, rl-ledger), tokio.

**Spec:** `docs/superpowers/specs/2026-11-01-project-overview.md` (on branch project-overview-design, PR #193). Each item below says what is wrong, the fix and its test; paths are relative to `code/`.

## Global Constraints

- No backward compatibility: daemon and CLI ship together; change proto shapes outright.
- Copy style: lower case, ` · ` separators, as the surrounding messages.
- Gates before each commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at least the touched crates per task; the whole workspace at the end).
- Test helpers: `tui/tests.rs` (`panel`, `panel_with_fake_summariser`, `press`, `type_in`, `draw_text`), `tui/overview_tests.rs` (`enter_project`, `md_in`, `approve`, `parked`, `said_of`, `wait_overviewed`), `tui/flow_tests.rs` (`flow_in`, `fired`), `tui/flow_proposal_tests.rs` (`proposed`, `ask_for`, `answer_md`, `on_screen`), `rl-daemon/tests/common` (`Harness`, `Conn`, `approve`, `kill_holder`).
- Line numbers below are from main d80e0b9 and are hints; find the code by name.

## Review Focus

- Leaving the overview gives the list its search back, also via a catch-up or Events opened over it.
- An esc during a proposal's wait never drops an answer that landed a moment before; a deliberate esc later still drops it.
- A reload started before a park's rename never re-lists the parked flow.
- `rl flow unpark` and the TUI report a folder that came back but did not load, without parsing error text.
- `^x` never offers DELETE for a folder that is not a registered project.

---

### Task 1: Daemon parking and reload (items 4, 5, 7)

Item 7 changes `FlowUnpark`'s result, so this task also updates the CLI unpark (`rl-cli/src/flow.rs`) and the TUI unpark (`tui/flow_ops.rs`) to keep the workspace compiling. Commit per item or once; tests listed under each item must pass.

#### 4. Park refusal says "waiting on a question" for a lost step

**Real?** Yes.
- `park.rs:64-72` maps "every open firing Waiting" to `Unmoved::Waiting`.
- `FiringStatus::Waiting` is documented as "On a question, or a lost step" (`rl-proto/src/flow.rs:298`).
- `Unmoved::Waiting`'s Display (`park.rs:35`) always says "is waiting on a question · answer or /stop it".

The cases can be told apart: `firings::open` → `query` → `with_steps` (`rl-ledger/src/firings.rs:212,230-239`) fills
`Firing.steps`. A lost step has `StepStatus::Lost` (`rl-proto/src/flow.rs:283-284`, "its run was lost while rld was
down; resuming picks it up"), and a question has `StepStatus::Waiting` + `decision`. The TUI already distinguishes
them in `tui/flows.rs:134-137` (`waiting · a lost` vs `waiting at review`), with the question taking precedence.

**Fix** (park.rs):
- Add `Unmoved::Lost { name: String, step: String }`. Display:
  `"{name} is waiting on its lost step {step} · /resume its run or /stop it, then park it"`.
  `/resume` / `rl resume <id>` is how a lost step's run is picked up (`run.resume`, see `restart.rs::resumed`).
- In `park`, when `waiting`: if any open firing has a step with `StepStatus::Waiting`, return `Waiting` (the question
  takes precedence, as in the TUI). Otherwise, if one has `StepStatus::Lost`, return
  `Unmoved::Lost { name, step: <first lost step's name> }`. Otherwise return `Waiting`.
- `rpc.rs::unmoved` already maps every non-Missing variant to `invalid_params(u.to_string())`, so it needs no change.
- Update the doc on `Unmoved::Waiting` to "on a question".

**Test.** New `rl-daemon/tests/flow_park.rs::park_refuses_while_a_step_is_lost`. Follow
`flow_fire.rs:362 a_lost_step_waits_and_resume_picks_it_up_in_the_same_firing`, which is `#[cfg(unix)]`. Steps:
- Use md `a: run: sleep 30`, approve, run, and `wait_firing` until step a has a run.
- `h.stop()`, then `common::kill_holder(&home, &old, true)`, then restart `Harness::on(home)`.
- `wait_firing` until step a is `StepStatus::Lost`.
- `refused(flow.park)` must end with
  `"hello is waiting on its lost step a · /resume its run or /stop it, then park it"`, and the folder must not be
  renamed.

The existing `park_refuses_while_waiting_on_a_question` (flow_park.rs:150-166) stays as is.

**Risk.** Low. It is one new enum arm, and only the message changes.

#### 5. Flow reloads are not serialized

**Real?** Yes.
- `Flows::reload` (`rl-daemon/src/flows/store.rs:29-102`) takes no lock. In order, it reads `workspaces::list` and
  `flows::list`, reads `folders(&dir)` from disk, then `flows::sync`s, then writes `broken`.
- Its callers:
  - startup: `server.rs:166`
  - `flow.reload`: `flows/rpc.rs:32-36`
  - `workspace up` / `remove`: `workspace.rs:79,106`
  - `flow.test`: `flows/preview.rs:53`
  - `changed_since_yes`: `store.rs:129`, called from `why_not_run` paths under `going`
  - `park` / `unpark`: `park.rs:89,117`
- `park` holds `going` (`park.rs:48`) but reload does not take it. The interleaving:
  1. Reload A (say `flow.reload` or `rl up`) reads `folders()` and still sees `hello/`.
  2. Park renames it to `_hello`, and its own reload B syncs, so hello is marked removed.
  3. A's stale `sync` then lands and writes hello as loaded again (now with no yes).
- Nothing watches the directory (no notify/watcher in `flows/`), so it stays listed **until the next reload**. That
  can be longer than "briefly". `broken` (a std Mutex) is also last-writer-wins, with the same staleness.

**Fix.** Add a field to `Flows` (`flows/mod.rs:44`, init at :83):
`pub(crate) reloading: tokio::sync::Mutex<()>`, documented as "Held for a whole reload, read to sync, so a reload
that read the folders before a rename can never sync after one that read them since." Take it as the first line of
`reload()`: `let _reading = self.reloading.lock().await;`.
- Do **not** reuse `going`. `park`, `unpark` and `fire`→`changed_since_yes` already hold `going` when they reload,
  and tokio's Mutex is not reentrant, so that would deadlock.
- The lock order is always `going → reloading`, and nothing takes `going` inside reload, so there is no cycle.
- tokio's Mutex is FIFO, so park's reload (started after the rename) syncs last among any reload that read the
  folders before the rename.

**Test.** New `rl-daemon/tests/flow_park.rs::a_reload_racing_a_park_never_leaves_it_listed`.
- Spawn a task with its own `Conn::open(&h.home)` that loops `flow.reload` until a `CancellationToken` or
  `AtomicBool` stops it.
- Meanwhile, 25 times: `flow.park hello`, assert `listed()` has no `hello`, then `flow.unpark _hello`.

Without the lock it fails intermittently. With it, it passes every time: after park returns, every reload that
syncs later started reading after the rename.

Optionally, add an in-crate unit test that holds `flows.reloading` and asserts that a spawned `reload()` has not
finished after a `yield_now`. That pins that the lock is taken.

**Risk.** Low. Reloads queue behind one another; they are short (disk read plus one sync). Keep the lock to reload
only, not around `park`.

#### 7. Unpark's "did not load" detection parses daemon text

**Real?** Yes. The daemon reports "back but not loaded" as an **error**:
- `rpc.rs:90-99` returns `Err(missing(&d, &name))` when `back.removed_at.is_some()`.
- `missing` (`rpc.rs:155-160`) formats ``"`{name}` did not load: {NotLoaded}"``.

Both clients parse that text back:
- `rl-cli/src/flow.rs:198-207` calls `came_back_unloaded(&folder, &now, &format!("{e:#}"))`, a second RPC to
  `flow.parked`, and `came_back_unloaded` (`flow.rs:252-262`) does `e.split_once(" did not load: ")`.
- The TUI does the same in `tui/flow_ops.rs:145-155`.

**Fix.** Make the result structured. There is no back-compat to keep (CLI and daemon ship together; strict proto).
- rl-proto/src/flow.rs:
  ```rust
  /// A parked flow back under its name: loaded, or why not.
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct Unparked {
      /// The name it came back as.
      pub name: String,
      /// The flow as loaded; none when it came back and did not load.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub flow: Option<Flow>,
      /// Why it did not load, with its file and line, as the reload said.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub problem: Option<String>,
  }
  ```
  and `impl Method for FlowUnpark { type Result = Unparked; }` (flow.rs:620-625).
- Daemon `rpc.rs:90`: after `unpark`, look the flow up with `flows::get` directly, not with `get()`, which errors.
  - Loaded and `removed_at.is_none()`: return `Ok(Unparked { name, flow: Some(shown), problem: None })`.
  - Otherwise: `problem = d.flows.not_loaded().into_iter().find(|n| n.flow == name).map(|n| n.to_string()).or(Some("not loaded".into()))`
    and return `Ok(Unparked { name, flow: None, problem })`. The other refusals (`Unmoved`) stay errors.
- CLI `flow.rs:198-213`: match `Unparked { flow: Some(f), .. }` and print as now. On `problem: Some(why)`, bail with
  the same copy, `"{name} is back in flows/{name} but did not load: {why} · rl flow edit {name}, or rl flow reload"`.
  Delete `came_back_unloaded` and its tests, and drop the extra `FlowParkedList` call.
- TUI `flow_ops.rs:130-157`: on `Ok(Unparked { flow: None, problem: Some(why), name })`, refresh, then say the
  existing copy `"{name} is back in flows/{name} but did not load: {why} · ^e on it, or /flow reload"`. On
  `flow: Some`, keep the current path. On `Err`, use `told(&e)`. The `--json` output now prints `Unparked`.

**Test.**
- `rl-daemon/tests/flow_park.rs`: new `unpark_of_a_folder_that_does_not_load_says_why_in_its_answer`. Park hello,
  write `"---\nsteps: [\n---\n"` into `_hello/flow.md`, then
  `let u: Unparked = c.ok("flow.unpark", {folder:"_hello"})`. Assert `u.flow.is_none()`,
  `u.problem.unwrap().contains("flow.md:")`, and that `hello/flow.md` is a file.
- Update `unpark_brings_it_back_needing_a_yes` (:230) and the end of
  `park_reads_the_flows_again_when_the_ledger_fails_after_the_rename` (:200) to read `Unparked.flow.unwrap()`.
- The existing TUI test `overview_tests.rs:1599 an_unpark_whose_folder_came_back_but_did_not_load_says_so_and_reads_again`
  should pass unchanged.
- Check `rl-cli/tests/flow.rs:492 flow_park_lists_and_unparks` too. Add a case for a broken unpark if it is cheap.

**Risk.** Medium-low. It is a proto shape change, touching rl-proto, rl-daemon, the rl-cli CLI and the TUI.
Grep `FlowUnpark` for every caller.

- [ ] Write the tests named above; see them fail where they can.
- [ ] Implement the fixes.
- [ ] Run the touched crates' tests, fmt and clippy; commit.

### Task 2: Flaky flow_state test and outside sessions in a folder gone (items 8, 9a)

#### 8. `a_running_step_sets_gets_writes_and_reads_through_rld` flake

**Where.** `rl-daemon/tests/flow_state.rs:201-290`, in the tail at :281-283:
```rust
std::fs::write(dir.join("go"), "").unwrap();
wait_firing(&mut c, "st", waiting).await;
let q = the_question(&mut c).await;   // asserts decision.list has exactly 1
```
`waiting` (`flow_state.rs:52`) is only `f.status == FiringStatus::Waiting`.

**Race (real, deterministic cause).** The engine saves the firing as Waiting **before** it raises the question:
- `engine.rs:465` runs `firings::save(...)`, and only then does `Do::Ask` (`engine.rs:290-292`) call `ask()`.
- `ask()` → `decisions.raise_for_step`, then `set_step_decision` (`flows/ask.rs:27-41`).
- `flow_fire.rs:411` (`a_question_never_raised_is_raised_at_a_restart`) even simulates the gap.

So the poll can see Waiting while `decision.list` is still empty, and `the_question`'s `assert_eq!(open.len(), 1)`
then fails. The same predicate is used by `a_probes_values_reach_the_if_the_question_and_the_next_command` (:84/:95)
and `nothing_set_skips_the_if_and_leaves_the_question_ungrouped` (:114/:117), so they can flake the same way.
`flow_ask.rs:48-54` already has the fixed predicate.

**Fix.** Make `flow_state.rs::waiting` match `flow_ask.rs`:
```rust
/// Waiting on a question: the step waiting has it.
fn waiting(f: &Firing) -> bool {
    f.status == FiringStatus::Waiting
        && f.steps.iter().any(|s| s.status == StepStatus::Waiting && s.decision.is_some())
}
```
The decision row is written (`raise_for_step`) before `set_step_decision`, so once the step has its id,
`decision.list` has it. One more fragility: `wait_firing`'s 10s deadline under a loaded `cargo test --workspace`. It
is shared with every flow test. Leave it unless it recurs.

**Test.** The fix is in the test itself. To validate, run
`cargo test -p rl-daemon --test flow_state -- --test-threads=1` in a loop, e.g. 50×.

**Risk.** None, since it is test-only.

#### 9. An outside session in a deleted folder cannot be removed

#### 9a. `hooked::prune` keeps a session whose cwd is gone
**Real?** Yes. `Hooked::prune(now, alive)` (`rl-daemon/src/outside/hooked.rs:336-351`) drops a session only when its
pid is dead, or after QUIET (48h, :41) or STALE (12h, :39). For a Codex desktop-app hook, the pid is the long-lived
app, so a session in a deleted folder stands for up to 48h. It is called from `Outsiders::prune_told`
(`outside/mod.rs:219-224`).

**Fix.**
- New signature: `pub fn prune(&self, now: Instant, alive: impl Fn(u32) -> bool, here: impl Fn(&Path) -> bool) -> Vec<String>`.
- Inside it, `let stands = here(&h.session.cwd) && match h.session.pid { … };`.
- Update the doc: "…or its folder gone".
- `prune_told` passes `|p| p.is_dir()`.
- Note that sessions only a lister lists (not hooked) are untouched. A lister that still lists one will show it
  again, which is acceptable and out of scope.

**Test.** In `outside/mod.rs` tests, next to `a_told_session_goes_with_its_process_or_after_long_quiet` (:853), add
`a_told_session_in_a_folder_gone_goes`. Use `tell` with `cwd: tmp.path().join("gone")` (create the dir, tell with
pid = `std::process::id()`, then remove the dir), plus one with cwd `/`. Call `store.prune_told()` and assert only
the `/` one remains.

- [ ] Write the tests named above; see them fail where they can.
- [ ] Implement the fixes.
- [ ] Run the touched crates' tests, fmt and clippy; commit.

### Task 3: TUI overview state (items 1, 2, 6)

#### 1. A list search carries into the overview

**Real?** Yes. The overview *is meant* to be searchable (doc on `overview_rows`, "the search narrows them as it does
the list"; `flow_tests.rs:506` searches inside it). The bug is that it **inherits** the list's search. There is one
`filter: String` on App (`tui/mod.rs:371`). `switch_to` (`tui/mod.rs:1990`) swaps it with `filter_aside` only when
going in or out of **Events**, so List→Overview keeps it. `overview_rows` (`tui/overview.rs:251`) builds with
`filter: &self.filter`. `open_overview` (`tui/overview.rs:67`) then picks `keys.first()` from the filtered rows, and
can fall back to `+ new flow` with every flow hidden. The first `esc` in the overview only clears the search
(`tui/keys.rs:710-715`), and a second `esc` leaves, which drops the list's search too. `show_flow`
(`tui/flow_ops.rs:21-25`) works around this ("the search lets go, to show {name}").

**Fix.** Set the list's search aside while the overview is up, and give it back when the overview is left.
- New App field (mod.rs, near `filter_aside`): `list_search: Option<String>` with the doc comment "The list's search
  while the overview is up: it opens unsearched, and leaving gives it back."
- `open_overview`: before `open_view`, when `self.view != View::Overview`, run
  `self.list_search = Some(std::mem::take(&mut self.filter)); self.search = None;`.
- Add a small helper `fn overview_closed(&mut self)` (in overview.rs). It runs `self.overview = None;` and then
  `if let Some(f) = self.list_search.take() { self.filter = f; }`. Call it from `leave_overview` (overview.rs:232,
  replacing `self.overview = None`, before `go_back`, then `settle()` as now) and from `back_to_list`
  (mod.rs:2002). In `back_to_list`, call it **after** `switch_to(View::Work)`, so the Events swap has already run.
- `show_flow` (flow_ops.rs:21-25): delete the "search lets go" branch, which is dead code after this change.
  `back_to_list` gives the list's search back, and `open_overview` sets it aside again.
- Decision: leaving **restores** the list's search. This matches `kept` holding each view's selection, and
  `leave_overview`'s `settle()` already handles a header that the search now hides.

**Test.**
- New `overview_tests.rs::a_list_search_stays_on_the_list` does the following. `flow_in` alpha; set
  `app.filter = "zzzz"`; call `app.select(Some(line_key))`; then call `app.open_overview(Scope::Project(folder), None)`
  directly, because enter cannot reach a line the search hides. Assert `app.filter.is_empty()`,
  `app.keys() == ["f:alpha", NEW_FLOW]` and `sel == "f:alpha"`. Then `press Esc` once. Assert the view is
  `View::Work` (one esc, not two) and `app.filter == "zzzz"`.
- Update `mod.rs:6432-6438`: drop the `said == "the search lets go, to show nightly"` assertion and assert that
  `filter` is empty in the overview.
- Update `overview_tests.rs:264-269`: keep the empty-filter and sel assertions. Optionally add: esc gives back "zzzz".

**Risk.** Low. Watch the Events-over-Overview swap. Overview→Events→Overview swaps the overview's own search
through `filter_aside`. That still works because `list_search` is a separate slot. A catch-up opened over the
overview leaves through `back_to_list`, which restores the search, and that is correct.

#### 2. `unseen_from` not recorded when the overview opens on a flow

**What it is.** `unseen_from: HashMap<String, Timestamp>` (`tui/mod.rs:518`) maps a flow row's key to the start
of its oldest **unseen** run. It is noted when the cursor lands on the row (`saw_selected`, `tui/select.rs:248-267`,
insert at :264), or for every flow a catch-up showed (`saw_catch_up`, `tui/catch_ops.rs:140`). It is noted because
landing marks those runs seen, and after that the daemon's default (`over_from` → `runs::oldest_unseen`,
`rl-daemon/src/summary.rs:629-641`) could no longer find where they began. A flow's `/summary` with no stretch reads
it at `summary_ops.rs:173-175`, and so does the pane's kept summary at `mod.rs:1807-1809`. It is used once:
`summary_ops.rs:218-223` removes it. Otherwise it is only ever **overwritten** by a later landing.

**Real?** Yes, as a stale value rather than a missing one. `enter` on a project line (`keys.rs:816-826`)
deliberately skips `moved()`, so opening does not mark the flow seen. That behaviour is pinned by
`overview_tests.rs:418 opening_the_overview_does_not_mark_its_first_flow_seen`. Because nothing is noted either,
an **older** entry for `f:alpha` stays in the map. #194 took flow rows off the list, so such an entry now comes
mostly from a catch-up's `saw_catch_up`. Example: a catch-up showed alpha's failures at T1, alpha fails again at
T2, then you open the overview on alpha. `/summary` and the pane's kept summary then use **T1** instead of T2.
With no stale entry the daemon's default still gives T2. (`show_flow` calls `moved()`, so it notes and marks seen.
That path is fine.)

**Correct behavior.** When the overview's cursor lands on a flow, note where its unseen runs begin (overwriting
any older note), but do **not** mark them seen.

**Fix.** Split the noting out of `saw_selected` (select.rs):
```rust
/// Where the flow under the cursor's unseen runs begin, noted for its `/summary`: the cursor
/// on it may mark them seen, after which the daemon could not tell. Their ids.
pub(super) fn note_unseen_from(&mut self) -> Vec<String>
```
It returns the unseen ids for `Row::Flow`, plus the run id for an unseen `Row::Run`, and does the `unseen_from`
insert itself. `saw_selected` becomes `let ids = self.note_unseen_from(); self.seen(ids).await;`.
In `open_overview` (overview.rs:79), after `self.select(Some(at))`, call `let _ = self.note_unseen_from();`. That
notes without marking seen. Fix the doc on `unseen_from` to read: "noted as the cursor lands on it, the overview's
opening too".

**Test.** New `overview_tests.rs::opening_the_overview_on_a_flow_notes_where_its_unseen_began`. Copy the setup of
`opening_the_overview_does_not_mark_its_first_flow_seen` (`fired` alpha Failed). Then pre-seed a stale
`app.unseen_from.insert("f:alpha".into(), Timestamp::now() - 10d)` and call `enter_project`. Assert that
`app.unseen_from["f:alpha"]` equals the started_at of alpha's run in `app.digest.items`, and that `needs(&app)` is
still true (not marked seen).

**Risk.** Very low.

#### 6. `/summary 3d` shifts the overview's stretch (shared `made`)

**Real?** Yes. `made: HashMap<String, Summary>` (`tui/mod.rs:470-473`) is written by:
- every `/summary` that lands: `summarised`, `summary_ops.rs:281`
- the catch-up's top: `topped`, `catch_ops.rs:234`
- the overview's own top: `overviewed`, `overview.rs:155`, unless the kept one is deep

`overview_stretch` (`overview.rs:135-141`) takes its `from` from `made[key].from`, falling back to `away_since`, then
`now - OVERVIEW_STRETCH` (`catchup.rs:549`, 24h). So after `/summary 3d` on the project line, `made[project].from`
is now−3d, and `write_overview_top` (`overview.rs:96-130`) plans and asks over 3 days. The summary the overview then
writes has `from` = now−3d and goes back into `made`, so the overview stays on 3 days for the rest of the session.

**Fix.** Give the overview its own note of its stretch:
- New App field `overview_from: HashMap<String, Timestamp>`, documented as "Where the overview's own top of each
  project started, by `SummaryTarget::key`, so it is found again; `/summary`'s stretch is not its."
- `overview_stretch`: `self.overview_from.get(&target.key()).copied().or(self.away_since).unwrap_or_else(|| now - OVERVIEW_STRETCH)`.
- `overviewed` (Ok arm): `if let Some(f) = summary.from { self.overview_from.insert(key.clone(), f); }`.
- Leave the `made` insert and the display order in `overview_top_lines` (overview.rs:372-410) alone. A `/summary`
  made of the project still shows at the top: that is pinned by `a_project_summary_made_shows_at_the_overviews_top`,
  overview_tests.rs:349. Only the stretch changes.

**Test.** New `overview_tests.rs::a_summary_over_another_stretch_leaves_the_overviews_alone`.
- Use `panel_with_fake_summariser`. Set `away = now - 1h` and `app.away_since = Some(away)`.
- Insert `app.made[project] = said_of(&target, "- three days")` with `.from = Some(now - 3d)`.
- `enter_project`, then assert `app.overviewing.as_ref().unwrap().from == Some(away)`. This is the same pattern as
  `a_stale_kept_summary_is_shown_while_a_new_one_is_written`, overview_tests.rs:1329-1330.

**Risk.** Low. The existing tests at :1281-1335 and :1550-1600 should still pass. Check
`a_deep_summary_made_of_the_project_is_not_replaced_by_the_brief`.

- [ ] Write the tests named above; see them fail where they can.
- [ ] Implement the fixes.
- [ ] Run the touched crates' tests, fmt and clippy; commit.

### Task 4: TUI keys: esc settle guard and ^x on rows that are not projects (items 3, 9b, 9c)

#### 3. esc right after a proposal's answer lands has no settle guard

**Real?** Yes, for **both** proposals.
- Enter's guards:
  - The flow proposal: `save_flow_proposal` (`tui/flow_proposal.rs:361`) checks `d.settling(now)`.
    `Drafted.drawn` is set at first draw. `SETTLE = 300ms` is at `tui/new_flow.rs:82`, `settling` at :125.
  - The run proposal: `go_proposal` (`tui/proposal.rs:232`) checks `p.settling(now)`, which reads the draft's
    landing `Instant` (`proposal.rs:48-52`, `SETTLE` at :14).
- Esc has no guard:
  - `key_subpage` sends `KeyCode::Esc` straight to `leave_subpage` (`tui/keys.rs:508`).
  - `leave_subpage` (`mod.rs:1847`) calls `leave_flow_proposal` (`flow_proposal.rs:327`) or `leave_proposal`
    (`proposal.rs:205`).
  - Once `since` is None, both **drop** the proposal: "dropped it · nothing written" / "dropped it". So an esc typed
    to leave a proposal still asking, arriving just after the answer lands, throws the answer away.
- An *error* answer has no draft Instant at all: `flow_answered` (`flow_proposal.rs:229-238`) sets only `p.error`,
  and `not_proposed` (`proposal.rs:175-181`) does the same. So the guard needs its own timestamp.

**Fix.**
- Make `proposal::SETTLE` `pub(super)` and reuse it. Do not add a third const.
- Add a field `pub landed: Option<Instant>` ("when the last answer came, whatever it was") to `FlowProposal`
  (flow_proposal.rs:45) and to `Proposal` (proposal.rs:20).
  - Set it in `flow_answered` (next to `p.since = None`).
  - Set it in `proposed` and in `not_proposed`.
- Add `pub fn just_landed(&self, now: Instant) -> bool { self.landed.is_some_and(|at| now.duration_since(at) < SETTLE) }`
  to both structs.
- Add an App helper in proposal.rs:
  ```rust
  /// An esc sooner than SETTLE after the shown proposal's answer came was typed for the wait.
  pub(super) fn proposal_just_landed(&self) -> bool
  ```
  It checks `flow_proposal_shown()` / `proposal_shown()` against `Instant::now()`.
- In `key_subpage` (keys.rs:508), add `(KeyCode::Esc, _) if self.proposal_just_landed() => {}` before the
  `leave_subpage` arm. Say nothing, as enter's guard says nothing. Leave `ctrl-\` unguarded: it is a deliberate
  "out".

**Test.**
- New `flow_proposal_tests.rs::an_esc_just_as_the_answer_lands_is_let_be`. Call `proposed(&mut app, "hello", HELLO)`,
  then `press Esc`. Assert `app.flow_proposal.is_some()`, `app.show_pane`, and that `said` is not "dropped it…".
  Then backdate `landed` by 1s and `press Esc`, and assert it is dropped.
- Add the same test to `proposal_tests.rs` for the run proposal (`an_esc_just_as_the_run_is_proposed_is_let_be`).
- Extend the `on_screen` helper (flow_proposal_tests.rs:82) to backdate `landed` as well. Also call a settle step in
  `esc_once_answered_drops_it` (:409) and in `proposal_tests.rs:95 esc_leaves_it_asking_and_ask_brings_it_back_then_esc_drops_it`,
  or they will fail.

**Risk.** Low. Existing esc tests need backdating; grep `Esc` in `flow_proposal_tests.rs`, `proposal_tests.rs` and
`overview_tests.rs` (words_on_* tests).

#### 9b. `^x` on an outside row, or on a header that is not a project, should hide it
**Real?** Yes.
- `remove_hint` (`tui/act.rs:215-236`) returns None for `Row::Outside`, and `remove_selected` (`act.rs:133-151`)
  has no arm for it, so `^x` says "nothing here to remove" (`keys.rs:701-703`).
- For a `Row::Group` that is **not registered** (e.g. the session's deleted cwd), `keys.rs:690-693` sends it to
  `confirm_remove_project`: a typed DELETE that reaches `WorkspaceRemove` on a folder that was never a project.

**Fix.**
- Add a helper (act.rs): `fn hides_on_x(&self) -> Option<String>`. It returns `Some(title)` for `Row::Outside(c)`
  (`c.title()`), and for `Row::Group(h)` when `!h.is_everywhere() && self.names.registered(&h.workspace).is_none()`
  (`rows/name.rs:31`).
- `keys.rs:690` guard: `matches!(self.selected(), Some(Row::Group(h)) if !h.is_everywhere() && self.names.registered(&h.workspace).is_some())`.
- `remove_hint`: put `else if let Some(t) = self.hides_on_x() { Some(format!("hide {t}")) }` before the Group arm.
- `remove_selected`: put `else if self.hides_on_x().is_some() { self.hide().await }` before the Group arm.
  `hide` (`act.rs:345`) does the keyed `/hide`.
- Guard against toggling it back: when `self.show_hidden` and the key is already hidden, say
  `"already hidden · /hide brings it back"` instead.

**Test.** In `outside_tests.rs`, use the `sessions()` fixture (:13). It already has an `odd` session in
`/nowhere/scratch`, which is an unregistered header.
- `ctrl_x_twice_hides_an_outside_session`: select `o:claude:sess-bg`, press `^x`. The flash reads
  "press ^x again to hide tidy the docs". Press `^x` again and assert `digest.hidden` has the key, as in
  `hide_takes_a_claude_session_off_the_list` (:125).
- `ctrl_x_on_a_header_that_is_no_project_hides_it`: select `g:/nowhere/scratch`, press `^x` twice. Assert
  `app.mode` is not `Mode::Confirm` and that `digest.hidden` has `g:/nowhere/scratch`.

#### 9c. `remove::done` says "removed X" when the daemon says `removed: false`
**Real?** Yes. `remove::done` (`tui/remove.rs:59-77`) always starts with `"removed {name}"`.
`WorkspaceRemoved.removed` (`rl-proto/src/methods.rs:995-996`, "It was registered, and no longer is") is ignored.
The CLI gets it right in `main.rs:770-775` (`"… was not registered"`).

**Fix.** The first word becomes
`if r.removed { format!("removed {name}") } else { format!("{name} was not a project") }`. When `!r.removed` and
every count is 0, append `"nothing removed"`.

**Test.** `remove.rs::tests::the_result_says_only_what_happened` (:126-156) currently asserts
`"removed relay · stopped 1 · 1 still going"` and `"removed relay"` for `removed: false`. Change those to
`"relay was not a project · stopped 1 · 1 still going"` and `"relay was not a project · nothing removed"`, and set
`removed: true` where "removed relay" is meant.

**Risk (9 overall).** Low. 9b changes what `^x` does on two row kinds; check the shortcut and hint text in
`mod.rs:3413` (`/hide` hint on outside rows) and `shortcuts.rs`.

- [ ] Write the tests named above; see them fail where they can.
- [ ] Implement the fixes.
- [ ] Run the touched crates' tests, fmt and clippy; commit.
