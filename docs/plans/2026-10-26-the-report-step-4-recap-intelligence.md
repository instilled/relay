# The report, step 4 — recap intelligence · Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One recap engine (`summary.rs`) that writes every run's recap from its session plus its facts, for Claude, Codex and Gemini, at a depth set by how long you were away, with honest token counts; the report reads it.

**Architecture:**
- **Daemon.** The daemon's readers count tokens once per message as fresh / cached / out, and learn Gemini. The recap sweep's free agent recap moves to its own `recap_seeds` table and becomes a seed. `summary.rs` always asks the assistant, feeding it the tail, the run's `run_facts` and the seed. A run's brief is three labelled parts, parsed by a shared `rl_proto::brief::Brief`. Many-run summaries brief each run first, then join.
- **Client.** The client derives a depth from the time away (`depth.rs`). On opening, the report writes the happened runs' recaps in the background behind a count-first guard. The board and the prose render them, falling back down the ladder.
- **Retired.** Step 3's `FactsStructure` path and the soft `RunFacts` fields go.

**Tech Stack:** Rust workspace (`code/`): tokio, rusqlite, serde, ratatui, jiff, clap.

**Spec:** `docs/specs/2026-10-25-the-report-step-4-recap-intelligence.md` (on `chore/roadmap`). Read it before any task.

## Global Constraints

- **No backward compat.** `PROTOCOL_VERSION` 8 → 9 (`code/rl-proto/src/lib.rs:22`), bumped once in Task 1. After merge, kill the running `rld`/`rl`.
- **Migrations are append-only.** The next one is `0030`, registered in `code/rl-ledger/src/migrate.rs` `MIGRATIONS`.
- **Gates before every commit and push:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings` (a warning is a CI error)
  - `cargo test --workspace`
- **Test fixtures are synthetic.** Never copy real session content.
- **Depth thresholds:** brief past **2h**, deep past **2 days**. Config keys `report.brief_after` (default `"2h"`), `report.deep_after` (default `"2d"`) and `report.summary` (`auto | ask`, default `ask`).
- **Fan-out guard:** ask first when a rendering would make **more than 3** assistant calls (`ASK_OVER = 3`), unless `report.summary: auto`.
- **Brief labels**, exactly: `Worked on:`, `Open questions:`, `Next:`. A part whose text is `none` is absent.
- **Token mapping:**

  | Agent | fresh | cached | out |
  |---|---|---|---|
  | Claude | `input_tokens + cache_creation_input_tokens` | `cache_read_input_tokens` | `output_tokens` |
  | Codex (`last_token_usage`) | `input_tokens − cached_input_tokens` | `cached_input_tokens` | `output_tokens` |
  | Gemini (`tokens`) | `input − cached` | `cached` | `output` |

  Claude dedupes by `/message/id` and Gemini by `id`; the **last** record per id wins.
- **Time away** in the pane is `now − App.left` (fixed at launch; `away_since` moves, so it is not the measure).
- **Commit messages** end with `Claude-Session: https://claude.ai/code/session_01MAdNa6AXB7uzyBtwg2uMYK`.

## Review Focus

1. **A Claude message streamed in many entries, each carrying the same usage**, is counted once. Pinned in Task 1.
2. **A run with no session file** (a shell, an unknown `cli`) still gets a brief from its output tail, with no facts section and no panic. Pinned in Task 4.
3. **A seed picked up after a written recap** never replaces it, and is never handed back as the kept brief. Pinned in Task 3.
4. **The report's refresh tick** (it refetches every few seconds while open) never restarts the briefing or asks twice. Pinned in Task 8.
5. **No named summariser**: opening the report after 3h makes no call and shows no error, and the rows keep their ladder line. Pinned in Task 8.

---

### Task 1: Honest token counts

**Files:**
- Modify: `code/rl-daemon/src/transcript.rs`
  - `Usage` at :158 and `usage()` at :167.
  - Tests in the same file.
- Modify: `code/rl-proto/src/facts.rs` (`RunFacts`): add `tokens_cached`.
- Modify: `code/rl-proto/src/lib.rs:22`: `PROTOCOL_VERSION = 9`.
- Modify: `code/rl-daemon/src/facts.rs:40-42`: fill `tokens_cached`.
- Modify: `code/rl-cli/src/tui/pane.rs:163-165`: cost line, plus its test (~:824).

**Interfaces:**
- Produces:
  - `pub struct Usage { pub tokens_in: u64, pub tokens_cached: u64, pub tokens_out: u64 }`. Here `tokens_in` means *fresh*.
  - `RunFacts.tokens_cached: Option<u64>` (`#[serde(default)]`).
