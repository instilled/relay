# Relay Product Roadmap

Goal for v1 (2026-11-07): **`claude agents`, but for every harness, plus shells, plus messaging
between agents.** One list in the terminal you already use of every agent session — Claude's and
Codex's own background sessions shown 1:1 as their agents views show them, Relay-held sessions for
harnesses without a background mode and for shells, agents in other terminals from hooks — attach
full-screen, ctrl-\ back to the list. No daemon: `rl`, `rl-session` holders and plain files.
Spec: `docs/specs/2026-11-07-slim-relay.md`.

The earlier v1 goals (flows for Notion feedback and GCP logs) and everything open below them — the
report arc, the briefer, the co-worker layer, notifications, the pane redesign — are dropped.

## Open

What the slim build left to check or finish:

* [ ] verify live what no test could: what `claude --bg` prints and the carry-on of an idle
  Claude (`--resume --bg`), `claude stop`, `codex resume <id>` attaching to the right thread,
  `codex queue --resume` reaching a Relay-held Codex, `claude --bg` sessions running user hooks,
  Gemini's MCP format, and Codex's `env_vars`
* [ ] the list doesn't restore the terminal on SIGTERM — a killed list leaves raw mode and the
  alternate screen behind
* [ ] Antigravity hooks — its hook system (`hooks.json`; `SessionStart`, `PreToolUse`,
  `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop`) differs from the Gemini CLI's and has no
  permission event, so its rows can't read "needs input" yet
* [ ] a Codex session handed to Codex's own daemon on detach — not today: codex 0.167 unloads a
  thread ~70 s after its TUI exits; revisit when Codex keeps threads loaded
* [ ] `inbox` leaves a hidden claimed file when a process dies between claim and delete; sweep
  stale `.taken-<pid>` files whose pid is dead

Later:

* [ ] peek and reply from the list without attaching (as `claude agents`' space)
* [ ] socket paths that can't overflow macOS's ~104 bytes, if short names don't suffice

## Done — newest first

* [x] relay mail pings — `send` to a Codex session also runs `codex queue … --resume`; a Claude
  session hears of waiting mail on its next prompt and is kept going one turn at stop (once per
  batch, never with `stop_hook_active`); Claude→Claude sends suggest `SendMessage`; MCP kept as the
  transport because it runs outside the agents' sandboxes (#205)
* [x] `/` runs commands on the list (`/colour`, `/rename`, `/pin`, `/hide`, `/stop`, `/rm`,
  `/agents`, `/shells`, `/hidden`, `/group`, `/help`, `/quit`); search on `ctrl+f`; help as a grid of
  keys like `claude agents`; keys written one way, `ctrl+x` (#204)
* [x] row colours as `claude agents` draws them (a highlighted name); your own colours on any row —
  `ctrl+g`, `rl color <session> <name|#rrggbb|none>`, `config.json` `list.colors`, the harness's
  colour first; the line's prompt is just `❯`; held sessions get `RL_SESSION` / `RL_SESSION_AGENT`
  (#203)

* [x] the list's command line — typing at the bottom starts a session in the selected project's
  folder: `claude …` a Claude background session, `codex …` a Relay-held Codex, `gemini`/`agy`/
  `antigravity …` Antigravity's `agy`, anything else a shell command in a new Relay shell; the
  right end shows what was detected; Tab completes harness words, commands and paths; search;
  filters on `ctrl+a`/`ctrl+l`/`ctrl+o` so every letter types; filters and grouping in the header; worktree
  sessions stand under their parent checkout; Claude sessions keep their colour (#201)
* [x] slim · steps 2–6 — `code/rl`, daemonless (the file store, `rl new | ls | attach | stop | rm`
  straight to the holder, `rl hook` writing state files); the list (three sources, by project and
  by state, filters, search, git line and badges, live by file watch, `config.json` prefs); native
  sessions 1:1 (Claude via `claude agents --json --all` + `~/.claude/jobs`, Codex via the
  app-server); the switch (`rl-daemon`, `rl-proto`, `rl-ledger`, `rl-flow`, `rl-cli`, `rld`,
  `--no-tty` and flows deleted, ~89k lines; README, ARCHITECTURE, PRODUCT rewritten); messaging
  (`rl mcp` with `who` / `send` / `inbox`, `rl setup` for hooks and MCP) (#200)
* [x] slim · step 1 — the holder knows itself: `rl-session` works out its own state and writes
  `state.json` beside its socket, with nobody attached (#199)

* [x] the TUI god module split — `impl App` (176 methods, ~5k lines) distributed by pure code motion into sibling child modules of `tui/` (`select`, `act`, `flow_ops`, `catch_ops`, `feed_ops`, `summary_ops`, `join`, `shell_ops`, `keys`), and the row library `rows.rs` (~4.1k lines) split into a `rows/` folder (`model`/`group`/`name`/`word`/`build`/`order`, tests beside them); `tui/mod.rs` went 9,206 → 5,991 lines, behaviour unchanged; the `drawing`/`pane` cluster is held back to fold into the pane redesign, and `main.rs` is still to trim (#178, #179, #180, #182; CI unbroken in #181)
* [x] reads off the single writer, bytes off the ledger — a 4-connection read-only WAL pool beside the writer carries the display / query surface (digest, events, summaries, scrollback, lookups) while the flow engine and supervisor stay on the writer for ordering; a run's terminal bytes leave SQLite for per-run append-only files under `~/.relay/io/<run>/{out,in,note}.log`, the `session_io` table keeping only `(run, seq, direction, pos, len, ts)` (#176, #177)
* [x] a flow's own graph, the global graph dropped — a selected flow's pane draws what starts it, its steps by rank with their `if`s and last firing's marks, and the flows it starts (decided by the same `rl_flow::events` the daemon uses); `^g`, `/graph`, `graph.get` and the global layout are gone (#167)
* [x] agents started outside Relay — Claude's, Codex's and Gemini's own sessions are rows under their folder's project with their state, told by a user-level hook `rl setup` adds on a yes (`claude agents` asked only with a reason); enter attaches to a Claude background session or carries an idle one on as a Relay run; a run sent to `/background` links to the session it became; a new agent is a `sessions:` block in its profile (#164)
* [x] every flow on the list — a flow is a row from its yes on, with its trigger and next time (`every 30m · next in 12m`) before it ever fired, `paused` or `needs a yes again` when so; its runs stay on its row whatever their age; the headline still counts only what ran (#160)
* [x] the templates become examples you copy — `--from`, `--set` and `--in` go; `prod-logs` and `morning-brief` live in `examples/flows` with placeholders and a test that holds them to the code (#158)
* [x] the README says how to install Relay and use it in five minutes (#157)
* [x] flows, writing it — `rl flow new <name> [words…]` and `/flow new` from words (the interpreter drafts `flow.md`, shown before its yes), a template (`--from prod-logs`, `--from morning-brief`, blanks by `--set`) or the bare one; `rl flow test` / `/flow test` say what would fire and which steps would run, starting nothing; the context pack teaches flows (#156)
* [x] every lead in the catch-up keeps its title; flows in the graph no longer look joined; the badge test stops flaking (#155)
* [x] seeing flows — a firing's outcome on the list, `catch_up: top` heads the catch-up, the flow's pane with its firings, `/summary` on a firing, steps as nodes in the graph; a missed schedule catches up once by default (#154)
* [x] flow state — steps leave values and files for the steps after them: `rl flow state set|get|write|read`, `$RELAY_WORK` and `runs/<firing>/`, `RELAY_*` env, caps, retention with the firing's runs (#153)
* [x] an agent's row wears its CLI's glyph and colour, from its profile (#152)
* [x] flows replace schedules and reactions — a folder with `flow.md` under `~/.relay/flows`, approved by hash; steps with `after`/`if`, fan-in, overlap, restart and resume; `when` and `on`; `ask` steps; `rl flow`; the graph's rules become flows (#151) — this is the pipelines & wiring item
* [x] a resumed run keeps the place of the one it picks up (#150)
* [x] the panel opens on what happened while you were away — the catch-up (#149)
* [x] `^h` shows the shortcuts in the footer in place of its hints (#148)
* [x] `/summary` works on any row, brief or deep, since you last looked (#147)
* [x] the summariser's job is called summary: `/summary`, `rl summary`, `assistants.summary` (#146)
* [x] shift types what it says on the key — `?` opens the shortcuts and capitals stay capitals in kitty-protocol terminals (iTerm2): the panel asks for alternate keys, a shifted letter is a capital anyway (#145)
* [x] inline rename — `^r` edits a run's or project's name in its row, whole name selected, the line's editing keys; enter saves, esc/moving cancels; empty goes back to Relay's name (#144)
* [x] the header's mark stands under a row of air, at the left edge (#143)
* [x] recap on the overview — a waiting Claude/Codex row says the agent's own recap (picked up from its session file on hooks and a 5s sweep, no assistant asked); `/summarise` shows it in the details; Claude's "(disable recaps…)" hint stripped (#142)
* [x] header with Relay's mark — a radio tower, five rows beside `relay <version>`, the tally and the notes; one line when wide/short/narrow; the full mark on an empty list and setup (#141)
* [x] shift-↑/↓ reorders rows within a project (kept, new rows on top, needs-you floats, `/sort` resets); `?` on an empty line shows the keys for where you are; CI on GitHub runs again (#140)
* [x] `/reload` says what did not load and where (global or project, file:line); a broken global file no longer blocks the projects'; a red `! rules` on the header until fixed (#139)
* [x] hide & a `hidden` lens — `/hide` on a run, job or project; the `hidden` lens brings it back; a hidden run that needs you still comes through; `rl hide`/`rl unhide` (#138)
* [x] a global rule's runs stay under `everywhere` after the rule is gone — decided from the run, not the rules loaded now (#137)
* [x] `/events` links a reaction to the event it fired on exactly — the triggering event's id is recorded on the run and question it starts (#136)
* [x] joining an agent works every time — a second join, and a join right after starting, no longer fail or bounce back to the list (#117 frames read cancel-safely, #119 no pane in front of a join, #124/#130 the join repaints); not reproduced in ~100 tries
* [x] quiet glyphs — one coloured one-column glyph per row: ✦ agent, ❯ shell (green), ▸ command, ↻ job, ? question (#135)
* [x] `/events` — every fired event, newest first, with what each set off; searchable; enter goes to its run or rule (#134)
* [x] moving the cursor no longer fetches the whole list — every step reloaded the full digest (131 → 16 ms/key at 1500 runs) (#133)
* [x] command bar completion — `/` command arguments, paths as the first word, git/make/just/npm/cargo words, completing where the shell is; `/edit [schedules|reactions] [global]` opens the file in your editor (#132)
* [x] global rules run where things happen — a global reaction with no `cwd` runs in its event's folder; `rl on`/the form stop writing `~/.relay`; a global rule's runs outside every project sit under `everywhere` (not `~` or `.relay`) (#131)
* [x] a join repaints once, and a fresh one at all — the #124 follow-ups re-landed (the joiner's size decides; letting go leaves the run a row short); panel runs start at the whole terminal; a run joined in its first 3 s is nudged again (#130)
* [x] stopping a shell hangs it up — `^x`/`/stop`/`rl stop` sent SIGTERM, which an interactive shell ignores; a shell now gets SIGHUP (#129)
* [x] ux: quieter list — the header is the path (folder bold, parents dim, ` · name` when renamed), `⎇ branch` under the state column; one emoji per row for its kind (🤖 🐚 🔧 ⏰ ❓), state in the word's colour; agents named `<cli>: <first prompt>…` once; a shell shows its folder (#128)
* [x] search lands on the best match — a project whose name matches first, else the closest row; `tab`/shift-tab walk the matches, in the list and the graph (#127)
* [x] ux: remove a project — `^x ^x` stops its runs, archives them, dismisses its questions and asks whether to move its `.relay` to `~/.relay/removed/`; `rl workspace rm --stop --archive --move-rules` (protocol 2) (#125)
* [x] coming back to a run repaints it — a join shrinks the run a row and gives the size back (#124)
* [x] `^j`/`^k` move through the list, the graph and candidates while nothing is typed; with text they stay the line's (new line, cut to end) (#126)
* [x] the global rules are in the headline, not a row of their own — `↻ 2 ⚡ 1 everywhere`, amber with `needs a yes · /graph` when one waits; `^g`/`/graph` from anywhere, `/edit schedule|reaction [global]` opens the home's files (#123)
* [x] what a restart took can be picked back up — reconcile marks runs `run.lost`, the headline offers them, `/resume [all [commands]]` / `rl resume <ids>|--lost [--commands]` carries each on as a new run (Claude `--resume`, Codex `resume` by session; a shell fresh; commands only when asked) and archives the old one (#122)
* [x] the caret is a block — the line, the search line and form fields draw it full width over the character (reverse video), Claude-style, instead of a thin `▏` that pushed the text aside (#121)
* [x] a shell line no longer opens a tile — the cursor goes to the warm shell's row and `enter` joins it (#120)
* [x] enter goes into the run — `enter` on a live run joins it (no side pane first) and on an ended run reads its output; `ctrl-\` from a joined run lands on the list with no pane; the watch tile is gone (#119)
* [x] a reply nobody asked for no longer takes the keys (#118)
* [x] a frame that loses a race is not lost (#117)
* [x] a cut-off run is told as one (#116)
* [x] asking a holder to go means staying until it does (#115)
* [x] a finished run's terminal bytes are kept thirty days, not for ever (#114)
* [x] moving sessions picks nothing for you and asks once more (#113)
* [x] a holder with nobody watching stops burning a core (#112)
* [x] a run stops itself when Relay is gone for good, and stopping the daemon can stop its runs (#111)
* [x] setup moves Claude's own sessions into Relay (#110)
* [x] the pane's terminal names its run and marks a cut row (#109)
* [x] one keyboard per terminal, and the panel asks before it takes over (#108)
* [x] handing a run the terminal and taking it back: the title says how to get out, letting go lands on the run (#107)
* [x] the first start asks which agents to offer and who writes summaries (#106)
* [x] assistant runs have no input to wait on (`claude -p` no longer warns into the answer); a run on pipes reads line by line in the pane and on a terminal (#99)
* [x] the list's highlight runs to the edge on every row (#98)
* [x] workspace badges — `workspaces.badges` commands say their piece after a git folder's branch, `gh-pr` (seeded in `~/.relay/plugins`) the open PR; asked when the branch changes, then at most every 5 minutes while the list is shown (#97)
* [x] git-backed workspaces keep their name — a folder is one project whatever its branch; its header shows `⎇ branch · N worktrees` (the count on the main checkout), a run started on another branch says so, `on:` searches by branch (#96)
* [x] recap & summary since — `/summarise [since]` / `rl summarise <run> [since]` recaps a run, live or ended, only when asked: Claude's own `away_summary`, else Codex's turns, else `assistants.summarise`; the latest kept per run and marked when the run moved on; aligned under the facts in the pane (#95)
* [x] holder leak — a holder (`rl-session`) outlives its ended child when no daemon collects the exit, and the integration tests leave their holders running; a holder now leaves once its socket is gone, checked once a minute (#93, #94)
* [x] questions lead the list, one line each; selecting one opens the pane on it, its choices (options, `attach` to the asking run, `dismiss`) taken by arrows + enter, the number, or a click; a line starting with an option answers with the rest as the note (`RELAY_NOTE`, `RELAY_CHOICE`, `RELAY_DECISION` reach the run); text before `^x` is a dismissal's reason (#92)
* [x] search — `^f` narrows the list or the graph as you type: every word, plus `in:` `is:` `kind:` `by:` completed by tab; `/filter` is its command (#91)
* [x] the panel runs to the screen's edges, runs one column in, the prompt is `❯`, header and footer level with typed text (#89, #90)
* [x] panel fixes — `^t` pins, a shell is a command not a key, projects by name with global last, the pane a third of the screen, shift-enter in tmux via iTerm2's line feed (#88)
* [x] the graph replaces the rules screen — one global network of schedules, reactions, questions, shells and agents, laid out in layers, walked along its wiring, acting on its nodes and answering questions in place; `^g` from a row opens it on that row's rule (#84–#87)
* [x] pin — `^p` / `/pin` keeps a run above every project, whatever its age or the lens; `/archive seen` leaves it alone (`run.pin`)
* [x] CLI names match the panel — `rl archive` and `rl prune` (`rl done` and `rl gc` still answer)
* [x] the cursor follows the mouse; a joined shell or agent shows its caret; the global row is `global · ~/.relay` (was "yours")
* [x] command & shortcut cleanup — one name per act (`/archive [seen]`, `/pane [full]`, `/reload [rules|shell]`, `/edit [file|schedule|reaction]`), keys `^x` `^r` `^o` `^t` `^c` shown in the command list
* [x] every project is on the list under every lens — registered folders even before anything ran, and the global row always (`/up` registers from the panel)
* [x] `/edit schedule|reaction` opens the rule file for the scope under the cursor
* [x] rename anything — `^r` on a folder, a run or a shell (`run.rename`), a job through its rule
* [x] shift-enter works in kitty-protocol terminals
* [x] esc closes the pane first
* [x] CI checks on Linux in Docker — `scripts/ci`
* [x] persistent project shell — several shells per folder reached from the list, per-command facts via prompt marks (OSC 133: what ran, exit code, into the ledger), a shell reads as a shell and leads its folder, prompt can show it is a Relay shell
* [x] mouse reaches the list; the selected row wears a band
* [x] `ctrl-x` and `ctrl-c` ask first; the command list scrolls and shows key shortcuts
* [x] UC2 wiring — logs check → analyse → a Notion task on approval (`examples/weather-alert`), `rl emit` for custom events
* [x] rename shared-* crates to rl-core, rl-detect, rl-ledger, rl-proto
* [x] warm per-project shell (slice 1) — a shell line runs in a persistent `$SHELL -i` per folder, not a fresh run each time (`run.input`)
* [x] trustworthy stop — reaps the whole process group (PTY + pipe children); a run's last output reaches viewers before the exit
* [x] manual commands run in your real shell — aliases and functions resolve, recognised via `$SHELL` (`/reindex`)
* [x] selectable project rows — stats pane, `/rules` to edit its schedules & reactions, `^r` rename (`workspace.rename`)
* [x] arrow-key navigation of command candidates
* [x] multi-line prompt input — shift-enter drops a newline, enter still sends
* [x] real terminal handover — raw attach/join passthrough (`rl join` standalone), watch tile, `/full`, `ctrl-\` detach
* [x] archive & done — `ctrl-x`, `/done`, `/done-all` (archive now, purge on retention later)
* [x] lens toggle — all / agents / jobs / reactions
* [x] rich TUI — colour carries run state; header, lens, gutter and section rules; detail pane beside the list
