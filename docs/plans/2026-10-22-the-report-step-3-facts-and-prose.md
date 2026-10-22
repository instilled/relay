# The report, step 3 — facts collector and prose digest — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collect deterministic, offline facts about a run when it ends, freeze them on the run, and render the report as an enriched board plus a short prose digest (`rl report --prose` and the pane's `/report`).

**Architecture:** A per-run `run_facts` record (migration `0029`) is written once at `finish()` from three local, windowed sources — a HEAD-delta git read, a one-shot transcript read, and the `shell.command` events — and refreshed with the agent's own recap blob by the existing recap sweep. No LLM call in that path. The report projection attaches the record to each built-in run lead (`facts` rides the wire, additive, protocol 7→8). The TUI renders a one-line digest on the row and the full facts on drill-in; opening a run's drill-in lazily runs one assistant call to split the recap into worked-on/questions/next and caches it. A pure client-side renderer turns the `Report` + facts into prose for `rl report --prose` and the pane.

**Tech Stack:** Rust workspace (crates under `code/`): `rl-proto`, `rl-ledger` (SQLite migrations), `rl-daemon` (tokio), `rl-cli` (ratatui TUI + CLI), `rl-detect`. `jiff::Timestamp` for time.

**Spec:** `docs/specs/2026-10-21-the-report-step-3-facts-and-prose.md`

## Global Constraints

- **No backward compat.** `PROTOCOL_VERSION` 7 → 8 (`code/rl-proto/src/lib.rs`); `facts` is additive on `Lead` with `#[serde(default)]`. Kill running `rld`/`rl` after merge.
- Migration `0029` is append-only; never edit an existing migration. Add the tuple to `MIGRATIONS` in `code/rl-ledger/src/migrate.rs`.
- **Relay makes no network call.** git is read-only; the collector runs no assistant. The only assistant call is the lazy structured split (Task 11), gated by a setting and fired only on drill-in.
- **Free at scale by default.** Nothing in the collect-at-finish or recap-sweep path may call an assistant.
- CI gates, verified locally before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. A clippy warning is a CI failure.
- Facts window is the run's `[started_at, ended_at]`. Tokens only, no dollars. Forward-only: no backfill of runs that ended before this ships.

## Review Focus

- **A run whose cwd is not a git repo** (or a detached/empty repo): git collection must no-op and leave git facts empty, never error or block `finish()`. → Task 2 tests.
- **A shell that exits 0 but ran a failing command:** `errors` counts any non-zero `shell.command` event in the window, and an event with absent `exit` is *unknown*, not a failure. → Task 4 tests.
- **A run with no transcript yet / an unflushed recap at `finish()`:** facts are written without the recap blob, and the recap sweep fills it later without overwriting harder facts. → Task 6 tests.
- **A happened lead with no facts record** (pre-existing run, or collection failed): the row, drill-in, and prose all render gracefully with facts absent. → Tasks 8, 9 tests.
- **The structured-split setting is off, or the recap blob is empty:** drill-in shows the facts it has and makes no assistant call; prose omits the soft clause. → Task 11 tests.

---

### Task 1: `run_facts` store + `RunFacts` type + protocol bump

**Files:**
- Create: `code/rl-ledger/migrations/0029_run_facts.sql`
- Modify: `code/rl-ledger/src/migrate.rs` (add the `0029` tuple to `MIGRATIONS`)
- Create: `code/rl-ledger/src/facts.rs` + register `mod facts;` in `code/rl-ledger/src/lib.rs`
- Create: `code/rl-proto/src/facts.rs` + register `mod facts; pub use facts::RunFacts;` in `code/rl-proto/src/lib.rs`
- Modify: `code/rl-proto/src/lib.rs` (`PROTOCOL_VERSION` 7 → 8)
- Test: inline `#[cfg(test)]` in `facts.rs` (ledger), mirroring the migration-roundtrip tests other ledger modules use.