- `usage(body: &str, since: Option<Timestamp>) -> Usage` keeps its signature.

- [ ] **Step 1: Write the failing tests** in `transcript.rs`:

```rust
#[test]
fn usage_counts_a_repeated_claude_message_once() {
    // three entries, one message id "m1", same usage on each; one more message "m2"
    let body = [
        r#"{"timestamp":"2026-10-01T10:00:00Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":7}}}"#,
        r#"{"timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":7}}}"#,
        r#"{"timestamp":"2026-10-01T10:00:02Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":9}}}"#,
        r#"{"timestamp":"2026-10-01T10:01:00Z","message":{"id":"m2","usage":{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":1015,"output_tokens":2}}}"#,
    ].join("\n");
    let u = usage(&body, None);
    assert_eq!((u.tokens_in, u.tokens_cached, u.tokens_out), (15 + 3, 1000 + 1015, 9 + 2));
}

#[test]
fn codex_cached_input_is_not_fresh() {
    let body = r#"{"timestamp":"2026-10-01T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":9841,"cached_input_tokens":4736,"output_tokens":71}}}}"#;
    let u = usage(body, None);
    assert_eq!((u.tokens_in, u.tokens_cached, u.tokens_out), (9841 - 4736, 4736, 71));
}
```

Also update the existing Codex usage test (~:680) to assert `tokens_cached == 0`.

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test -p rl-daemon transcript::tests::usage_counts_a_repeated_claude_message_once transcript::tests::codex_cached_input_is_not_fresh`
  - Expected: FAIL. `tokens_cached` doesn't exist yet, so this is a compile error.

- [ ] **Step 3: Implement.**
  - **`usage`:** collect Claude usages into a map keyed by `/message/id`, last one wins. An entry with no id counts on its own. Then sum the map using the Global Constraints mapping. For Codex, read `cached_input_tokens`, default 0, and use `saturating_sub` for fresh.
  - **`facts::collect`:** sets `tokens_cached: Some(u.tokens_cached)` alongside the other two.
  - **`RunFacts`:** add `pub tokens_cached: Option<u64>`.
  - **Pane cost line:** reads `{fresh} in · {cached} cached · {out} out tokens`. Drop the `cached` part when it is `None` or 0.
  - Bump `PROTOCOL_VERSION` to 9.

- [ ] **Step 4: Run** `cargo test -p rl-daemon transcript` and `cargo test -p rl-cli pane`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `fix(facts): count tokens once per message, as fresh / cached / out`

---

### Task 2: Gemini, read

**Files:**
- Modify: `code/rl-daemon/src/transcript.rs`
  - `Agent` (:21) and `Agent::of` (:27)
  - `session_id` (:121)
  - `usage` (Gemini branch)
  - new `find_gemini`, `gemini_messages` and `gemini_story`
  - tests
- Modify: `code/rl-daemon/src/summary.rs`
  - `locate_in` match (:253–256): Gemini arm calls `find_gemini`
  - `run_brief` (:442) and `run_deep` (:507): material arms
- Modify: `code/rl-daemon/src/recap.rs:50,66`: a Gemini run has no recap word, so `pick_up` returns `Ok(false)` for it.

**Interfaces:**
- Produces:
  - `Agent::Gemini`. `Agent::of` maps `run.cli == Some("gemini")` to it; the detect profile is named `gemini` (`rl-detect/src/registry.rs:25`).
  - `pub fn find_gemini(home: &Path, cwd: &Path, started: Timestamp) -> Option<PathBuf>`:
    - Reads `home/.gemini/projects.json` `{"projects": {"<cwd>": "<name>"}}`.
    - Then takes the newest `home/.gemini/tmp/<name>/chats/session-*.jsonl` modified at or after `started − SLACK`.
    - Hashed folder names from older installs: **out**.
  - `pub fn gemini_messages(text: &str, from: Option<Timestamp>) -> String` returns the `user`/`gemini` records' `content` in order, **one per `id`, the last record of each**, each prefixed `you:` / `gemini:`.
  - `pub fn gemini_story(text: &str, from: Option<Timestamp>) -> String` is `gemini_messages` plus each record's `toolCalls` names and args, one line each.
  - `session_id(Agent::Gemini, p)` takes the header record's `sessionId`.
  - `usage` reads records with `"type":"gemini"` and a `tokens` object. It dedupes by `id`, last wins, and maps per Global Constraints.

- [ ] **Step 1: Write the failing tests.** Use synthetic fixtures. Two `gemini` records share id `g1` with tokens `{input:100,cached:40,output:5}` then `{input:120,cached:40,output:9}`, and record `g2` has `{input:50,cached:0,output:2}`.
  - `gemini_usage_keeps_the_last_record_per_message`: `(tokens_in, tokens_cached, tokens_out) == (80 + 50, 40, 9 + 2)`.
  - `gemini_messages_say_each_message_once`: `g1` appears once, with its last `content`.
  - `find_gemini_reads_the_project_map`: a tempdir with `projects.json` mapping the cwd to `relay`, and two session files of different mtimes. It returns the newer one.
  - `find_gemini_without_a_project_map_is_none`: a missing `projects.json`, or a cwd absent from it, gives `None`, with no panic.
  - `agent_of_knows_gemini`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon transcript`. Expected: FAIL, because nothing is defined yet.

