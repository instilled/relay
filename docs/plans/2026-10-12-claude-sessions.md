# Claude's own sessions on the list: implementation plan

> **For agentic workers:** use superpowers:subagent-driven-development, one task at a time.

**Goal:** every Claude session that `claude agents --json` lists and that isn't a Relay run gets a row under its folder's project, with its state. A background session can be joined, which hands the terminal to `claude attach`. An interactive one, open in another terminal, can be resumed here.

**Roadmap item** (on `chore/roadmap`): "agents started outside Relay, Claude's first".

**Spec:** this plan stands as the spec. Its decisions come from the user in chat, 2026-10-12:
- Show Claude's background sessions and interactive ones.
- Attach to background sessions.
- Interactive ones: "we can also enter then /resume".
- Codex comes later. Gemini is not planned.

## What is known (verified 2026-10-12, Claude Code 2.1.296)

- `claude agents --json` takes about 0.13s and needs no TTY. It prints active sessions, both interactive and background, as a JSON array. The fields are:
  - always: `cwd`, `kind` (`interactive`|`background`), `startedAt` (unix ms) and `sessionId`
  - background only: `id` (short) and `state` (`working`|`blocked`|`done`|`failed`|`stopped`)
  - while the process lives: `pid` and `status` (`busy`|`waiting`|`idle`)
  - when `status` is `waiting`: `waitingFor` (`permission prompt`|`input needed`|`sandbox request`|`worker request`|`dialog open`)
  - optionally: `name`