**Interfaces:**
- Produces: `rl_proto::RunFacts` — a `#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]` struct, all fields `#[serde(default)]`:
  `run: String`, `commits: u32`, `added: u32`, `removed: u32`, `files: u32`, `top_paths: Vec<String>`, `branch: Option<String>`, `pushed: bool`, `active_secs: Option<u64>`, `tokens_in: Option<u64>`, `tokens_out: Option<u64>`, `errors: u32`, `recap: Option<String>`, `worked_on: Option<String>`, `questions: Option<String>`, `next: Option<String>`, `at: Timestamp`.
- Produces (ledger): `facts::set(conn, &RunFacts) -> Result<()>` (upsert by `run`), `facts::get(conn, run: &str) -> Result<Option<RunFacts>>`, `facts::get_many(conn, ids: &[String]) -> Result<std::collections::HashMap<String, RunFacts>>`.
- Storage: table `run_facts(run TEXT PRIMARY KEY, facts_json TEXT NOT NULL, at TEXT NOT NULL)`; `facts_json` is the serialized `RunFacts`. (Mirrors the `cost_json` serialize pattern already in `runs.rs`.)

- [ ] **Step 1: Write the failing test** `set_then_get_roundtrips` and `get_missing_is_none` in `facts.rs`: open an in-memory conn, `migrate::run`, `set` a populated `RunFacts`, assert `get` returns an equal value and `get` of an unknown id is `None`; `get_many` returns only the present ids.
- [ ] **Step 2: Run** `cargo test -p rl-ledger facts` — Expected: FAIL (module/table absent).
- [ ] **Step 3: Write `0029_run_facts.sql`**, add its tuple to `MIGRATIONS`, implement `facts.rs` (serialize `RunFacts` to `facts_json`), add `RunFacts` to `rl-proto`, bump `PROTOCOL_VERSION` to 8.
- [ ] **Step 4: Run** `cargo test -p rl-ledger facts` and `cargo test -p rl-proto` — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(ledger): run_facts store, RunFacts type, protocol 8`.

---

### Task 2: Git facts — HEAD delta and diff-stat

**Files:**
- Modify: `code/rl-daemon/src/git.rs` (add public readers beside the private `git(cwd, args)` wrapper ~:27)
- Test: inline `#[cfg(test)]` in `git.rs` using a temp repo created with `git init` + commits (mirror any existing temp-repo test; else `tempfile`).

**Interfaces:**
- Produces:
  - `pub async fn head(cwd: &Path) -> Option<String>` — `git rev-parse HEAD`, `None` outside a repo.
  - `pub struct DiffStat { pub commits: u32, pub added: u32, pub removed: u32, pub files: u32, pub top_paths: Vec<String> }`
  - `pub async fn delta(cwd: &Path, from: &str, to: &str) -> DiffStat` — counts commits in `from..to` (`git log --oneline from..to`), and `git diff --numstat from to` summed into added/removed/files, with the ≤3 dirs/files by churn as `top_paths`. Empty `DiffStat::default()` when not a repo, `from`/`to` unknown, or `from == to`.
  - `pub async fn pushed(cwd: &Path, branch: &str) -> bool` — whether an upstream exists and contains HEAD (`git rev-parse --abbrev-ref @{u}` succeeds and `git branch -r --contains HEAD` non-empty); `false` on any failure. (Local only; no fetch.)

- [ ] **Step 1: Write failing tests** `delta_counts_commits_and_churn` (temp repo: base commit, capture `from`; commit two files; capture `to`; assert `commits==2`, `added/removed/files` match, `top_paths` names the changed dir) and `delta_outside_repo_is_empty` (a temp non-repo dir → `DiffStat::default()`), `head_outside_repo_is_none`.
- [ ] **Step 2: Run** `cargo test -p rl-daemon git::` — Expected: FAIL.
- [ ] **Step 3: Implement** `head`, `delta`, `pushed`, `DiffStat` via the existing `git(cwd, args)` wrapper (extend or sibling). Parse `--numstat` lines (`added\tremoved\tpath`; binary files show `-` — count as a changed file, 0/0 churn).
- [ ] **Step 4: Run** `cargo test -p rl-daemon git::` — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(daemon): read a run's commits and diff-stat as a HEAD delta`.