- [ ] **Step 3: Implement** the interfaces above.
  - Extend every exhaustive `match agent`: `summary.rs:253, 442, 507`, `recap.rs:50, 66`, `transcript.rs:121`.
  - In `run_brief`, Gemini material is `gemini_messages`, headed "What was said". In `run_deep` it is `gemini_story`.
  - `entries()` already needs a top-level `timestamp`, and Gemini records have one, so `active_span` works unchanged. The header record has none and is skipped.

- [ ] **Step 4: Run** `cargo test -p rl-daemon`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(transcript): read Gemini sessions — tokens, active time, what was said`

---

### Task 3: The seed gets its own table

**Files:**
- Create: `code/rl-ledger/migrations/0030_recap_seeds.sql`, and register it in `migrate.rs`.
- Create: `code/rl-ledger/src/seeds.rs`, and export it in `lib.rs`.
- Modify: `code/rl-ledger/src/summaries.rs`
  - `lines` (:62) falls back to seeds.
  - `first_line` (:85) is label-aware.
- Modify: `code/rl-daemon/src/recap.rs`
  - `pick_up` (:33–105) writes seeds.
  - Delete `merge_recap` (:112) and its test (:143), and the `facts::set` call (:93–99).
- Modify: `code/rl-daemon/src/summary.rs`
  - `get` (:75): a run with no written recap returns its seed for display.
  - `run_kept` (:193) and `run_latest` (:211) are unchanged; they read `summaries` only.
- Create: `code/rl-proto/src/brief.rs` (`Brief`), and export it in `lib.rs`.

**Interfaces:**
- Produces:
  - **Migration `0030`:** `CREATE TABLE recap_seeds (run TEXT PRIMARY KEY, agent TEXT NOT NULL, at TEXT NOT NULL, text TEXT NOT NULL, source TEXT NOT NULL, reach TEXT NOT NULL);` It then moves existing seeds:
    - `INSERT INTO recap_seeds SELECT run, agent, at, text, source, reach FROM summaries WHERE by_run = '';`
    - `DELETE FROM summaries WHERE by_run = '';`
    - Match the real column names in `0011`/`0013`.
  - `rl_ledger::seeds::put_newer(conn: &Connection, s: &Summary) -> Result<bool>` (newer `at` wins) and `rl_ledger::seeds::get(conn: &Connection, run: &str) -> Result<Option<Summary>>` (returns `by` empty and `calls` 0).
  - `rl_proto::brief::Brief { pub worked_on: Option<String>, pub questions: Option<String>, pub next: Option<String> }`, with `impl Brief { pub fn parse(text: &str) -> Brief; pub fn is_empty(&self) -> bool }`.
    - It takes over `facts::split_structured`'s rules: labels `Worked on:` / `Open questions:` / `Next:`, tolerant of `**…**` and `##`, and `none` → `None`.
    - Move the three `split_tests` here as `Brief` tests. `facts.rs` keeps `split_structured` until Task 6.
- `summaries::lines` is unchanged in signature and returns, per run:
  - the written recap's line if there is one, else the seed's;
  - for each, when `Brief::parse(text).worked_on` is `Some`, that part without its label, else the first non-blank line.

