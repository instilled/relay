# The report, step 4b — the review queue and the inbox · Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `rld` queues every run that comes to rest (asks or finished) as a pointer in `review_queue`; the report becomes one two-pane inbox, by team, that reads the queue, the logs and (where there is one) git, with no assistant call except `p`.

**Architecture:**
- **Ledger.** Migration `0031` adds `review_queue` (one row per `(source, ref)`, a `reviewed` flag keeps *from*) and `runs.start_head`. `rl-ledger/src/review_queue.rs` holds the writes and reads.
- **Daemon.** A pure classifier (`rest.rs`) tells asks from finished. The `run.hook` handler queues turn ends and permission prompts; the supervisor queues screen rests after a grace and adds `stopped`/`failed` at a run's end. `report.get` puts each run lead's queue item, its last message and what changed (git, optional) on the lead; a new `queue.item` reads the log since *from*; `queue.review` clears.
- **Client.** A pure model (`tui/inbox.rs`) builds the NEEDS YOU strip, teams (today's lanes) with counts and gists. `tui/report.rs` draws the list pane and the reading pane side by side. Report keys move to `j/k/enter/esc/x/r/p/w/t`. The written summary (`prose.rs`) walks the queue. Step 4's generate-on-open is retired.

**Tech Stack:** Rust workspace (`code/`): tokio, rusqlite, serde, ratatui, jiff, clap.

**Spec:** `docs/specs/2026-10-29-the-report-step-4b-review-queue.md` (on `chore/roadmap`). Read it before any task. Mockups: the "Relay report mockups" canvas, artboards 4–6.

**Code base:** `main` at `7d73621` (the `chore/roadmap` worktree's `code/` is stale; never read code there).

## Global Constraints

- **No backward compat.** `PROTOCOL_VERSION` 9 → 10 (`code/rl-proto/src/lib.rs:23`), bumped once in Task 1. Kill the running `rld`/`rl` after merge.
- **Migrations are append-only.** The next one is `0031`, registered in `code/rl-ledger/src/migrate.rs` `MIGRATIONS`; add `review_queue` to the table list in `applies_once_and_is_idempotent` (`migrate.rs:116-142`).
- **Gates before every commit:** `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
- **Test fixtures are synthetic.** Never copy real session content.
- **Wire names, exactly:** kind `asks` | `finished`; how `permission` | `reply` | `screen`; hints `question`, `permission`, `stopped`, `failed`, `unsure` (closed set). Source for runs: `run`.
- **Git is optional.** Every git figure is `Option`; no repo → the field is `None` and nothing is drawn for it, never a placeholder.
- **Screen grace:** `SCREEN_GRACE = 30s`.
- **Trims:** a line is the first non-empty line, cut to the width with `…` (`rows::fit`); a block is the first paragraph, at most 6 wrapped lines.
- **Two panes at ≥ 100 columns** (`INBOX_SPLIT_W = 100`); list pane width `(w * 36 / 100).clamp(34, 52)`. Below 100: one pane at a time (see Task 7).
- **Opening:** `report.brief_after` (default `2h`) picks the starting selection only. `report.deep_after` and `report.summary` are removed.
- **Colours** (`tui/theme.rs`): asks `needs()`, failed `bad()`, working `fg(LIVE)`, done `dim()` with a `✓`. Marks: `●` asks, `✕` failed, `✓` done, `◐` working.
- **Commit messages** end with `Claude-Session: https://claude.ai/code/session_01MAdNa6AXB7uzyBtwg2uMYK`.

## Rulings on the spec's open questions

- **Gemini's message:** `payload["prompt_response"]`; Claude and Codex: `payload["last_assistant_message"]`. Missing or empty → the log's last assistant message (Task 2); none there either → *asks* + `unsure`.
- **The log position in *from*:** time only. Every log reader already takes `from: Option<Timestamp>`; no byte offset is stored. *from* is `(at, head)`.
- **The sweep:** stays as it is in 4b; the inbox doesn't read seeds. Retiring it is a later cleanup.
- **Narrow terminals:** Task 7.
- **`p` on a team** is not built in 4b: it flashes `open an item to read it deeply`. (A team is a lane, not a summary target; a deep read over a lane needs a new target. Noted as a deviation for the user.)
- **Reply** is `run.input` with the text plus `\r`.
- **Codex out of order:** a Codex `Stop` whose `turn_id` differs from the last `UserPromptSubmit`'s `turn_id` for that run queues nothing. No `turn_id` on either → queue.
- **`x` on a lead with no queue item** (a decision, a firing) dismisses it (`LeadDismiss`); `d` goes.
- **`rl report`** prints the written summary; `--prose` and `--yes` go.

## Review Focus

1. **A run in a folder with no git repo** (a shell in `/tmp`, an agent working in Notion) is queued, listed and opened with no git line, no "nothing in git", and no panic. Pinned in Tasks 5 and 7.
2. **The report's refresh tick** (it refetches every few seconds) keeps the selection: inside a team, on an item, on *All teams*, even when the selected item was reviewed elsewhere (then the next one). Pinned in Task 8.
3. **A hook that must not queue:** Claude `idle_prompt`, Codex `Interrupt`, a foreign run, a guarded `carries_on`, a late Codex `Stop`. Pinned in Task 3.
4. **A reviewed run that rests again** is queued again with `rests == 1` and *from* at the review; `stopped`/`failed` survive later rests until review. Pinned in Task 1.
5. **A terminal 80 columns wide** shows the list, and `enter`/`esc` move between it and the reading pane. Pinned in Task 7.

---

### Task 1: The queue in the ledger and on the wire

**Files:**
- Create: `code/rl-ledger/migrations/0031_review_queue.sql`; register in `migrate.rs`.
- Create: `code/rl-ledger/src/review_queue.rs`; `pub mod review_queue;` in `lib.rs`.
- Modify: `code/rl-ledger/src/runs.rs`: `set_start_head`, and `Run` reads `start_head`.
- Create: `code/rl-proto/src/queue.rs`; `pub mod queue;` in `lib.rs`.
- Modify: `code/rl-proto/src/lib.rs:23` → `PROTOCOL_VERSION = 10`.
- Modify: `code/rl-proto/src/types.rs` `Run`: `start_head: Option<String>` (`#[serde(default)]`).

**Interfaces:**
- Produces (`rl_proto::queue`, serde `snake_case`, `FromStr`/`as_str` like `lead::Band`):
  - `enum RestKind { Asks, Finished }`, `enum How { Permission, Reply, Screen }`, `enum Hint { Question, Permission, Stopped, Failed, Unsure }`.
  - `struct QueueItem { source: String, reference: String, kind: RestKind, how: How, hints: Vec<Hint>, at: Timestamp, rests: u32, from: Timestamp, from_head: Option<String> }` (`reference` is the spec's *ref*; serde name `ref`).
  - `struct Changed { commits: u32, added: u64, removed: u64, subjects: Vec<String>, branch: Option<String>, pushed: bool }`.
- Produces (`rl_ledger::review_queue`, all `fn(conn: &Connection, …) -> anyhow::Result<…>`):
  - `rest(conn, source, reference, kind, how, hints: &[Hint], at, start: Timestamp, start_head: Option<&str>) -> Result<()>`.
    - No row: insert, `rests = 1`, `from = start`, `from_head = start_head`.
    - Reviewed row: set kind/how/hints/at, `rests = 1`, `reviewed = 0`; keep *from*.
    - Queued row: set kind/how/at, `rests += 1`; hints = the new ones ∪ the row's `stopped`/`failed`.
  - `end(conn, source, reference, hint: Hint /* Stopped | Failed */, at, start, start_head) -> Result<()>`: add the hint; a missing or reviewed row is queued as `finished`, `how` `reply` if it had one else `screen`, `at` = the end.
  - `review(conn, source, reference, at, head: Option<&str>) -> Result<bool>`: `reviewed = 1`, `from = at`, `from_head = head`, hints cleared; `false` when nothing was queued.
  - `list(conn) -> Result<Vec<QueueItem>>`: unreviewed rows, `at` desc. `get(conn, source, reference) -> Result<Option<QueueItem>>` (unreviewed only).
  - `last_rest(conn, source, reference) -> Result<Option<(Timestamp, How)>>` (reviewed or not; Task 4 uses it for the grace).
- `runs::set_start_head(conn, id, head: &str) -> Result<()>`.

- [ ] **Step 1: Write the failing tests** in `review_queue.rs` (`Ledger::open_in_memory()` + `call_blocking`, as in `dismissals.rs`):
  - `a_first_rest_queues_from_the_start`: `rest(.., Asks, Reply, &[Question], t2, t0, Some("abc"))` → `list` is one item, `rests == 1`, `from == t0`, `from_head == Some("abc")`.
  - `resting_again_updates_the_one_item`: two rests → one item, `rests == 2`, kind and `at` from the second.
  - `stopped_survives_a_later_rest`: `end(.., Stopped, ..)` then `rest(.., Finished, Reply, &[], ..)` → hints `== [Stopped]`.
  - `review_clears_and_keeps_from`: `review(.., t5, Some("def"))` → `list` empty, `get` `None`; a later `rest` → `rests == 1`, `from == t5`, `from_head == Some("def")`, hints only the new ones.
  - `end_queues_an_unqueued_run_as_finished`: `end(.., Failed, ..)` with no row → kind `Finished`, hints `[Failed]`.
  - `review_of_nothing_is_false`.
  - In `rl-proto/src/queue.rs`: `wire_names_are_exact` round-trips each enum through `serde_json` and `FromStr` against the Global Constraints strings, and `QueueItem` serialises `reference` as `"ref"`.
  - Update `migrate.rs` `applies_once_and_is_idempotent` with `review_queue`.

- [ ] **Step 2: Run** `cargo test -p rl-ledger review_queue` and `cargo test -p rl-proto queue`. Expected: FAIL to compile.

- [ ] **Step 3: Implement.**
  - SQL: `review_queue(source TEXT NOT NULL, ref TEXT NOT NULL, kind TEXT NOT NULL, how TEXT NOT NULL, hints TEXT NOT NULL DEFAULT '[]', at INTEGER NOT NULL, rests INTEGER NOT NULL, from_at INTEGER NOT NULL, from_head TEXT, reviewed INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source, ref))`, and `ALTER TABLE runs ADD COLUMN start_head TEXT`. Times via `rl_core::time::to_millis`/`from_millis`.
  - Bump `PROTOCOL_VERSION`.

- [ ] **Step 4: Run** `cargo test -p rl-ledger` and `cargo test -p rl-proto`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(ledger): the review queue — one pointer per session, asks or finished`

---

### Task 2: Telling asks from finished, and reading the last message

**Files:**
- Create: `code/rl-daemon/src/rest.rs` (pure); `mod rest;` in `main.rs`/`lib.rs` as the crate does.
- Modify: `code/rl-daemon/src/transcript.rs`: `last_message`, `log_lines`.

**Interfaces:**
- `pub fn classify(message: Option<&str>) -> (RestKind, Vec<Hint>)`:
  - *asks* + `question` when the last paragraph (text after the last blank line, code fences removed) ends with `?`, or contains, case-insensitive, one of: `should i`, `want me to`, `shall i`, `which `, `do you want`, `let me know if`, `let me know whether`.
  - *finished*, no hints, otherwise.
  - `None`, empty, whitespace, or only a fenced code block → *asks* + `unsure`.
- `pub fn last_message(agent: Agent, text: &str) -> Option<String>`: Claude the last `type:"assistant"` record's text blocks joined; Codex `codex_turns(text, None).last()?.message`; Gemini the last `type:"gemini"` record's `content` (last record per `id`).
- `pub struct LogLine { pub at: Timestamp, pub who: String /* "you" | agent name */, pub text: String /* first line */ }` in `rl_proto::queue` (serde), and `pub fn log_lines(agent: Agent, text: &str, from: Option<Timestamp>) -> Vec<LogLine>` built from the same records `claude_messages`/`codex_turns`/`gemini_messages` read, one per message, oldest first.

- [ ] **Step 1: Write the failing tests.**
  - `rest.rs`, one fixture per phrasing: `ends_on_a_question_asks` ("Removed 23 worktrees.\n\nShould I prune the branches too?"), `want_me_to_asks`, `let_me_know_whether_asks`, `which_asks` ("Which of the two do you prefer."), `a_plain_wrap_up_is_finished` ("All 8 findings fixed; gates green."), `a_question_in_an_earlier_paragraph_is_finished` ("Why did it fail? A stale lock.\n\nFixed and pushed."), `no_message_is_unsure_asks` (`None` → `(Asks, [Unsure])`), `only_code_is_unsure_asks`.
  - `transcript.rs`: `last_message_reads_each_agent` (one synthetic body per agent, two assistant messages each; the second wins; Gemini repeats id `g2` and the last record wins); `log_lines_start_at_from` (three messages, `from` between the first and second → two lines, `who` `you`/`claude`, first line only).

- [ ] **Step 2: Run** `cargo test -p rl-daemon rest transcript`. Expected: FAIL.

- [ ] **Step 3: Implement** as above.

- [ ] **Step 4: Run** the same. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(daemon): tell asks from finished by the reply, read the last message`

---

### Task 3: Hooks queue a run

**Files:**
- Modify: `code/rl-daemon/src/handlers.rs` RunHook (:194-287).
- Create: `code/rl-daemon/src/queue.rs`: the hook rule (pure) and `record(d, run, …)`.
- Modify: `code/rl-daemon/src/lib.rs` (or wherever `Daemon` lives): `turns: Mutex<HashMap<String, String>>` (run id → last prompt `turn_id`).
- Test: `code/rl-daemon/tests/queue.rs`.

**Interfaces:**
- `pub enum HookRest { TurnEnd(Option<String> /* payload message */), Permission }` and `pub fn hook_rest(dialect: &str, event: &str, payload: &Value) -> Option<HookRest>`:
  - Claude/Codex `Stop` → `TurnEnd(payload["last_assistant_message"])`; Gemini `AfterAgent` → `TurnEnd(payload["prompt_response"])`.
  - Claude `Notification` with `notification_type` `permission_prompt` or `elicitation_dialog` → `Permission`; Codex `PermissionRequest` → `Permission`; Gemini `Notification` with `notification_type` `ToolPermission` → `Permission`.
  - Everything else (`idle_prompt`, `Interrupt`, `PostToolUse`, …) → `None`.
- `pub async fn record(d: &Daemon, run: &Run, rest: HookRest) -> Result<()>`: for `TurnEnd(None)` reads `last_message` from the run's log (`summary::locate`); `classify`; writes `review_queue::rest(.., "run", &run.id, kind, how, hints, now, run.started_at, run.start_head)`. `Permission` → `(Asks, Permission, [Permission])`.

- [ ] **Step 1: Write the failing tests.**
  - Unit (`queue.rs`): `hook_rest_maps_each_agent` (a table of `(dialect, event, payload) → expected`, including the five `None` cases above).
  - Integration (`tests/queue.rs`, `Harness`, `stand_in(dir, "claude")` as in `tests/recap.rs`):
    - `a_question_in_the_reply_queues_asks`: `run.hook` `Stop` with `last_assistant_message: "Done.\n\nShould I push?"` → the ledger has the item `Asks`, `Reply`, `[Question]`.
    - `a_stop_with_no_message_reads_the_log`: payload without the field, `transcript_path` to a synthetic JSONL whose last assistant text is "Fixed it." → `Finished`.
    - `a_permission_prompt_queues_asks`.
    - `idle_prompt_queues_nothing`.
    - `codex_late_stop_queues_nothing`: a `codex` stand-in; `UserPromptSubmit` `turn_id: "t2"`, then `Stop` `turn_id: "t1"` → no item; `Stop` `turn_id: "t2"` → an item.
    - `gemini_after_agent_reads_prompt_response`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon --test queue` and `cargo test -p rl-daemon queue`. Expected: FAIL.

- [ ] **Step 3: Implement.**
  - In the handler, after the guards (:213-228) and before `apply` (:265): on the dialect's prompt event remember `payload["turn_id"]` in `d.turns`; for Codex `Stop`, compare and return early (no queue, no `apply`) on a mismatch.
  - After `get_run` (:275), alongside `recap::pick_up`: `if let Some(r) = hook_rest(..) { tokio::spawn(record(d, run, r)) }` — off the hook's clock.

- [ ] **Step 4: Run** the same. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(daemon): turn ends and permission prompts queue the run`

---

### Task 4: The screen, the end, and the start head

**Files:**
- Modify: `code/rl-daemon/src/supervisor.rs`: `start_on` (:241), `consider` (:844), `finish` (:927).
- Test: `code/rl-daemon/tests/queue.rs`.

**Interfaces:**
- Consumes `review_queue::{rest, end, last_rest}`, `runs::set_start_head`.
- `pub const SCREEN_GRACE: SignedDuration = 30s` in `supervisor.rs` (tests may override through a `Supervisor` field `screen_grace`, set by the harness to 300ms).

- [ ] **Step 1: Write the failing tests** (`tests/queue.rs`, `Harness::quick_registry` shell runs, `fake_cli`):
  - `a_shell_that_goes_idle_queues_finished_by_screen`: a shell run prints a prompt and idles → after the grace an item `Finished`, `Screen`.
  - `a_hook_inside_the_grace_wins`: a Claude stand-in goes idle on screen, then `run.hook` `Stop` within the grace → one item, `how == Reply`, `rests == 1`.
  - `a_failed_run_is_queued_failed`: a run exits 3 → item `Finished`, hints `[Failed]`.
  - `a_killed_run_is_stopped`: `run.stop` → hints contain `Stopped`.
  - `start_head_is_kept`: a run in a temp git repo → `runs.start_head` is the repo's HEAD; in a non-repo temp dir → `None`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon --test queue`. Expected: FAIL.

- [ ] **Step 3: Implement.**
  - `start_on`: after `git::head(&cwd)`, `runs::set_start_head` when `Some`.
  - `consider`: when `apply` returns a transition with `source == Source::Pty` and state `NeedsInput` or `Idle`, spawn: sleep the grace; then if `last_rest` shows a rest at or after the transition with `how != Screen`, do nothing; else `rest(.., NeedsInput → Asks / Idle → Finished, Screen, [], transition time, ..)`.
  - `finish`: after `runs::finish`, `review_queue::end(.., Stopped | Failed, ended_at, ..)` (`Finished` and `Stopped` outcomes → `Stopped`; `Failed` → `Failed`).

- [ ] **Step 4: Run** the same and `cargo test -p rl-daemon`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(daemon): screen rests and run ends reach the queue`

---

### Task 5: The report reads the queue; review and item RPCs

**Files:**
- Modify: `code/rl-proto/src/lead.rs` `Lead`: `rest: Option<LeadRest>` (`#[serde(default)]`).
- Modify: `code/rl-proto/src/queue.rs`: `LeadRest`, `ItemView`.
- Modify: `code/rl-proto/src/methods.rs`: `QueueReview` (`"queue.review"`), `QueueItemGet` (`"queue.item"`).
- Modify: `code/rl-daemon/src/report.rs` `project_builtins` (:38); `handlers.rs` report closure (:419-446), `run.input` handler, new handlers.
- Create: `code/rl-daemon/src/reading.rs`: the cached readers.

**Interfaces:**
- `struct LeadRest { item: QueueItem, last: Option<String> /* block-trimmed: first paragraph */, changed: Option<Changed> }`.
- `QueueReviewParams { runs: Vec<String> }` → `QueueReviewed { cleared: u32 }`. Sets `from_head = git::head(run.cwd)`.
- `QueueItemParams { run: String }` → `ItemView { log: Vec<LogLine>, changed: Option<Changed>, branch: Option<String> }` (log since *from*; a shell's log is its `session_io::tail_since(from)` lines as `who: "shell"`).
- `reading::changed(cwd: &Path, from_head: Option<&str>) -> Option<Changed>`: `None` when `from_head` or `git::head(cwd)` is `None`; else `git::delta`, `git::log` subjects, branch, `git::pushed`. Memoised in a `Daemon` map keyed `(cwd, from_head, head)`.
- `reading::last(d, run) -> Option<String>`: `last_message` of the located log, memoised by `(path, len)`; shells: the last non-empty lines of `session_io::tail`.
- Bands: a run lead is `NeedsYou` when its item is `asks`; `Happened` only when it has an item (finished); `Live` when working, as today. An ended run with no item (reviewed) is not in the report. Decisions and firings unchanged.

- [ ] **Step 1: Write the failing tests.**
  - `report.rs` unit: `an_asks_item_puts_the_run_in_needs_you` (an idle run + an `Asks` item), `a_reviewed_finished_run_leaves_the_report`, `a_finished_item_is_happened_with_its_rest`.
  - `reading.rs`: `changed_outside_a_repo_is_none` (temp dir) and `changed_counts_commits_since_from` (temp repo, two commits after `from_head`).
  - Integration `tests/queue.rs`: `review_clears_and_report_drops_it`; `typing_into_a_run_reviews_it` (`run.input` → cleared); `item_view_reads_the_log_since_from`.

- [ ] **Step 2: Run** `cargo test -p rl-daemon report reading` and `--test queue`. Expected: FAIL.

- [ ] **Step 3: Implement.** The report closure reads `review_queue::list` alongside `facts::get_many`, passes the items to `project_builtins` (new parameter `queue: &HashMap<String, QueueItem>`), and fills `LeadRest` per run lead through `reading`. The `run.input` handler calls `review_queue::review` after a successful write.

- [ ] **Step 4: Run** `cargo test -p rl-daemon`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(report): run leads carry their queue item; review and item reads`

---

### Task 6: The inbox model

**Files:**
- Create: `code/rl-cli/src/tui/inbox.rs` (pure); `pub mod inbox;` in `tui/mod.rs`.
- Modify: `code/rl-cli/src/tui/report.rs`: `lane_name` becomes `pub(crate)`; `lanes`/`Lane` go once Task 7 stops using them.

**Interfaces:**
- `pub enum Sel { All, Team(String), Item(String /* lead key */) }`.
- `pub enum Mark { Asks, Failed, Done, Working }` and `pub fn mark(lead: &Lead) -> Mark` (asks: `NeedsYou`; failed: hint `Failed`; working: `Live`; else done).
- `pub struct Team<'a> { name: String, needs: usize, failed: usize, done: usize, working: usize, commits: Option<u32>, gist: String, items: Vec<&'a Lead> }`.
- `pub struct Inbox<'a> { needs: Vec<&'a Lead> /* asks, oldest first */, teams: Vec<Team<'a>>, to_review: usize, working: usize, commits: Option<u32> }` and `pub fn inbox(report: &Report) -> Inbox<'_>`.
  - Teams by `lane_name`; only teams with a queued item, a needs-you lead or a working run.
  - Items: asks → failed → done → working, newest first within.
  - Teams: needs > 0 first, then failed > 0, then to-review > 0, then latest `at`, then name.
  - `commits`: sum over items with `rest.changed`; `None` when none has one.
- `pub fn gist(team_items: &[&Lead]) -> String`: clauses joined by `; `, at most 3: each asks item `"{title} asks"`, each failed `"{title} failed"`, a `Stopped` hint `"{title} stopped"`, then the latest done `"{title} done"`; a done item with `changed` and `!pushed` appends `", not pushed"`. Never the message text.
- `pub fn opening(inbox: &Inbox, away: Option<SignedDuration>, brief_after: SignedDuration) -> Sel`: `away < brief_after` and `needs` non-empty → `Item(first needs key)`; else `All`.
- `pub fn order(inbox: &Inbox, inside: Option<&str>) -> Vec<Sel>`: what `j`/`k` walk. Outside a team: `All`, each needs item, each team. Inside team `t`: its items.

- [ ] **Step 1: Write the failing tests** (`mk_lead` style from `report.rs` tests, with `rest` filled):
  - `teams_sort_by_urgency_then_name`, `needs_strip_lists_asks_across_teams_oldest_first`, `a_team_with_only_reviewed_runs_is_absent`.
  - `gist_never_uses_the_message`: an item whose `last` is "SECRET WORDS" → gist has no "SECRET".
  - `gist_says_not_pushed_only_with_git`: one done item with `changed.pushed == false` → `"migrate auth done, not pushed"`; same item with `changed: None` → `"migrate auth done"`.
  - `commits_are_none_without_any_repo`.
  - `opening_picks_the_first_ask_when_back_soon` and `opening_is_all_teams_after_long_away`.

- [ ] **Step 2: Run** `cargo test -p rl-cli inbox`. Expected: FAIL.

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run** the same. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(report): the inbox model — needs-you strip, teams, gists`

---

### Task 7: Drawing the inbox

**Files:**
- Modify: `code/rl-cli/src/tui/report.rs`: replace `report_view`/`report_lines` with `list_lines` and `reading_lines`.
- Modify: `code/rl-cli/src/tui/mod.rs` `draw_report` (:2793): split the body.

**Interfaces:**
- `pub fn list_lines(inbox: &Inbox, sel: &Sel, inside: Option<&str>, width: usize, now: Timestamp) -> (Vec<Line<'static>>, Option<usize> /* selected line */)`.
  - Outside a team: the *All teams* row (two lines), a `NEEDS YOU · n` heading in `needs()` and one line per ask (`team · title`, hint chip), a `TEAMS · n` heading in `dim()`, then per team three lines: `▸ name`, counts (`n needs you`, `n failed`, `n done`, `n working`, `n commits` — zeros and `None` left out), gist.
  - Inside a team: a `‹ team` line, then per item two lines: mark, title, time (`fmt_when`, or `fmt_age` while working); hint chips, `+X/−Y · N commits` when `changed`, the `last` line trimmed.
- `pub fn reading_lines(report: &Report, inbox: &Inbox, sel: &Sel, view: Option<&ItemView>, briefs: &HashMap<String, Summary>, width: usize, now: Timestamp) -> Vec<Line<'static>>`:
  - `All`: per team a header line (`name` · to review · working · `+X/−Y · N commits` when any), and the gist.
  - `Team`: `SINCE YOU LOOKED` (a templated sentence: "N done, M ask, K working" + totals when git), then `NEEDS YOU`, `DONE`, `WORKING` rows.
  - `Item`: title line (team · title, agent, branch when `Some`, `N turns since HH:MM` when `rests > 1`); `ASKS · HH:MM` box with `last` (block trim; the line with `?` in `name()`), or for a finished item `last` alone; `CHANGED SINCE YOU LOOKED` only when `changed` is `Some`; `DEEP READ` (the kept one from `briefs`, else `not made · p reads the log`); `LOG SINCE YOU LOOKED` from `view` (`HH:MM  who  first line`).
- `draw_report`: width ≥ `INBOX_SPLIT_W` → `Layout::horizontal([Length(list_w), Length(1), Fill(1)])` with a `│` column; else one pane: the reading pane when `report_open` (an item or team opened with `enter`), otherwise the list.

- [ ] **Step 1: Write the failing tests.**
  - `report.rs`: `list_lines_fit_the_width` (every line `chars().count() <= width` at 40 and 52), `no_git_draws_no_git` (an item with `changed: None` → no line contains `+`, `commits`, `CHANGED` or `git`), `inside_a_team_lists_its_items_only`.
  - `mod.rs` panel tests (`draw_text` 120×40, `screen(app, 80, 30)`): `the_inbox_draws_two_panes_when_wide` (seeded `app.report`; frame has `All teams`, `NEEDS YOU`, `TEAMS` and a team's block on the right), `narrow_shows_one_pane_and_enter_opens_the_reading_pane`.

- [ ] **Step 2: Run** `cargo test -p rl-cli report inbox`. Expected: FAIL.

- [ ] **Step 3: Implement.** Remove `report_view`, `report_lines`, `flat_leads`, `Lane`, `lanes` and their tests once nothing calls them; keep `headline` only if Task 9 uses it.

- [ ] **Step 4: Run** `cargo test -p rl-cli`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(report): draw the inbox — list pane and reading pane`

---

### Task 8: Keys and actions

**Files:**
- Modify: `code/rl-cli/src/tui/keys.rs` (report letters :348-367, `key_list` :580-595/:636, `enter_row` :686-690), `feed_ops.rs` (`report_step`, `act_selected`, `fetch_report`), `mod.rs` (App fields, `Mode::Reply`, footer, attach loop :1396-1418), `shortcuts.rs` (`Place::Report`).

**Interfaces:**
- App fields replace `report_sel`: `inbox_sel: inbox::Sel`, `inbox_inside: Option<String>`, `report_open: bool`, `item_view: Option<(String, ItemView)>`.
- `Mode::Reply { run: String, field: line::Field }`; submit → `RunInput { id, bytes: text + "\r" }`, then `QueueReview`.
- Keys while the report is up, the line empty, no modifiers, no subpage:

  | Key | Team / All teams | Item |
  |---|---|---|
  | `j` `k` (and ↓ ↑) | step along `inbox::order` | same |
  | `enter` | step into the team (`inbox_inside`), select its first item | attach (`self.attach = Some((id,false))`); a decision lead keeps today's `act_selected` |
  | `esc` | from a team → *All teams* list; at the top → back to the list view (today) | back to the team / strip |
  | `r` | — | `Mode::Reply` (agent and shell runs) |
  | `x` | `QueueReview` for every queued run in the team | `QueueReview` (or `LeadDismiss` for a lead with no item), then select the next in `order` |
  | `p` | flash `open an item to read it deeply` | today's deep summary path with `SummaryTarget::Run`, `from: Some(item.from)` |
  | `w` | toggle the written summary | same |
  | `t` | tag the team's source (today's `tag_prompt`) | tag the item's source |

- Selecting an item fetches `QueueItemGet` once per selection (kept in `item_view`); it never reviews.
- Attach from anywhere in the TUI (the main loop's `attach` handler) sends `QueueReview { runs: [id] }` before handing the terminal over.
- `fetch_report` keeps `inbox_sel` when it still exists; a vanished item → the next in `order`, else its team, else `All`. On open, `inbox_sel = inbox::opening(..)`.

- [ ] **Step 1: Write the failing tests** (panel tests in `mod.rs`, seeded `app.report`):
  - `j_and_k_walk_the_strip_and_teams`; `enter_steps_into_a_team_and_esc_comes_back`.
  - `x_on_an_item_reviews_it_and_moves_on` (real daemon via `panel_with_runs`, a queued item through `run.hook`; after `x`, `review_queue::list` is empty and the selection is the next one).
  - `the_refresh_keeps_the_selection` (select an item inside a team, call `fetch_report` twice → same `inbox_sel`; remove the item from the report → the next item).
  - `r_sends_the_reply_and_reviews` (a shell run; type `r`, `echo hi`, enter → the run's output has `hi`, the item is cleared).
  - `typing_letters_outside_the_report_still_types` (in the list view `j` goes to the line).

- [ ] **Step 2: Run** `cargo test -p rl-cli tui::tests`. Expected: FAIL.

- [ ] **Step 3: Implement.** Footer `Place::Report` shows `j/k move · enter open/attach · r reply · x reviewed · p read deeply · w written · esc back`.

- [ ] **Step 4: Run** `cargo test -p rl-cli`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `feat(report): inbox keys — step in, attach, reply, mark reviewed`

---

### Task 9: The written summary, and step 4 retired

**Files:**
- Modify: `code/rl-cli/src/tui/prose.rs`: `prose(report, away)` walks the inbox.
- Modify: `code/rl-cli/src/main.rs`: `rl report` (:521, :1081-1173, :1887).
- Delete: `code/rl-cli/src/depth.rs` (move `ASK_OVER` to `code/rl-cli/src/cost.rs`, used by `summary_ops.rs:178` and `main.rs`).
- Modify: `tui/mod.rs` (`Mode::ReportCost`, `Briefing`, `Briefed`, `BriefJob`, App fields `report_prose`/`briefing`/`report_ask`/`report_said`, the select arm :1462-1470, footer :3061), `feed_ops.rs` (`brief_report`, `start_briefing`, `briefed`, `stop_briefing`, `brief`), `keys.rs:155-162, 351-356`.
- Modify: `code/rl-proto/src/config.rs` `ReportConfig`: drop `deep_after`, `summary`, `auto_summary()`; `code/rl-daemon/src/config.rs:59-62, 329-330`; `rl-cli/src/context.rs:142`.

**Interfaces:**
- `pub fn prose(report: &Report, away: Option<SignedDuration>) -> String`: an opening sentence ("While you were out (14h): 3 need you, 1 failed, 9 to review.", commits only when `Some`), a *Needs you* paragraph per ask (title, team, how it asks, waiting since), then per team a paragraph from its items (title, done/failed/stopped, `+X/−Y · N commits` only with `changed`, the `last` line).
- `briefs` stays on App: it now holds deep reads made by `p` (`made`/`summarised` feed it).

- [ ] **Step 1: Write the failing tests.** `prose_walks_needs_then_teams`, `prose_without_git_has_no_commit_words`, and in `main.rs` tests (if any exist for `rl report`) `rl_report_prints_the_written_summary`. Delete the step-4 tests listed in the retired code (mod.rs 5065, 5172–5396, 5551 and helpers 5128–5157; feed_ops 402, 455; config 362–371).

- [ ] **Step 2: Run** `cargo test -p rl-cli prose`. Expected: FAIL.

- [ ] **Step 3: Implement**, then delete the retired code; `grep -rn "ReportCost\|BriefJob\|deep_after\|auto_summary\|report_prose" code/` returns nothing.

- [ ] **Step 4: Run** `cargo test --workspace`. Expected: PASS.

- [ ] **Step 5: Gates, commit.** `refactor(report): the written summary walks the queue; retire generate-on-open`

---

### Task 10: Look at it (controller, after Task 9)

- [ ] Build, start `rld` with `RELAY_HOME` set to a scratch home, start a Claude run and a shell run in a non-git folder, let both rest, open `/report` at 120 and 80 columns. Check against artboards 4–6 and the Review Focus list; fix what differs in a follow-up commit before the final review.