---

### Task 3: Transcript facts — token usage and active span

**Files:**
- Modify: `code/rl-daemon/src/transcript.rs` (add windowed readers beside the existing parsers)
- Test: inline `#[cfg(test)]` in `transcript.rs`, reusing the fixture jsonl under `code/fixtures/` (e.g. `claude-session.jsonl`) and/or a small inline sample, as the existing transcript tests do.

**Interfaces:**
- Produces:
  - `pub struct Usage { pub tokens_in: u64, pub tokens_out: u64 }`
  - `pub fn usage(body: &str, since: Option<Timestamp>) -> Usage` — sum the per-message `usage` token fields of entries at/after `since` (Claude `message.usage.{input_tokens,output_tokens,cache_*}`; Codex equivalent where present). Missing usage → zero.
  - `pub fn active_span(body: &str, since: Option<Timestamp>, gap: Duration) -> Option<u64>` — seconds of activity across entry timestamps in-window, collapsing idle gaps longer than `gap`; `None` when fewer than two timestamped entries.
- Consumes: the existing entry-timestamp parsing already used by `claude_recap`/`codex_turns`.

- [ ] **Step 1: Write failing tests** `usage_sums_in_window` (inline jsonl with two messages carrying usage; assert the sum; one before `since` excluded) and `active_span_collapses_idle_gaps` (timestamps 0s,10s,[2h gap],+5s with `gap=120s` → ~15s, not ~2h) and `active_span_needs_two_points` (→ `None`).
- [ ] **Step 2: Run** `cargo test -p rl-daemon transcript::` — Expected: FAIL.
- [ ] **Step 3: Implement** `Usage`, `usage`, `active_span`. One pass over entries; parse timestamps with the module's existing helper.
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(daemon): read token usage and active span from a transcript window`.

---

### Task 4: The collector — assemble `RunFacts` for a run

**Files:**
- Create: `code/rl-daemon/src/facts.rs` + `mod facts;` in `code/rl-daemon/src/server.rs` (or wherever daemon modules are declared)
- Test: inline `#[cfg(test)]` in `facts.rs`; a real-ledger harness only where needed, else pure assembly tests over hand-built inputs.

**Interfaces:**
- Consumes: Task 2 (`git::{head, delta, pushed, DiffStat}`), Task 3 (`transcript::{usage, active_span}`), `summary::locate` (`code/rl-daemon/src/summary.rs:239`), `rl_ledger::events::list` with `EventFilter { run, since, kind }`, `rl_proto::RunFacts`, `rl_proto::types::{Run, RunKind, Cause}`.
- Produces:
  - `pub const GAP: Duration = Duration::from_secs(120);`
  - `pub async fn collect(d: &Daemon, run: &Run, start_head: Option<&str>, end_head: Option<&str>) -> RunFacts` — the frozen local facts: for an agent run, git `delta(cwd, start_head, end_head)` + `pushed` + transcript `usage`/`active_span` (read the file via `read_body`/windowed read) ; `errors` = count of `shell.command` events in `[started_at, ended_at]` whose `data.exit` is present and non-zero, for every kind; `recap`/`worked_on`/`questions`/`next` left `None` here (filled by Task 6 / Task 11); `branch` from `run.branch`, `pushed` from `git::pushed(cwd, branch)`; `at = ended_at.unwrap_or(started_at)`. Never calls an assistant. Never panics on a missing repo/transcript.
  - `pub async fn commits_in(d: &Daemon, run: &Run, start_head: Option<&str>, end_head: Option<&str>) -> Vec<String>` — the commit shortlog lines for the `commit.landed` events Task 5 emits.