- [ ] **Step 1: Write the failing tests.**
  - `rl-ledger/tests/ledger.rs`, `a_later_seed_never_replaces_a_written_recap`:
    1. `summaries::put` a recap with `by = "r9"` and `at = T`.
    2. `seeds::put_newer` a seed at `T + 1m`.
    3. Assert that `summaries::get` still has the written text, and `lines` gives the written line.
  - `rl-ledger/tests/ledger.rs`, `lines_fall_back_to_the_seed_and_strip_the_label`:
    - A run with only a seed gives the seed's first line.
    - A run whose written text is `"Worked on: the parser\nOpen questions: none\nNext: tests"` gives `"the parser"`.
  - `migrate` idempotency test: extend it to `0030`.
  - `rl-proto` `brief` tests: the three moved cases.
  - `rl-daemon` `recap.rs`, `a_picked_up_recap_is_a_seed_not_a_summary`, using the existing pick-up test harness:
    - after `pick_up`, `seeds::get` is `Some`, `summaries::get` is `None`, and `run_facts` is untouched.

- [ ] **Step 2: Run** `cargo test -p rl-ledger -p rl-proto -p rl-daemon recap`. Expected: FAIL.

- [ ] **Step 3: Implement** the interfaces.
  - `pick_up` builds the same `Summary` it builds today (:70–85) and writes it with `seeds::put_newer`; `set_read_to` is unchanged.
  - `summary::get` for `SummaryTarget::Run` with nothing written returns `seeds::get`.
  - Keep `brief`/`deep` kept-checks on `summaries`/`summary_kept` only. A seed must never satisfy `run_brief`'s reach check (:435).

- [ ] **Step 4: Run** `cargo test --workspace`. Expected: PASS. Fix any test that relied on the sweep writing `summaries`; point it at `seeds`.

- [ ] **Step 5: Run the gates and commit.** `feat(recap): the agent's own recap is a seed, kept apart from written ones`

---

### Task 4: Every brief is written, from the tail, the facts and the seed

**Files:**
- Modify: `code/rl-daemon/src/summary.rs`
  - `prompt` (:992) and its brief instruction (:1015–1021)
  - `run_brief` (:427–486)
  - `run_deep` prompt calls (:535, :544)
- Modify: `code/rl-daemon/tests/summary.rs`
  - Rewrite the Claude-recap-as-is test (:182–240).
  - Add new tests.

**Interfaces:**
- Consumes:
  - `rl_ledger::facts::get(conn, run) -> Result<Option<RunFacts>>`
  - `rl_ledger::seeds::get` (Task 3)
  - `Brief` (Task 3)
- Produces:
  - `pub fn prompt(run: &Run, from: Option<Timestamp>, events: Option<&[Event]>, facts: Option<&RunFacts>, seed: Option<&str>, heading: &str, material: &str, deep: bool) -> String`.
    - After `## The run`, when `facts` is `Some`, add a `## The facts` section listing:
      - the commits (from the `commit.landed` events already fetched, else the count);
      - `+added/−removed` across `files` files, and the top paths;
      - errors, branch (pushed or not), active time, and tokens fresh/cached/out.
    - When `seed` is `Some`, add `## Its own recap` with the seed text, marked as the agent's own words to check against the rest.
  - The **brief instruction** asks for exactly three labelled parts, `Worked on:`, `Open questions:`, `Next:`, each one or two sentences, `none` when empty, for the person coming back, grounded in the facts.
  - `run_brief` **always asks**:
    - Remove the Claude-recap-as-is return (:443–450) and the Codex no-assistant shortcut (:454–460).
    - `source` is `"claude"`, `"codex"`, `"gemini"` or `"output"` by material.
    - The seed is `seeds::get(run)` when its `at` is within the stretch.
    - Facts are `facts::get(run)`, ended runs only.
  - `run_deep` passes facts and seed to the single-part prompt, and to part 0 when in parts.

- [ ] **Step 1: Write the failing tests** in `rl-daemon/tests/summary.rs`, using the existing `fake_agent()` harness, `FAKE_AGENT_PROMPT` and `FAKE_AGENT_ANSWER`.
  - **`a_claude_brief_is_written_with_its_recap_as_a_seed`** (replaces :182–240). Set up a Claude transcript with an `away_summary` "Search fixed; nothing committed." and a `run_facts` row `{commits:3, added:180, removed:40, files:4, top_paths:["auth/"]}`. Assert:
    - the captured prompt contains `## The facts`, `+180/−40`, `## Its own recap` and `Search fixed`;
    - the returned `Summary.by` is not empty, and `source == "claude"`;
    - `Brief::parse(&s.text).worked_on` is `Some` (the fake answer is a three-part text).
  - **`a_shell_run_is_briefed_from_its_output`**: a `sh -c` run with output and no session file. The prompt has no `## The facts` (it has no `run_facts`) and no `## Its own recap`, and the call succeeds.
  - **`the_brief_asks_for_three_labelled_parts`**: a pure unit test in `summary.rs` that `prompt(..., deep: false)` contains `Worked on:`, `Open questions:` and `Next:`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon --test summary`. Expected: the new tests FAIL.

- [ ] **Step 3: Implement** the interfaces. Fetch the facts and the seed inside the existing ledger read alongside events; no I/O inside `call` closures beyond rusqlite.

- [ ] **Step 4: Run** `cargo test -p rl-daemon`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(summary): every brief is written — the tail, the run's facts, its own recap as a seed`

