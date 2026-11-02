# Project overview, PR 4: parking flows, and the summary written on open

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- `^x` on a flow in the overview parks it. Its folder becomes `~/.relay/flows/_<name>`, its runs are archived, and it stops firing.
- Parked flows can be listed from the overview and unparked.
- Opening a project's overview writes its summary by itself when the kept one is missing or out of date.

**Architecture:**
- **Parking is the daemon's work,** done in one go by three new `flow.*` methods: `flow.park`, `flow.unpark` and `flow.parked`. The TUI never touches `~/.relay/flows`.
  - Park, in order:
    1. refuse while the flow is firing;
    2. rename the folder;
    3. archive the flow's ended runs (cause prefix `flow:<name>/`);
    4. clear the flow's yes in the ledger;
    5. reload.
  - The loader already skips `_` folders, so the reload marks the flow removed.
  - Unpark renames the folder back and reloads. The yes was cleared at park, so the flow needs a yes again.
- **The overview** ends with a dim `parked: N` line. `enter` on it folds out the parked flows as dim rows, and `enter` on one of those unparks it.
- **The summary written on open** uses the catch-up top's rule. It is asked when there is no kept summary for the stretch, or the kept one is stale. It runs only with `catch_up.summary: auto` and a named summary assistant. It runs on a task of its own, so the flow list never waits for it.

**Tech Stack:** Rust. The crates touched are `rl-proto` (`flow.rs`), `rl-ledger` (`flows.rs`, `runs.rs`), `rl-daemon` (`flows/rpc.rs`, `flows/store.rs`), and `rl-cli` (the TUI in `code/rl-cli/src/tui`).