- [ ] **Step 1: Write failing tests** `errors_counts_nonzero_shell_commands` (hand-built events: exit 0, exit 2, exit absent → `errors == 1`) and `collect_outside_repo_leaves_git_empty` (a run whose cwd is not a repo → `commits==0`, `added==0`, no panic). Use the lightest harness that observes these (mock the event list via a tiny in-memory ledger if the daemon test harness supports it; otherwise assert the pure counting helper split out for testability).
- [ ] **Step 2: Run** `cargo test -p rl-daemon facts::` — Expected: FAIL.
- [ ] **Step 3: Implement** `collect`, `commits_in`, `GAP`. Factor the error-counting into a pure `fn count_errors(events: &[Event]) -> u32` so Step 1 can test it without a live daemon.
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(daemon): collect a run's frozen facts over its window`.

---

### Task 5: Capture HEAD at start/end and collect at `finish()`

**Files:**
- Modify: `code/rl-daemon/src/supervisor.rs` (`finish()` ~:913; run start where `git::placement` is called ~:237)
- Modify: `code/rl-daemon/src/git.rs` only if a start-HEAD stash helper is needed
- Test: a daemon integration test if the harness supports starting/finishing a run; else assert the ordering/seam via a focused unit on the extracted collection step.

**Interfaces:**
- Consumes: Task 4 (`facts::{collect, commits_in}`), Task 1 (`rl_ledger::facts::set`), `rl_ledger::events::append`, `event_kind::COMMIT_LANDED` (`code/rl-proto/src/types.rs:488`).
- Produces: at run start, the start `HEAD` is recorded (in memory on the `Live`/supervisor state, or a lightweight store) so `finish()` can pass it to `collect`. `finish()` computes `end_head = git::head(cwd)`, calls `facts::collect(...)` **outside** the `ledger.call` closure (git/transcript are async/blocking), then writes `facts::set` and appends one `COMMIT_LANDED` event per line from `commits_in` in a ledger transaction. Collection failure is logged and does not fail `finish()`.

- [ ] **Step 1: Write the failing test** `a_finished_agent_run_has_facts` (if a live-daemon harness exists: start a short run in a temp git repo with one commit, finish it, assert `facts::get(run)` has `commits>=1`). If no such harness, write `start_head_is_captured_and_passed` against the extracted seam and note the handler path is covered by the manual check.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement** the start-HEAD capture and the `finish()` collection + `commit.landed` emission. Do not hold git/transcript I/O inside the ledger closure.
- [ ] **Step 4: Run** `cargo test -p rl-daemon` — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(daemon): collect facts and emit commit.landed when a run ends`.

---

### Task 6: Recap sweep refreshes the facts blob

**Files:**
- Modify: `code/rl-daemon/src/recap.rs` (`pick_up` ~:32)
- Test: inline unit asserting the merge rule (recap set, hard facts preserved).

**Interfaces:**
- Consumes: Task 1 (`rl_ledger::facts::{get, set}`), the recap text `pick_up` already computes.
- Produces: when `pick_up` keeps a new recap, it also loads the run's `RunFacts` (creating a default if absent), sets `recap = Some(text)`, and `facts::set`s it — **without** touching `commits/added/removed/tokens/errors/...`. No assistant call (unchanged: the sweep reads the agent's own recap).

- [ ] **Step 1: Write the failing test** `recap_refresh_keeps_hard_facts` (a `RunFacts` with `commits=3` already stored; apply the merge with a new recap; assert `recap` set and `commits` still 3).
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement** the merge in `pick_up` (factor a pure `fn merge_recap(facts: &mut RunFacts, recap: &str)` for the test).
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(daemon): the recap sweep fills the facts blob, free of any call`.

---

### Task 7: Attach facts to the happened lead on the wire

**Files:**
- Modify: `code/rl-proto/src/lead.rs` (add `#[serde(default)] pub facts: Option<RunFacts>` to `Lead`)
- Modify: `code/rl-daemon/src/report.rs` (`project_builtins` ~:34; it needs the facts map)
- Modify: `code/rl-daemon/src/handlers.rs` (`ReportGet` ~:419-443: load `facts::get_many` for the runs and pass into the projection)
- Test: inline in `report.rs` — a finished run + a `RunFacts` → the lead carries `facts`.