---

### Task 5: Many runs: brief each, then join; each run on its own stretch

**Files:**
- Modify: `code/rl-daemon/src/summary.rs`
  - `brief_lines` (:641)
  - `over` (:667–745), including the per-run stretch closure at :685
- Modify: `code/rl-daemon/tests/summary.rs`

**Interfaces:**
- `over`'s brief path:
  1. For each run in the stretch (still chosen by `in_stretch` with the over's `from`), get its brief with `run_brief` on **the run's own stretch** (`from: None`). A kept one costs nothing.
  2. Join those briefs' `worked_on`/first lines through the existing `many_lines` and `join`.
  - `dry` returns `calls` = the unkept runs + 1 (the join), and `runs` = the runs in the stretch.
- `over`'s deep path uses `run_stretch(d, r, None)` per run instead of the closure at :685.

- [ ] **Step 1: Write the failing tests.**
  - `over_brief_writes_each_runs_brief_then_joins`: three ended `sh -c` runs and the fake agent. `SummaryMake{target: All, from: <before them>}` makes 4 assistant calls (count `FAKE_AGENT_READS`, or the summary runs the harness exposes). A second identical call with `again: false` makes 0 new per-run calls.
  - `over_dry_counts_one_per_unkept_run_plus_the_join`: dry gives `calls == 4` and `runs == 3`. After one run's brief is kept, `calls == 3`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon --test summary over_`. Expected: FAIL.

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run** `cargo test -p rl-daemon`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(summary): a summary of many runs briefs each first, on its own stretch`

---

### Task 6: Retire step 3's split

**Files:**
- Delete:
  - `code/rl-daemon/tests/facts_structure.rs`
  - `facts::structure`, `STRUCTURE_PROMPT` and `split_structured`, with its tests (`code/rl-daemon/src/facts.rs:50–110, 262–282`)
- Modify:
  - `code/rl-proto/src/methods.rs:1543–1556`: remove `FactsStructure` and `FactsStructureParams`.
  - `code/rl-daemon/src/handlers.rs:119–121`
  - `code/rl-proto/src/facts.rs`: remove `recap`, `worked_on`, `questions` and `next`.
  - `code/rl-ledger/src/facts.rs` test (:62–64)
  - `code/rl-cli/src/tui/feed_ops.rs`: remove `structure_run` (:97–110) and its call (:77). `act_selected` just calls `show_run`. Remove `feed.facts` if nothing else reads it; otherwise keep it filled from `lead.facts`.
  - `code/rl-cli/src/tui/pane.rs:172–179`: drop the soft lines, and update its tests (:821–835).
  - `code/rl-cli/src/tui/prose.rs:22–37`: the happened clause becomes the title plus `headline(lead)` when not empty. Task 9 replaces it.

**Interfaces:** after this task, `RunFacts` has only hard facts:

```rust
RunFacts {
    run, commits, added, removed, files, top_paths, branch, pushed,
    active_secs, tokens_in, tokens_cached, tokens_out, errors, at,
}
```

- [ ] **Step 1:** Remove the items above. Run `cargo build --workspace --all-targets` and fix each compile error at its call site as described.
- [ ] **Step 2:** Run `cargo test --workspace`. Expected: PASS, with no test referring to `FactsStructure` or the soft fields (`rg 'worked_on|FactsStructure|split_structured|merge_recap' code/` finds only `Brief`).
- [ ] **Step 3: Run the gates and commit.** `refactor(report): retire step 3's recap split — the summary engine writes recaps now`

---

### Task 7: Depth: config and the rule

**Files:**
- Modify: `code/rl-proto/src/config.rs`
  - Add `report: ReportConfig` to `Config` (:28–49), named `ReportConfig` so it does not clash with `methods::Report`.
  - Add a `ReportConfig` struct beside `CatchUp` (:55–91).
  - Update the module doc example (:10–22).
- Modify: `code/rl-daemon/src/config.rs`: the config template (:56) and its assertion (:315).
- Create: `code/rl-cli/src/depth.rs`, and add `pub mod depth;` in `code/rl-cli/src/lib.rs`.
- Modify: `code/rl-cli/src/catchup.rs:545`: factor `named_summariser`.

**Interfaces:**
- Produces:
  - **The config struct:**

    ```rust
    #[serde(deny_unknown_fields)]
    pub struct ReportConfig {
        pub brief_after: Option<String>,
        pub deep_after: Option<String>,
        pub summary: Option<String>,
    }
    ```

    - Methods: `pub fn brief_after(&self) -> SignedDuration` (default 2h), `pub fn deep_after(&self) -> SignedDuration` (default 2 days), `pub fn auto_summary(&self) -> bool` (true only for `"auto"`).
    - Unparseable durations fall back to the defaults, the same way `CatchUp::after` handles them.
  - **The depth rule:**
    - `pub enum Depth { Headline, Brief, Deep }`
    - `pub fn depth(away: SignedDuration, r: &rl_proto::config::ReportConfig) -> Depth`. Away up to 2h is `Headline`; over 2h and up to 2 days is `Brief`; over 2 days is `Deep`.
    - `pub const ASK_OVER: u32 = 3;`
  - **The summariser check:** `pub fn named_summariser(r: &ConfigReport) -> bool` (the expression at `catchup.rs:545`). `auto_top` uses it.

- [ ] **Step 1: Write the failing tests.**
  - In `depth.rs`: `depth_is_the_headline_up_to_two_hours`, `depth_is_brief_past_two_hours`, `depth_is_deep_past_two_days` (at 2h, 2h+1m, 2d and 2d+1m), and `depth_follows_configured_thresholds` (`brief_after: 30m`).
  - In `rl-proto` config tests: `report_keys_parse_and_default`, and `unknown_report_key_is_refused`.

- [ ] **Step 2: Run** `cargo test -p rl-cli depth` and `cargo test -p rl-proto config`. Expected: FAIL.

- [ ] **Step 3: Implement.** Document the three keys in the doc example and the daemon template:

  ```yaml
  report:
    brief_after: 2h
    deep_after: 2d
    summary: ask
  ```

- [ ] **Step 4: Run** `cargo test --workspace`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(config): report.brief_after / deep_after / summary — depth by time away`

---

### Task 8: Opening the report writes its recaps

**Files:**
- Modify: `code/rl-cli/src/tui/mod.rs`
  - App fields (near :429–438), initialisers (~:1251–1266)
  - `Mode` (:184): add `ReportCost`
  - main-loop `select!` arms (:1392–1403)
  - `footer` (:2956–2982)
- Modify: `code/rl-cli/src/tui/feed_ops.rs`: `open_report` (:25) and `fetch_report` (:38)
- Modify: `code/rl-cli/src/tui/keys.rs:147–154`: the `y` / any-key handling, extended to the report ask.
- Test: inline in `tui/mod.rs`, next to the report tests (~:4939–5126).

**Interfaces:**
- Consumes: `depth::{depth, Depth, ASK_OVER}`, `catchup::named_summariser` and `ReportConfig` (Task 7); `SummaryGet`, `SummaryMake` and `ConfigGet`.
- Produces:
  - App fields:
    - `briefs: HashMap<String, Summary>`, keyed by run id;
    - `briefing: Option<Briefing>`, where `struct Briefing { rx: tokio::sync::mpsc::UnboundedReceiver<(String, anyhow::Result<Summary>)>, task: tokio::task::JoinHandle<()> }`;
    - `report_ask: Option<Vec<SummaryParams>>`;
    - `report_depth: Depth`.
  - `Mode::ReportCost { runs: u32, calls: u32 }`. Its footer is `"write {runs} recap{s} · {calls} calls · y / any key"`.
  - `pub(super) async fn brief_report(&mut self)`:
    1. It runs once per open, from `open_report` after the first `fetch_report`, never from `refresh`.
    2. It computes `report_depth = depth(now − self.left.unwrap_or(now), &cfg.report)`.
    3. For `Headline`, it returns.
    4. For `Deep`, it sets `report_prose = true`.
    5. For each `Band::Happened` lead whose key is `run:<id>`, it calls `SummaryGet{Run{id}, deep: depth == Deep, from: None}`; a kept recap that is not stale goes into `briefs`.
    6. If none are left, or `!named_summariser`, it returns.
    7. It sums `calls` from `SummaryMake{dry: true}` per remaining run. Over `ASK_OVER`, and without `report.auto_summary()`, it sets `report_ask` and `Mode::ReportCost`. Otherwise it calls `start_briefing`.
  - `fn start_briefing(&mut self, params: Vec<SummaryParams>)` spawns one task. It uses a fresh `Client::connect(&home)`, as `write_top` does (`catch_ops.rs:195–215`), runs `SummaryMake` per run in order, and sends each `(run_id, result)` down the channel. Any previous `briefing` is aborted first.
  - `fn briefed(&mut self, run: String, r: anyhow::Result<Summary>)` inserts into `briefs` on `Ok`. On error it flashes once per briefing. A cancelled briefing says nothing.
  - Closing the report (`open_report` toggling off, or `esc`) aborts `briefing`. Reopening the report replans: `briefs` is kept, so kept recaps come back free.

- [ ] **Step 1: Write the failing tests**, with `panel_with_fake_summariser`. Seed `app.report` with happened run leads for runs that exist in the harness, and set `app.left`.
  - `opening_the_report_after_three_hours_writes_briefs`: `left = now − 3h`, two happened runs. After `open_report()` and a `wait_briefed` helper (copy `wait_topped`), `app.briefs` has both ids, and `report_prose == false`.
  - `opening_the_report_within_two_hours_writes_nothing`: `left = now − 1h`. `briefing.is_none()`, and `briefs` is empty.
  - `opening_the_report_after_three_days_opens_the_prose_with_deep_reads`: `left = now − 3d`. `report_prose == true`, and the `briefs` entries have `deep == true`.
  - `many_recaps_ask_first`: five happened runs. `app.mode == Mode::ReportCost{runs: 5, ..}` and `app.footer(120).starts_with("write 5 recaps · ")`. Pressing `y` starts `briefing`. In a fresh panel, pressing any other key leaves `briefing.is_none()`.
  - `the_refresh_tick_does_not_restart_the_briefing` (Review Focus 4): after the briefing starts, call `refresh()` twice. The same `briefing` task is still running (compare `task.id()`), and `mode` is not `ReportCost`.
  - `no_named_summariser_writes_nothing_and_says_nothing` (Review Focus 5): `panel_with_config` without `assistants.summary`, `left = now − 3h`. `briefing.is_none()`, there is no flash, and the board still draws its rows.

- [ ] **Step 2: Run** `cargo test -p rl-cli report`. Expected: FAIL.

- [ ] **Step 3: Implement** the interfaces. Add the main-loop arm:

  ```rust
  got = async { match &mut self.briefing { Some(b) => b.rx.recv().await, None => std::future::pending().await } } => {
      match got { Some((run, r)) => self.briefed(run, r), None => self.briefing = None }
  }
  ```

  Extend the `y` handling in `keys.rs:147–154`: `if let Some(ps) = self.report_ask.take()`. On `y`, call `start_briefing(ps)`; any other key cancels. In both cases, return to the normal mode.

- [ ] **Step 4: Run** `cargo test -p rl-cli`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(tui): opening the report writes its recaps, at the depth time away calls for`

---

### Task 9: The board, the prose and `rl report --prose` show them

**Files:**
- Modify: `code/rl-cli/src/tui/report.rs`
  - `report_view` (:154) and `report_lines` (:140)
  - the row loop seam after :245
  - tests (:251–474)
- Modify: `code/rl-cli/src/tui/prose.rs`: `clause` (:14), `prose` (:47), lead-in (:74), tests
- Modify: `code/rl-cli/src/tui/mod.rs`: `draw_report` (:2725–2756) passes `&self.briefs`, `self.left` and `report_depth`.
- Modify: `code/rl-cli/src/main.rs`
  - `ReportArgs` (:518): add `yes`
  - `report()` (:1072–1086)
  - `report_parses` test (:2030)

**Interfaces:**
- Consumes: `briefs`, `report_depth` and `Brief::parse` (earlier tasks).
- Produces:
  - `pub fn report_view(report: &Report, width: usize, now: Timestamp, sel: Option<usize>, briefs: &HashMap<String, Summary>) -> (Vec<Line<'static>>, Option<usize>)`. `report_lines` gains `briefs` too.
    - Under a `Band::Happened` row whose run has a brief that is not deep, add up to three dim, indented lines:
      - `worked  <worked_on>`
      - `asks    <questions>`
      - `next    <next>`
    - Each line is truncated to the width, and absent parts are skipped.
    - No lines are added when there is no brief; the row already carries its ladder line (`lead.summary`) and headline.
  - `pub fn prose(report: &Report, briefs: &HashMap<String, Summary>, since: Option<Timestamp>, now: Timestamp) -> String`. The happened clause, in order:
    1. a deep brief gives `"{title}: {its text as one paragraph}"`;
    2. a brief gives `"{title}: {worked_on}"`, plus `", next: {next}"` when present;
    3. otherwise, `lead.summary` when not empty;
    4. otherwise, `headline(lead)`;
    5. otherwise, the title.

    The lead-in is `"While you were out ({fmt_duration(now − since)}):"` when `since` is `Some`, else `"While you were out:"`.
  - `ReportArgs` gains `#[arg(long)] yes: bool`.
  - `rl report --prose`:
    1. `since` defaults to `PresenceTouch.last_seen_at`, copied from `since()` at main.rs:971–985; with none, 24h.
    2. It calls `ReportGet{since}`.
    3. The depth is `depth(now − since)` from `ConfigGet`.
    4. For each happened run, `SummaryGet`, then, when the depth is at least `Brief`, `named_summariser` holds and the dry total is ≤ `ASK_OVER` or `--yes` or `report.summary: auto`, `SummaryMake`.
    5. Over the limit without `--yes`, it prints the prose with what is kept, then the line `# {n} more recaps would take {calls} calls · --yes writes them`.
  - The flat `rl report` keeps its daemon 24h default.
  - **The drill-in shows what the report wrote.** `act_selected` (feed_ops.rs:71), for a `run:` lead with an entry in `briefs`, inserts that `Summary` into `self.made` before `show_run`, so the run pane shows the same recap with no extra call. With no entry, the pane is as today; its hint points at `/summary`, and `/summary deep` reads deeper.
  - Test `enter_on_a_briefed_run_shows_its_brief_in_the_pane`, in `tui/mod.rs` beside `enter_on_a_run_lead_opens_that_run_on_the_list`: seed `briefs` with a three-part text for the run, press `enter`, and `draw_text(app)` contains `the parser`.

- [ ] **Step 1: Write the failing tests.**
  - `report.rs`, `a_happened_rows_brief_shows_under_it`: a three-part brief for the run gives `worked  the parser` and `next    tests` lines after the row. There is no `asks` line for `none`, and `sel_line` still points at the row.
  - `prose.rs`, `prose_tells_a_run_by_its_brief_and_says_how_long`: `since = now − 3h` gives `While you were out (3h):` and the clause contains `the parser` and `next: tests`.
  - `prose.rs`, `prose_without_a_brief_falls_back_to_the_rows_line`: no briefs, `lead.summary = "waiting on you"`. The clause contains it.
  - `main.rs`, `report_parses`: also `["rl","report","--prose","--yes"]`.
  - Update existing callers and tests of `report_view`, `report_lines` and `prose` to pass `&HashMap::new()` / `None`.

- [ ] **Step 2: Run** `cargo test -p rl-cli report prose`. Expected: FAIL.

- [ ] **Step 3: Implement.** `draw_report` passes `&self.briefs`, and to the prose it passes `self.left` and `self.briefs`. The scroll fix in `draw_report` (:2747–2756) must use `sel_line` (the row's index), not assume one line per row.

- [ ] **Step 4: Run** `cargo test -p rl-cli`. Expected: PASS.

- [ ] **Step 5: Run the gates and commit.** `feat(report): the board and the prose tell each run by its written recap`

---

### Task 10: Real recaps to read together (controller, after Task 9)

This is not a code task. It produces the material for the spec's open *content* question, without touching the user's running daemon.

- [ ] **Step 1:** Build the branch with `cargo build --workspace`.
- [ ] **Step 2:** Make an isolated home, `export RELAY_HOME=<scratchpad>/home`.
  - Copy `~/.relay/ledger.db` and `~/.relay/config.yaml` into it (and `io/` if present).
  - Leave `HOME` as is, so the transcripts in `~/.claude`, `~/.codex` and `~/.gemini` resolve.
  - Start the branch's `rld` on it, on its own socket under that home.
- [ ] **Step 3:** Pick about 6 recent ended runs, two each of Claude, Codex and Gemini if present, preferring ones with commits. Run `rl summary run:<id>` (brief) and `rl summary run:<id> --deep` for each. This is about 12–20 real assistant calls.
- [ ] **Step 4:** Collect the outputs into `<scratchpad>/recap-samples.md`. Per run, include the headline facts, the brief and the deep read. Stop the isolated `rld`.
- [ ] **Step 5:** Hand the file to the user with the spec's open content questions: voice, what each part leads with, how much of the facts it repeats, how a long deep read is organised, and needs-you versus finished.