**Spec:** `docs/superpowers/specs/2026-11-01-project-overview.md`, on branch `project-overview-design` (PR #193). The relevant sections are "Parking a flow" and "The overview" item 1 (the summary at the top).

## Global Constraints

- **Gates** before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`, all clean.
- **No backward compatibility,** per the repo memory: a strict protocol, no fallbacks.
- **Parked folder names:** `_<name>` the first time. While that is taken, `_<name>.2`, then `_<name>.3`, and so on.
  - This is a ruling against the spec's suggested `-2`. A flow name may contain `-`, so `_foo-2` could be `foo` parked twice or `foo-2` parked once. A `.` cannot appear in a flow name, so the original name is everything between the leading `_` and the first `.`.
  - Nothing is ever overwritten.
- **Copy:** lower case and short, with ` · ` between clauses.
  - Park flash: `{name} parked · its runs archived · /flow unpark {name} brings it back`
  - Refused while firing: `{name} is firing · /stop its run, then park it`
  - Unpark flash: `{name} back · it needs a yes: enter on it`
  - Unpark refused when the name is taken: `a flow named {name} is there now · park or rename it first`
  - Overview line: `parked: {n}` (dim), and `parked: {n} ▾` while open.
  - A parked row: `{name}` plus dim `  parked {ago}`, where `{ago}` is formatted as the rest of the TUI does.
- **`^x` is armed then fired,** like any row's remove (the existing arming). Only the fire parks.

## Review Focus

1. **Parking a flow while it is firing:** refused, with nothing renamed and nothing archived. Pinned in Task 1 (`park_refuses_while_firing`).
2. **Parking the same name twice** (park `foo`, write a new `foo`, park again) gives `_foo` and `_foo.2`, with both folders intact. Unparking `_foo.2` while `foo` exists is refused. Pinned in Task 1.
3. **Run-archive prefix collisions:** parking `nightly` must not archive `nightly-2`'s runs. Pinned in Task 1 (`park_archives_only_its_own_runs`).
4. **An unparked flow comes back with no yes,** even if its folder is byte-identical to the one approved. The ledger's "has its yes back if its folder is the one said yes to" must not apply. Pinned in Task 1.
5. **The summary on open must not write again and again.** Opening, leaving and reopening while one is being written must not start a second. A fresh, non-stale kept summary must start none. Pinned in Task 3.

---

### Task 1: Parking in the daemon

**Files:**
- Modify: `code/rl-proto/src/flow.rs` (the methods and types)
- Modify: `code/rl-ledger/src/flows.rs` (`unapprove`) and `code/rl-ledger/src/runs.rs` (`archive_flow_runs`)
- Modify: `code/rl-daemon/src/flows/rpc.rs` (handlers) and `code/rl-daemon/src/flows/store.rs`, or a new `code/rl-daemon/src/flows/park.rs` (the rename and parked-list logic)
- Test: in-file ledger tests, plus `code/rl-daemon/tests/flow_park.rs`, using `tests/common` as `flow_state.rs` does.

**Interfaces:**
- Produces:
  - `pub struct FlowPark; impl Method { NAME = "flow.park"; Params = FlowName; Result = Parked }`
  - `pub struct FlowUnpark; impl Method { NAME = "flow.unpark"; Params = ParkedName; Result = Flow }`
  - `pub struct FlowParkedList; impl Method { NAME = "flow.parked"; Params = (); Result = Vec<Parked> }`
  - `pub struct ParkedName { pub folder: String }`
  - `pub struct Parked { pub name: String, pub folder: String, pub at: Timestamp, pub archived: usize, pub in_: Option<PathBuf> }`
    - `at` is the folder's mtime as of the rename.
    - `archived` is the run count, only on park. It is 0 in the list.
    - `in_` is read from `flow.md`'s frontmatter, best effort, and is `None` when unreadable. Serialize it as `in`, as `FlowSpec` does.
  - `rl_ledger::flows::unapprove(conn, name) -> Result<()>`
  - `rl_ledger::runs::archive_flow_runs(conn, flow: &str, at) -> Result<usize>`. It archives only runs that have ended and are not already archived, whose `cause` starts with `flow:{flow}/`. Compare with `substr`, not `LIKE`.
  - `pub fn rl_proto::flow::parked_name(folder: &str) -> Option<&str>`. It returns the name from `_name` or `_name.N`, only when that name is `valid_name`.

- [ ] **Step 1: Write the failing tests.**

  **Ledger:**
  - `archive_flow_runs_takes_only_its_flows_ended_runs`: given runs with causes `flow:nightly/a` (ended), `flow:nightly/b` (running), `flow:nightly-2/a` (ended) and `flow:nightly/c` (already archived), it returns 1, and only the first run is archived.
  - `unapprove_clears_the_yes`.

  **Proto:** `parked_name`:
  - `_foo` → `foo`, `_foo.2` → `foo`, `_foo-bar.12` → `foo-bar`;
  - `foo`, `_`, `_Foo` and `__foo` → `None`.

  **Daemon, `tests/flow_park.rs`:**
  - `park_renames_archives_and_removes`:
    1. Approve and run flow `hello` (`run: echo hi`) to its end.
    2. Call `flow.park`. The result is `{name:"hello", folder:"_hello", archived:1}`.
    3. `flows/_hello/flow.md` exists and `flows/hello` doesn't.
    4. `flow.list` has no `hello`.
    5. The run is archived.
    6. `flow.parked` lists `_hello`.
  - `park_twice_takes_the_next_folder`: `_hello`, then `_hello.2`, both kept.
  - `park_refuses_while_firing`:
    - use a step that sleeps;
    - while it fires, `flow.park` errors with `hello is firing · /stop its run, then park it`;
    - the folder is unchanged.
  - `park_archives_only_its_own_runs` (Review Focus 3).
  - `unpark_brings_it_back_needing_a_yes`:
    - after park then `flow.unpark {folder:"_hello"}`, `flows/hello` exists, `flow.list` has `hello`, and `approved()` is false (Review Focus 4);
    - `flow.parked` is empty.
  - `unpark_refuses_a_taken_name` uses the copy in Global Constraints.
  - `park_unknown_flow_errors`.
  - `parked_lists_in_from_the_frontmatter`: a flow with `in: <a registered workspace>`, parked, lists that path as `in`.

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**
  - **Firing check:** use whatever the engine uses for overlap. `fire` returns `Ok(None)` when it is still firing, so find the predicate behind that.
  - **Park:**
    1. Rename with `std::fs::rename` within `flows_dir`.
    2. Archive the runs, then `unapprove`.
    3. Reload with `d.flows.reload()`.
    4. If the rename fails, nothing else happens and the error is returned.
    5. If a later step fails, the folder stays parked and the error says so. Do not rename back.
  - **Unpark:**
    1. `parked_name(folder)`, else invalid params `{folder} is not a parked flow`.
    2. The target `flow_dir(name)` must not exist (the copy in Global Constraints).
    3. Rename, reload, and return `get(name)`.
  - **Parked list:** read `flows_dir` entries that start with `_` and that `parked_name` accepts, sorted by name then by `at`.
  - Register the methods.
  - `docs/ARCHITECTURE.md`, where the flow methods are listed: add the three methods, and say that `_` folders are parked.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the gates.

- [ ] **Step 5: Commit:** `Flows can be parked: flow.park renames the folder to _<name>, archives its runs and takes its yes; flow.unpark brings it back; flow.parked lists them`.

### Task 2: Parking from the overview

**Files:**
- Modify:
  - `code/rl-cli/src/tui/act.rs` (`remove_selected` on a flow in the overview parks it)
  - `code/rl-cli/src/tui/overview.rs` (the parked line and rows, fetching the list)
  - `code/rl-cli/src/tui/keys.rs` (`enter` on those rows)
  - `code/rl-cli/src/tui/select.rs` (keys and selection for the new rows)
  - `code/rl-cli/src/tui/flow_ops.rs` (`/flow park [name]`, `/flow unpark <name|folder>`)
  - `code/rl-cli/src/tui/palette.rs`, `code/rl-cli/src/tui/shortcuts.rs`, `docs/ARCHITECTURE.md`
  - the CLI, `code/rl-cli/src/main.rs` and its flow subcommands: `rl flow park <name>`, `rl flow unpark <name|folder>` and `rl flow parked`, each printing one line per result as its neighbours do, with `--json` honoured
- Test: `code/rl-cli/src/tui/overview_tests.rs`, and the CLI tests where the flow subcommands are tested (`code/rl-cli/tests/flow.rs`).

**Interfaces:**
- Consumes: Task 1's `FlowPark`, `FlowUnpark`, `FlowParkedList`, `Parked`, `ParkedName` and `parked_name`.
- Produces:
  - `const PARKED: &str = "+parked"`, the key of the `parked: N` line;
  - parked rows keyed `parked:<folder>`;
  - `App.parked: Vec<Parked>` and `App.parked_open: bool`;
  - `App::park_flow(&mut self, name: &str)` (async);
  - `App::unpark(&mut self, folder: &str)` (async).

- [ ] **Step 1: Write the failing tests.**
  - `ctrl_x_twice_on_a_flow_parks_it`, in a project's overview on `hello`:
    1. `^x` arms.
    2. `^x` again parks.
    3. The flash is `hello parked · its runs archived · /flow unpark hello brings it back`.
    4. `hello` is gone from the overview's rows.
    5. The line `parked: 1` is there.
  - `the_parked_line_counts_only_this_scopes_flows`: one parked flow with `in` = this project and one with another `in` gives `parked: 1`. Everywhere counts the one with no `in`.
  - `no_parked_line_with_none_parked`.
  - `enter_on_parked_folds_them_out_and_in`:
    - `enter` shows `parked: 1 ▾` and a row `hello  parked …`;
    - `enter` again hides it.
  - `enter_on_a_parked_flow_unparks_it`:
    - the flash is `hello back · it needs a yes: enter on it`;
    - `hello` is a flow row again, selected, and needs a yes;
    - the parked line is gone.
  - `flow_park_and_unpark_commands`:
    - `/flow park` on the selected flow parks it, and `/flow park hello` parks by name;
    - `/flow unpark hello` unparks the newest `_hello*` folder; `/flow unpark _hello.2` unparks that one.
  - `ctrl_x_in_the_catch_up_on_a_flow_still_says_where`: the existing `FLOW_ON_THE_LIST` behaviour, unchanged. Update its copy to say `^x in its project's overview parks it` if it currently says archive.
  - `slash_archive_on_a_flow_still_archives_its_runs`: `/archive` on a flow in the overview archives its runs without parking it.
  - **CLI:**
    - `rl flow park hello` prints `hello parked as _hello · 1 run archived`;
    - `rl flow parked` lists `_hello  hello  <ago>`;
    - `rl flow unpark hello` prints `hello back · it needs a yes: rl flow approve hello`.

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**
  - **Fetching the parked list:** fetch `App.parked` in `open_overview`, after a park or an unpark, and on the overview's refresh. It must not be fetched on every list refresh.
  - **The `parked: N` line** comes last, after `+ new flow`, only when N > 0 for the scope. Parked rows go under it while it is open. Neither row takes typed words (`words_only` false). `^x` on them says `already parked`.
  - **After an unpark,** select the flow's row (`rows::row_key`). If the flow's `in` is another project, flash where it went, as PR 3's save does.
  - **`/flow unpark <name>`** with a bare name picks the newest `_name` or `_name.N` folder by `at`.
  - **Palette `/flow` text:**
    - add `park` and `unpark`;
    - say that `_` folders are parked;
    - update the usage line `/flow run|pause|resume|approve|edit|new|test|draft|park|unpark|reload [name]`.
  - **Shortcuts, `Place::Overview`:** `^x parks a flow`.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the gates.

- [ ] **Step 5: Commit:** `^x in the overview parks a flow; parked: N lists them, enter brings one back; /flow park, /flow unpark and rl flow park|unpark|parked`.

### Task 3: The overview's summary, written on open

**Files:**
- Modify:
  - `code/rl-cli/src/tui/overview.rs` (`fetch_overview_top` becomes plan-then-maybe-write)
  - `code/rl-cli/src/tui/mod.rs` (a new `overviewing: Option<Summarising>` field, and its arm in the main loop's `select!` beside `topping`)
  - `code/rl-cli/src/catchup.rs` (generalise `plan_top`, or add a sibling `plan_project_top`)
- Test: `code/rl-cli/src/tui/overview_tests.rs` and `catchup.rs` unit tests.

**Interfaces:**
- Consumes: `SummaryGet`, `SummaryMake`, `ConfigGet`, and `catchup::named_summariser` / `auto_summary`.
- Produces:
  - `pub async fn catchup::plan_project_top(rpc: &mut Client, workspace: &Path, from: Timestamp) -> TopPlan`
  - `App.overviewing: Option<Summarising>`

- [ ] **Step 1: Write the failing tests.** Use the `auto_top` tests' config fixtures and the harness's fake summariser, as the `topping` tests in `mod.rs` (~6800-7150) do.
  - `opening_writes_the_summary_when_none_is_kept`:
    - with auto on and a named summariser, opening a project's overview sets `overviewing`, and the top shows `summarising…`;
    - when it lands, the top shows its text and `overview_said` holds it.
  - `a_fresh_kept_summary_is_shown_not_rewritten`: kept and not stale, so no `overviewing`.
  - `a_stale_kept_summary_is_shown_while_a_new_one_is_written`: the old text stays under the `summarising…` line, as PR 2 shows a `/summary` in progress.
  - `no_auto_no_write`: `catch_up.summary` not auto, or no named summariser, means no `overviewing`. The PR 2 text ` no summary yet · /summary writes it` stays.
  - `reopening_while_writing_starts_no_second`: open, esc, open again, and the same task is still running (Review Focus 5).
  - `a_summary_landing_after_leaving_is_kept_not_shown`: it goes into `made`, and the next open shows it without asking.
  - `everywhere_writes_nothing`: everywhere's top stays as PR 2 left it.
  - `catchup.rs`: `plan_project_top` asks with `target = Project{workspace}`, `deep = false` and `from` as given.

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**
  - **The stretch:** `from` is `self.away_since`, else 24h before now. Use a named constant `OVERVIEW_STRETCH`.
  - **`plan_project_top`:**
    1. Call `SummaryGet`.
    2. A kept summary that is not stale is shown, and nothing is asked.
    3. Otherwise, ask only if `ConfigGet` says `auto_summary() && named_summariser(..)`. This is the same rule as `auto_top`, without the time-away part: opening the overview is the person asking to look.
  - **Writing:** if it asks, spawn `SummaryMake` as `write_top` does into `overviewing`, with `title` set to the project's name.
    - Skip it if `overviewing` is already for the same workspace.
    - If it is for another workspace, abort it.
  - **`overview_top_lines`:** show `summarising… {secs}s` while `overviewing` (or `summarising`) is for this project.
  - **Landing:** insert into `made`. If the overview is still on that project, set `overview_said`. An error flashes `the summary: {e}`, the same form as `topped`.
  - `/summary` while `overviewing` runs for the same project says `still summarising {project}`.

- [ ] **Step 4: Run the tests and confirm they pass,** plus the gates.

- [ ] **Step 5: Commit:** `Opening a project's overview writes its summary when none kept is current, by the named summariser and only with summary: auto`.