**Interfaces:**
- Consumes: Task 1 (`RunFacts`, `facts::get_many`).
- Produces: `Lead.facts: Option<RunFacts>`; `project_builtins(..., facts: &HashMap<String, RunFacts>)` sets `facts` on each built-in run lead keyed `run:{id}` (`None` when absent). Other lead kinds leave it `None`.

- [ ] **Step 1: Write the failing test** `a_happened_run_lead_carries_its_facts` (build a finished `Run` + a `RunFacts` map; `project_builtins` → the matching lead has `facts.is_some()` with the right `commits`; a run with no facts → `None`).
- [ ] **Step 2: Run** `cargo test -p rl-daemon report::` — Expected: FAIL.
- [ ] **Step 3: Implement** the `Lead` field and thread the facts map through `project_builtins` and the `ReportGet` handler.
- [ ] **Step 4: Run** `cargo test -p rl-daemon report::` and `cargo test -p rl-proto` — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(report): carry a run lead's facts on the report`.

---

### Task 8: Row digest — the one-line headline fact

**Files:**
- Modify: `code/rl-cli/src/tui/report.rs` (the row rendering added in step 2)
- Test: inline `#[cfg(test)]` asserting the digest string per kind.

**Interfaces:**
- Consumes: `Lead.facts: Option<RunFacts>`, the `Lead`'s derived kind (agent/shell/flow — from `run_source`/`RunKind` already used in the board).
- Produces: `fn headline(lead: &Lead) -> String` — agent: `+{added}/−{removed} · {active}m · {commits} commits` (omit a clause whose value is absent/zero); shell: `ok` or `{errors} error(s)`; flow: `fired` + ` · {n} decision(s)` when present. Empty string when `facts` is `None`. Rendered into the existing row columns via `fit()` (no new column; folds into the digest cell).

- [ ] **Step 1: Write failing tests** `agent_headline_from_facts`, `shell_headline_errors`, `headline_absent_facts_is_empty`.
- [ ] **Step 2: Run** `cargo test -p rl-cli report` — Expected: FAIL.
- [ ] **Step 3: Implement** `headline` and wire it into the row.
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(tui): a run row shows its headline fact`.

---

### Task 9: Prose renderer + `rl report --prose`

**Files:**
- Create: `code/rl-cli/src/tui/prose.rs` (pure renderer) + `mod prose;`
- Modify: `code/rl-cli/src/main.rs` (the `report` CLI command: add `--prose`)
- Test: inline `#[cfg(test)]` in `prose.rs` over a hand-built `Report`.

**Interfaces:**
- Consumes: `rl_proto::methods::Report`, `Lead`, `RunFacts`, `fmt_when`/duration helpers, the lane grouping already in `tui/report.rs::lanes` (reuse; do not re-sort).
- Produces: `pub fn prose(report: &Report, now: Timestamp) -> String` — a short narrative, needs-first, grouped by co-worker: a `While you were out` lead-in, then `Needs you` / `Live` / `Happened` paragraphs, one clause per lead leading with the headline fact; uses `facts.recap`/`worked_on`/`next` when present, the title otherwise; omits a band with no leads. Deterministic, no network.
- `rl report --prose` prints `prose(&report, now)`; the existing flat output is unchanged when the flag is absent.