- `claude attach <id>` opens a background session full-screen in this terminal. Ctrl+Z hands the terminal back. `←` goes to Claude's agent view, which you then quit. Either way the session keeps running. It works only for background sessions, and an interactive one has no `id`.
- `claude --resume <sessionId>` carries a conversation on in a new process.
- `/background` in a session starts a new supervisor-hosted process that resumes the conversation. `--settings` carries over. `RELAY_RUN` probably does not, and `rl hook` without `RELAY_RUN` does nothing (`code/rl-cli/src/main.rs:565`). Whether the `sessionId` stays the same is not documented. The last task checks it.
- Relay already has `code/rl-daemon/src/import.rs` (#110), with these pieces:
  - `parse` turns `claude agents --json` into `rl_proto::methods::ClaudeSession`.
  - `ask` runs `claude` with a timeout.
  - `move_one` stops a background session and resumes it as a Relay run, tagged `claude-session:<sessionId>` (`TAG`).
  - The panel uses it through `/import` and the setup guide.

## Global constraints

- Never touch `~/.relay`. Test homes are tempdirs or `RELAY_HOME=/tmp/<unique>`.
- Never run a real agent or the real `claude` in tests. Use a fake `claude` script in a tempdir, the way `code/fixtures/claude` is used. Check how existing import and setup tests fake it.
- Gates:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo build --workspace`
  - `cargo test --workspace`
- Clean code in the surrounding style. New logic goes in its own modules, not in `tui/mod.rs`, beyond the wiring.
- Don't chain git with other commands. Never use a bare `git stash`.
- Commit messages: one plain sentence in the repo's style, a blank line, then `Claude-Session: https://claude.ai/code/session_01J28o6ipYyGjMabKudJsm1y`.
- Words: the row and pane call these sessions **Claude's**, as in "Claude's · background" and "Claude's · in another terminal". A Relay run is never called Claude's.

## Review focus

1. `claude` missing, failing or slow: no rows, no error spam, the panel never waits on it. `ask` already has a timeout, and polling must never block a digest.
2. A Relay-started Claude run must never also show as a Claude row. Duplicates are the worst failure here.
3. Garbage or unknown fields or values in the JSON (a new `state`, a missing `sessionId`): skip or tolerate them, never fail the whole list.
4. Hand-over: after `claude attach` exits by any route, including a failure to start, the panel comes back intact. The same holds for the editor hand-over.
5. A session whose `cwd` is not a registered project, or is a worktree: it groups the way a Relay run in that folder would.

---

### Task 1: The daemon keeps Claude's sessions, and Relay's own are told apart

**Files:** `code/rl-daemon/src/import.rs`. Split it if it reads better: for example, the listing and parsing go to `code/rl-daemon/src/claude_sessions.rs`, and `import.rs` keeps the move. Also touched:
- the server/Daemon state
- `code/rl-daemon/src/handlers.rs` (digest)
- `code/rl-proto/src/methods.rs` (`Digest`, `ClaudeSession`)
- the hook handling, wherever `rl hook` events land in the daemon

**What it does:**
- **Polling.** A daemon task polls `claude agents --json` every 5s, but only while the claude profile's binary exists. It keeps the latest list in memory. A failure keeps the last good list for up to 30s, then clears it, and logs once per change of error, at `debug!`/`warn!` as for other polls. The digest never runs `claude` itself.
- **Fields.** `ClaudeSession` gains the fields the list needs, each `#[serde(default)]` so old clients still read the digest: `pid: Option<u32>`, `started_at: Option<Timestamp>`, `state` (already there), `status` (already there), and `waiting_for: Option<String>`. Unknown values stay strings, read as they come.
- **Relay's own runs are dropped.** When Claude's `SessionStart` hook reaches the daemon for a run, which has `RELAY_RUN`, the daemon reads `session_id` from the payload and tags the run `claude-session:<session_id>` (import's `TAG`) if it isn't tagged already. A listed session is then Relay's own, and dropped, if:
  - a run carries that tag, live or ended, archived or not, or
  - its `pid` is the agent process of a live Relay run.

  Use the pid only if the daemon already knows its runs' child pids. Look at `supervisor.cwds()`, which reads the child's cwd. If it doesn't, the tag alone is enough.
- **Digest.** `Digest` gains `claude: Vec<ClaudeSession>`, with serde default and skipped when empty. It holds the sessions that are not Relay's.
- **Events.** When the list changes (sessions appear, go, or change state), the daemon emits whatever event makes an open panel refresh. Use the same mechanism run state changes use.

**Tests:**
- Parsing the new fields, including an unknown `state`, and an entry missing `sessionId`, which is skipped.
- `SessionStart` with a session id tags the run once.
- The filter drops a tagged session and keeps an untagged one.
- With a fake `claude` that prints two sessions, the digest carries the untagged one after a poll.
- With a fake `claude` that fails, the digest carries none after the grace period and the daemon stays up.

### Task 2: Claude's sessions are rows on the list, with a pane

**Files:** `code/rl-cli/src/tui/rows.rs` (a new `Row::Claude(&ClaudeSession)`, grouping, rank, lens, search, hide), a new `code/rl-cli/src/tui/claude.rs` for the words (like `tui/flows.rs`), `tui/pane.rs`, the drawing arm in `tui/mod.rs`, and `catchup.rs` if it lists rows.

**What a row is:**
- **Group.** It stands under its `cwd`'s project, grouped as a Relay run in that folder would be: through `Names`, by ref or registered folder or worktree, the same `place_key` rules. Read how `group_of` handles a plain `Run` and give `ClaudeSession` the same treatment from its `cwd`. A folder that isn't a project gets a group as an unregistered run's folder would.
- **Icon and title.** Claude's agent glyph and colour, from the claude profile as agent rows have it (#152). The title is the session's `name`.
- **State words**, in the column runs use:
  - background: `working`, `needs you · permission prompt` (`blocked`, with `waitingFor` when there is one), `done`, `failed`, `stopped`
  - interactive: `working` (busy), `needs you · <waitingFor>` (waiting), `idle`
- **The summary column** says where it lives: `Claude's · background` or `Claude's · in another terminal`.
- **The time column** is `fmt_age(started_at)`.
- **Rank.** It ranks with runs, as a run in the matching state would (`rank::score`'s order): needs-you first, working, then the rest. Write a small mapping to a `RunState`, used only for ranking and lens, and say so in a comment.
- **Headline.** Counts include Claude's sessions: one blocked counts as needing you, one working as working.
- **Lenses.** `All` and `Agents` show them. `Flows` does not.
- **Hiding.** `/hide` on one stores key `c:<sessionId>` (`digest.hide` takes any key, so check). The hidden lens shows it. Hiding its project hides it too.
- **Search.** It hits on name, project and folder, with `kind:agent`, `kind:claude`, and `is:needs`/`is:going`/`is:done` as for runs.
- **Pane.** The pane for a selected row says:
  - its name
  - `Claude's · background` or `Claude's · in another terminal`
  - its state with `waitingFor`
  - its folder
  - when it started
  - its session id
  - what you can do: `enter: open it here (claude attach)` for background, `enter: carry it on here (claude --resume)` for interactive

**Tests:** in rows.rs and the panel tests, matching how runless flow rows were tested (#160):
- grouping under a registered project and under an unregistered folder
- lens `All`/`Agents`/`Flows`
- rank: blocked above working above done
- the headline count
- hide and the hidden lens
- search `kind:claude`
- drawing the panel and opening the pane with one of each kind, without a panic

### Task 3: Enter on a Claude row: attach or carry on

**Files:** `code/rl-cli/src/tui/mod.rs` (the wiring next to `self.attach` and the editor hand-over, around line 1250), `code/rl-cli/src/tui/claude.rs`, `code/rl-daemon/src/import.rs` (`move_one`), and the palette or help words.

**Background session, enter or join:**
- The panel hands over the real terminal, the same hand-over the editor uses (`leave_screen`, run the child with inherited stdio, `enter_screen`, `refresh`). It runs `claude attach <id>`, using the claude profile's binary as `import::claude_bin` finds it.
- On return, whatever the exit, the list comes back and the row reflects the new state on the next poll. A non-zero exit says `claude attach: <first stderr line or exit code>` via `self.say`.
- Nothing is created in the ledger.

**Interactive session, enter:**
- It is carried on as a Relay run. Extend `import::move_one`:
  - An interactive session with `status` `idle` is no longer refused. It cannot be stopped from here (no `id`), so `move_one` resumes it with `claude --resume <sessionId>` as today, and the panel says `<name> carries on here; it is still open in its own terminal — leave that one`.
  - A `busy` or `waiting` interactive session is refused with `<name> is working in its terminal; /bg there to open it here, or wait for it to finish`.
- Selecting the new run after the move works as it does after `/import` today.

**`/import <name>`** keeps its meaning (stop and take over) for background sessions.

**Tests:**
- A fake `claude` script records its argv into a file. Enter on a background row runs `claude attach <id>`. Test the command line it builds, not a real TTY hand-over; factor the argv out so it can be tested.
- `move_one` on an idle interactive session starts `--resume <sessionId>`, and on a busy one returns the refusal.
- Existing import tests still pass.

### Task 4: A Relay run sent to Claude's background points to where it went

**Files:**
- the daemon (whatever reads the hook tag)
- `tui/claude.rs` and rows.rs
- `docs/ARCHITECTURE.md`
- `README.md`, the "not built yet" line, if needed

**What it does:**
- If a listed **background** session's `sessionId` is the `claude-session:` tag of a Relay run, it is no longer dropped as Relay's own **when that run has ended**. It shows as a Claude row whose summary says `Claude's · background · was <run title>`. The ended run's own summary says `went to Claude's background`.
- If the tagged run is still live, the session stays dropped: the run still has it.
- This rests on `/background` keeping the session id. If a manual check, done after merge by the user, shows the id changes, this task's link is dead code. So keep it small, and name the assumption in a comment and in ARCHITECTURE.
- ARCHITECTURE says:
  - where Claude's sessions come from
  - how Relay's own are told apart
  - what enter does for each kind
  - that polling happens only while `claude` exists

**Tests:** an ended tagged run plus a background session with the same id gives one Claude row with `was <title>`. A live tagged run gives no Claude row.

---

## Addendum (2026-10-13): generic, with Codex and Gemini

The user asked for this in chat: "generic, gemini and codex support to, please". Tasks 1–2 shipped the shape for Claude. Task 2a makes it generic. Tasks 2b and 2c add Codex and Gemini as sources. Task 3 (enter) and Task 4 (the `/background` link) then work across agents. Task 3 is also renamed: join and carry-on come from the agent's profile.

Hook facts, checked against the docs and source on 2026-10-13. The full notes with URLs are in `docs/plans/2026-10-13-agent-hooks-notes.md`, and they are summarised here:

- **Codex:**
  - **Where hooks live:** user hooks go in `$CODEX_HOME/hooks.json` (default `~/.codex`), or inline `[[hooks.<Event>]]` in `config.toml`. Project and user hooks add together.
  - **Events and payload:** `SessionStart`, `UserPromptSubmit` (working), `PermissionRequest` (waiting for approval), `Stop` (turn done, idle) and `SessionEnd`. Stdin always carries `session_id`, `transcript_path`, `cwd`, `hook_event_name` and `model`.
  - **How a hook runs:** it is spawned as `sh -lc` in a new session, so `getppid()` is not the agent. It inherits codex's env, except some auth vars.
  - **Not blocking:** exit 0 with empty stdout is a no-op; never exit 2. Use `timeout: 3` (seconds) and `async: true` where the schema has it.
  - **Trust:** the user must trust a new user-level hook once in codex's `/hooks` screen before it runs.
  - **Resume:** `codex resume <session_id>`, which Relay already uses.
- **Gemini:**
  - **Where hooks live:** user hooks go in `~/.gemini/settings.json` (`GEMINI_CLI_HOME` moves the home), shaped as `hooks.<Event>: [{matcher, hooks: [{type: "command", command, timeout}]}]`, with `timeout` in ms. Layers merge, and user hooks need no trust prompt.
  - **Events:** `SessionStart` (`source`), `BeforeAgent` (working), `Notification` with `notification_type: ToolPermission` (waiting), `AfterAgent` (turn done) and `SessionEnd`. Stdin carries `session_id`, `transcript_path`, `cwd`, `hook_event_name` and `timestamp`. The env also has `GEMINI_SESSION_ID` and `GEMINI_CWD`.
  - **Hooks are synchronous:** they block the agent, so use `timeout: 2000`, and exit 0 with empty stdout.
  - **Resume:** `gemini --resume <session_id>`, run in the session's `cwd`.
- **Env:** both pass the agent's env to the hook, so `RELAY_RUN` is visible for runs Relay started.

### Task 2a: one shape for every agent's own sessions

**Rename and generalise. Behaviour is unchanged for Claude.**
- **Protocol:** `ClaudeSession` becomes `rl_proto::methods::Outside`, a session an agent CLI runs that is not a Relay run.
  - It gains `agent: String`, the profile name: `claude`, `codex` or `gemini`.
  - `id` (the attach id) is still Claude-only.
  - Keep a serde alias or the old field names wherever the import RPC needs them. The import screen keeps working.
- **Digest:** `Digest.claude` becomes `Digest.outside`.
- **Daemon:** `claude_sessions.rs` becomes `code/rl-daemon/src/outside/`:
  - `mod.rs`: the shared store, the Relay-own filter by `<agent>-session:<id>` tag or pid, and the digest feed
  - `claude.rs`: today's poller
  - The tag prefix becomes per agent: `claude-session:` stays as is, and `codex-session:` and `gemini-session:` are added. Import's `TAG` keeps its value.
- **Panel:**
  - `Row::Claude` becomes `Row::Outside`, and `tui/claude.rs` becomes `tui/outside.rs`.
  - The glyph and colour come from the session's agent profile.
  - The summary says `<Agent>'s · background` or `<Agent>'s · in another terminal`, with the agent's display name taken from its profile, or its name capitalised.
  - Search gets `kind:<agent>`.
- **Hooks into Relay:** `rl hook` gains an agent argument: `rl hook <agent> <Event>`. The existing Claude settings file keeps working: either it keeps calling `rl hook <Event>` and that means `claude`, or `rld` rewrites the file on start. Pick whichever is simpler and say which.

- **Grouping like runs (from Task 2's review, S1).** The daemon resolves each session's group folder once per session, on the poll or hook path and never on the digest path. It uses the rules `supervisor.rs:265-280` applies to a run: canonicalize, then the registered folder, then the git toplevel, then cwd. The result is carried as `workspace: Option<PathBuf>` on `Outside`. The panel groups by it in place of its own deepest-registered-folder guess, and keeps that guess only as a fallback when `workspace` is `None`.
- **Hidden sessions come through (from S2).** The daemon keeps `state_at: Option<Timestamp>`, the time it first saw the session's current state, on `Outside`. A hidden session, by itself or through its project, comes through to the list when it starts needing you after it was hidden, exactly as `comes_through` does for runs.
- **Keys.** Hide and order keys become `o:<agent>:<session_id>`, replacing `c:<session_id>`. Nothing is released, so no migration is needed. Change `Hidden::valid_key`, `Order::valid_row` and the daemon's error text together.

**Tests:** the existing Claude tests pass after the rename. One test covers a non-Claude session in the digest rendering with that agent's glyph. Further tests: a session in an unregistered repo's subfolder groups under the repo root, as a run there would, and a hidden session that starts needing you comes through.

### Task 2b: Codex and Gemini sessions, learned from user-level hooks

**What it does:**
- **Install with a yes.** `rl setup`, both the CLI and the setup guide in the panel, offers per agent whose binary exists: "Show Codex sessions started outside Relay? This adds a hook to ~/.codex/hooks.json". Only on a yes does it write the hook.
  - **Codex:** append one entry per event to `$CODEX_HOME/hooks.json`, creating the file if needed. Never remove or reorder what is there. The command is `<rl> hook codex <Event>`, with a 3s timeout, async if the schema allows. Then say: "codex asks you to trust it once: open codex, /hooks".
  - **Gemini:** merge into `~/.gemini/settings.json` under `hooks.<Event>`, keeping every other key and hook. Mark the entry so a second install recognises it and doesn't duplicate it: the command path containing `rl hook gemini` is the marker. Use a 2000ms timeout.
  - **Uninstall.** Removing Relay's entries (`rl setup` toggled off) touches only entries whose command is Relay's.
  - **Don't break the user's settings.** A file that doesn't parse is never rewritten: say so and stop.
- **`rl hook <agent> <Event>`** reads stdin JSON.
  - With `RELAY_RUN` set: it is Relay's own run. Post it as today's Claude hooks do, so these runs get exact signals. On `SessionStart`, tag the run `<agent>-session:<session_id>`.
  - Without `RELAY_RUN`: post an outside-session event `{agent, session_id, cwd, event, detail}` to the daemon.
  - It always exits 0 with empty stdout, fast. If the daemon is not up, give up within the timeout and never start the daemon from a hook.
  - It finds the agent's pid by walking up its own ancestors, through `ps` or libproc/procfs as the code already does elsewhere, to the first process whose command line names the agent's binary. It sends that pid when found.
- **The daemon keeps these sessions in the same store as Claude's**, keyed `<agent>:<session_id>`:
  - `SessionStart`: idle
  - `UserPromptSubmit` / `BeforeAgent`: working
  - `PermissionRequest` / `Notification(ToolPermission)`: needs you · permission
  - `Stop` / `AfterAgent`: idle
  - `SessionEnd`: removed

  All of these are interactive (kind `interactive`, no attach `id`). An entry goes when its pid is known and the process is gone, checked on each poll tick, or after 12h with no event when no pid is known. Relay's own runs are filtered as in Task 2a.
- **Profiles.** `detect/codex.yaml` and `detect/gemini.yaml` lose "No hook API yet". Their PTY patterns stay as the fallback when the hook isn't installed. If a profile field is the cleanest way to give the resume argv shape, add it as `resume: ["resume", "{session}"]` for codex and `["--resume", "{session}"]` for gemini, and use it in `resume.rs` in place of the hard-coded `Agent` match, with Claude's `["--resume", "{session}"]` in claude.yaml. Otherwise extend the `Agent` enum with Gemini.

**Tests**, all with fake `CODEX_HOME` / `GEMINI_CLI_HOME` / `HOME` tempdirs, never the real ones:
- install into an empty file, into an existing file with other hooks (kept), and twice (no duplicate)
- uninstall keeps others
- a malformed file is refused
- `rl hook codex PermissionRequest` without `RELAY_RUN` puts a codex session needing you into the digest; `SessionEnd` removes it
- with `RELAY_RUN`, the run is tagged and no outside session appears
- a dead pid is dropped
- the ancestor walk finds the agent in a fake process chain, if factored to be testable

### Task 3 (generalised): enter on an outside row

As the original Task 3, but per agent:
- **Attach** exists only where the session has an attach `id` (Claude background): `claude attach <id>`.
- **Carry on** applies to an interactive session of any agent: the resume argv from the profile (Task 2b), started in the session's `cwd` as a Relay run tagged `<agent>-session:<id>`.
  - Idle sessions carry on, with the note about the other terminal.
  - Busy ones are refused, with the per-agent wording: Claude's mentions `/bg`, the others say "wait for it to finish or close it there".
- **`import::move_one`** generalises or delegates: background Claude sessions are still stopped first.
- **The pane hint** says `enter: carry it on here (codex resume)` and the like.

---

## Addendum 2 (2026-10-13): dead cheap, one way for every agent

The user asked in chat: "colud we make it dead cheap and same behaviour for gemni and codex?". Measured on this machine:
- `claude agents --json` costs 0.13s, ~0.12s CPU and a 160 MB Node start per call. Asked every 5s while a panel shows the list, that is 2–3% of a core.
- `rl hook` costs under 10ms. `ps -A` adds ~30ms, and today it runs on every prompt (`asks_pid`).

So Claude's sessions become told by a user-level hook, as Codex's and Gemini's are. `claude agents` is asked only when there is a reason. Nothing polls on a timer except a cheap fallback.

### Task 5: Claude's sessions told by hooks; `claude agents` only with a reason

**Install, with a yes.**
- **Where:** `rl setup` (the CLI and the panel's setup guide) offers Claude as it offers Codex and Gemini. The prompt is "Show Claude sessions started outside Relay? This adds a hook to ~/.claude/settings.json".
  - The file is `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
  - It is the same shape as Relay's per-run settings: `hooks.<Event>: [{hooks: [{type: "command", command, timeout}]}]`, with `timeout` in seconds (5).
  - It merges as Gemini's does: other keys and hooks are kept, the hook is never duplicated, and a file that doesn't parse is refused. Uninstall removes only Relay's entries.
- **Events:** `SessionStart`, `UserPromptSubmit`, `Notification`, `PostToolUse`, `Stop`, `SessionEnd`.
- **Command:** `'<rl>' hook claude <Event> --user || true`.
  - **Why the marker:** Relay's own Claude runs already call `rl hook claude <Event>` from the per-run `--settings` file, and Claude runs both. The trailing `--user` tells `rl hook` this is the user-level copy.
  - **Older `rl`:** an older `rl` refuses words it doesn't know (`words` returns None), so it does nothing and exits 0.
  - **Same-file check:** `is_relays` must recognise this form for `claude`. The rule that `rl` comes from PATH only when it is the same file as `current_exe` applies here too.
- **Message after install:** "Claude picks it up in sessions started from now on; ones already open may ask you to review it in /hooks".

**`rl hook claude <Event> --user`:**
- **Under `RELAY_RUN`:**
  - If the run is Claude's own, it does nothing: the per-run hook already said it. It never posts `run.hook` from the user-level copy.
  - Only if the run is foreign (`user_hooks::foreign`, e.g. a claude started in a Relay shell run) does it tell `rld` of an outside session. Decide this in `rld`: `OutsideHookParams` gains `run: Option<String>`, and `rld` ignores the word when that run is a non-foreign run of this agent. That keeps the hook to one RPC.
- **Without `RELAY_RUN`:** it posts `outside.hook` as Codex and Gemini do.
- **How the events read** (`user_hooks::told` for `claude`):
  - `SessionStart` → idle
  - `UserPromptSubmit` → busy
  - `Notification` with `notification_type` `permission_prompt` → waiting, `permission prompt`
  - `Notification` with `idle_prompt` → idle
  - `PostToolUse` → carries on (as Codex's)
  - `Stop` → idle
  - `SessionEnd` → ended
- **Agent lists:** Claude joins `user_hooks::AGENTS`. Check every user: `handlers.rs:191`, `outside/mod.rs:144`, `hook.rs`, `setup.rs:78`. Relay's own runs keep their per-run hooks unchanged.

**One store for Claude's sessions: told by hooks, completed by asking.** In `rld`, Claude's sessions come from the hooked store as Codex's and Gemini's do. `claude agents --json` is kept to fill in what a hook cannot say: whether a session is in Claude's background, its attach `id`, its `name`, its `pid` and `startedAt`. It also finds sessions that started before the hook was installed or before `rld` started.

It is asked only:
1. **When the list is first wanted:** the first panel digest since `rld` started, or since the list last stopped showing for over `SHOWING`.
2. **When a hook tells of a Claude session the last ask didn't list.** Asks are coalesced, at most one per 3s.
3. **While the panel shows and a Claude background session is on the list:** once every 60s. Whether Claude runs user hooks inside background sessions is not documented; this is the safety net, and it goes on the manual-check list.
4. **As a fallback when Claude's user-level hook is not installed:** every 30s while the panel shows. Read "installed" from the file at most once a minute (`user_hooks::installed`), never per tick.

**Merging the two:**
- An ask's entry fills in `id`, `kind`, `name`, `pid` and `started_at` on the hooked session with the same key.
- Its state words are taken only if no hook has been heard for that session since the ask began.
- An asked-only session is kept as told-by-nothing, and goes when its pid dies (`hooked::alive`) or a later ask doesn't list it.
- A `SessionEnd` removes it from both.
- Pruning stays per tick and costs a `kill(pid, 0)` per session, nothing more.
- With `RELAY_CLAUDE_SESSIONS=off`, `claude` is never asked. Hooks still tell.

**Docs:** update the module docs (`outside/mod.rs`, `outside/claude.rs`, `outside/hooked.rs`, `user_hooks.rs`, `hook.rs`) and `docs/ARCHITECTURE.md` to say Claude's sessions are told by hooks, and when `claude agents` is asked.

**Tests** use temp `HOME` and `CLAUDE_CONFIG_DIR`, and a fake `claude` that counts its calls. No real agent runs.
- **Install:** into an empty file and into one with other hooks and keys (kept, order kept); twice is not duplicated; a malformed file is refused; uninstall keeps the others; `is_relays` knows the `--user` form.
- **`rl hook claude Stop --user`:**
  - Without `RELAY_RUN`, it puts an idle Claude session into the digest, and `SessionEnd` removes it.
  - With `RELAY_RUN` of a Relay Claude run, nothing appears and the run's state isn't touched by it.
  - With a shell run's `RELAY_RUN`, an outside session appears.
- **When `claude` is asked:**
  - With the hook installed and no background session, ten digests over 50s ask `claude` once.
  - A hook about an unknown Claude session asks once more.
  - A background session listed makes it ask about once a minute while showing.
  - Without the hook installed, it asks about every 30s while showing.
  - Use a short-interval `Polling` in tests, as the existing ones do.
- **Merging:** an ask fills in `id` and `kind` on a hooked session, and a hook heard after the ask began keeps its own state.

### Task 6: the hook looks for the agent's process once per session; only the panel keeps the list showing

- **`ps` once per session.** `outside.hook`'s result becomes `OutsideHookResult { wants_pid: bool }`: true when `rld` knows no pid for that session.
  - `rl hook` sends without a pid first. Only when `wants_pid` is true and the event is one `asks_pid` allows does it walk `ps` and send once more, with the pid.
  - Everything stays inside `WITHIN`.
  - An older `rld` that returns `null` reads as `wants_pid: false`.
- **Only the panel keeps the list showing.** `DigestParams` gains `#[serde(default)] watching: bool`.
  - The panel sets it, and only a watching digest counts as the list showing (`Outsiders::latest` takes the flag).
  - The catch-up command (`main.rs:910`) and any other one-off digest leave it false, so they read what is kept without making `rld` ask anything.
  - A one-off digest made while `rld` has never asked may show no Claude sessions; that is fine.
- **Tests:**
  - A second `PermissionRequest` for a session with a known pid doesn't run `ps`. Factor the decision so it can be tested without `ps`.
  - A digest with `watching: false` doesn't wake the asker.