- [ ] **Step 1: Write failing tests** `prose_leads_with_needs_you` (a report with a needs-you + a happened agent lead with facts → string contains the needs-you line first and the happened clause with `+/−` and commits) and `prose_omits_empty_bands`.
- [ ] **Step 2: Run** `cargo test -p rl-cli prose` — Expected: FAIL.
- [ ] **Step 3: Implement** `prose` and the `--prose` flag dispatch.
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(cli): rl report --prose renders the report as a narrative`.

---

### Task 10: Pane `/report` prose view

**Files:**
- Modify: `code/rl-cli/src/tui/report.rs` + the `/report` view state/draw and key dispatch (the list-area view added in step 2)
- Test: inline App test asserting the toggle flips what `draw_report`/`report_lines` emit.

**Interfaces:**
- Consumes: Task 9 (`prose::prose`), the existing `View::Report` + `report: Option<Report>`.
- Produces: a board/prose toggle on the report view (a `prose: bool` on the view state; a free key — e.g. `p` — flips it; record the choice). When on, the view renders `prose(report, now)` wrapped to width; when off, the step-2 board. The cursor/act/tag/dismiss keys stay on the board view only.

- [ ] **Step 1: Write the failing test** `report_view_toggles_to_prose` (seed `report`, flip the toggle, assert the rendered lines contain a prose lead-in rather than lane headers).
- [ ] **Step 2: Run** `cargo test -p rl-cli report` — Expected: FAIL.
- [ ] **Step 3: Implement** the toggle + key + draw branch.
- [ ] **Step 4: Run** the tests — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(tui): /report toggles between the board and the prose digest`.

---

### Task 11: Lazy structured split on drill-in

**Files:**
- Modify: `code/rl-proto/src/methods.rs` (new method `FactsStructure { run: String } -> RunFacts`)
- Modify: `code/rl-ledger/src/facts.rs` (no schema change; reuse `set`/`get`)
- Create/Modify: `code/rl-daemon/src/facts.rs` (`pub async fn structure(d, run) -> Result<RunFacts>`) + register in `code/rl-daemon/src/handlers.rs`
- Modify: `code/rl-cli/src/tui/...` (the report drill-in: call `FactsStructure` on open when the setting is on and the split is absent; show full facts)
- Modify: config to add a `report.structure` (or reuse `assistants.summary`) on/off gate — follow the existing config pattern in `code/rl-*` for a boolean setting; record the exact key chosen.
- Test: a daemon test that `structure` is a no-op (returns facts unchanged, makes no call) when the recap is empty or the setting is off; a parse test for splitting an assistant answer into the three fields.

**Interfaces:**
- Consumes: Task 1 (`RunFacts`, `facts::{get,set}`), `assistant::run` (as `summary.rs::ask` uses it), the run's `recap` blob.
- Produces:
  - `FactsStructure` RPC (write method; `d.ledger.call` to persist the cache) returning the updated `RunFacts`.
  - `facts::structure(d, run)`: if the setting is off, or `recap` is empty, or `worked_on` already cached → return the stored `RunFacts` unchanged (no call). Else run one `assistant::run` with a fixed structuring prompt over the recap, parse into `worked_on`/`questions`/`next` via `fn split_structured(answer: &str) -> (Option<String>, Option<String>, Option<String>)`, `facts::set`, return.
  - The drill-in calls `FactsStructure` once on open (then reads from the cached facts), and renders worked-on/questions/next when present.

- [ ] **Step 1: Write failing tests** `structure_is_noop_without_recap_or_when_off` and `split_structured_parses_three_sections` (a sample answer with the three labeled sections → the three Options).
- [ ] **Step 2: Run** `cargo test -p rl-daemon facts::` — Expected: FAIL.
- [ ] **Step 3: Implement** the method, the no-op gates, `split_structured`, the handler registration, the config key, and the drill-in call.
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS.
- [ ] **Step 5: fmt + clippy, then commit** — `feat(report): split a run's recap into worked-on/questions/next on drill-in`.

---

## Notes for the executor

- Tasks 1–7 are backend and testable without the TUI; 8–11 are the renderings. 1 → {2,3} → 4 → 5 → 6, then 7, then the UI tasks; 11 depends on 1 and 7.
- Keep grouping/ordering in `lanes()` (step 2); `headline`/`prose` are rendering-only.
- The run's derived kind (agent/shell/flow) is already computed for the board by `run_source`/`RunKind` — reuse it, don't re-derive.
- **PR correlation is minimal by design:** the collector records only `branch` + `pushed` (per spec, "nothing more"). Linking a run to an existing GitHub-source PR lead by shared branch is a deferred follow-up, not a task here.
- After the last task: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` green on the final HEAD, then the whole-branch review.
