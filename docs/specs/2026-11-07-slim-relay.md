# Slim relay — one agents view for every harness

**Status:** design · **Date:** 2026-11-07

## Intent

Relay becomes one thing: **Claude Code's agents view (`claude agents`), but for every harness, plus
shells, plus messaging between agents.** instilled loves `claude agents` and finds it the best
experience available; its only problem is that it is Claude-only. Everything else Relay grew
(flows, decisions, catch-up, ledger facts, summaries, project overview, cost) goes, and so does
the daemon.

Why not use an existing tool: a scan on 2026-11-07 (see `docs/competitive-landscape.md` for the
October one) found the agent-agnostic tools — tuios, herdr, agent-manager, agent-deck, Agent of
Empires — all **reinvent the terminal** (tiling WM, tmux, their own panes) and only see agents
started inside them. JetBrains Air is a GUI app over ACP. Meanwhile Claude (`claude agents`,
supervisor daemon) and Codex (`codex agents`, app-server daemon) now run their own background
sessions. Nobody shows those together. That is the gap: **a thin view over each harness's own
background sessions**, run in the terminal you already use, with Relay's own holder only where a
harness has none.

## Goals

1. **One full-screen list** in the user's existing terminal, of every agent session across
   harnesses and every Relay-held shell. A row: state icon, name, harness, folder, summary, age.
   - **Two groupings, toggled with a key** (Claude's call: `^s`, as in `claude agents`; the
     choice persists). Both include every row, agents and shells alike.
     - **By project**, exactly as relay has it today (`rl-cli/src/tui/rows/order.rs`,
       `group.rs`): rows under their project folder; within a project, rows in the order they
       came, newest at the bottom, unless you reorder them (`⇧↑↓`); projects can be reordered
       too; a project header shows `! N need you` for buried rows that need you.
     - **By state**, as in `claude agents`: headings Needs input · Working · Done (idle,
       finished, failed, stopped). Within a heading, oldest first, new at the bottom.
   - **Kept from today's list (instilled): grouping, filtering, sorting, the navigation keys, shells
     and agents side by side.** Keys as today (`rl-cli/src/tui/shortcuts.rs`,
     `palette.rs::key`): `↑↓`/`^j^k` move, `⇧↑↓` reorder, enter attaches, `^x`
     stop/archive/remove, `^t` pin, `^r` rename, `^f` search, `a`/`s`/`h` filter to
     agents/shells/hidden, `^c` quit, `^h` help. `^o` (details pane) goes with the pane.
   - Project headers and their git information show what they show today
     (`rl-daemon/src/git.rs`, `badges.rs`, `rl_proto::types::Checkout`); move that code, don't
     redesign it.
2. **Attach full-screen to the agent's own TUI; ctrl-\ detaches back to the list and never stops
   it.** Relay draws nothing over an attached session (status goes in the window title, as
   today).
3. **Start a new agent or shell from the list**: pick harness + folder, optional prompt.
4. **Stop** a session from the list.
5. **Three sources feed the list:**
   - **Harness-native background sessions**, listed and attached through the harness: Claude via
     `claude agents --json` + `claude attach <id>`; Codex via its app-server daemon
     (`codex agents` / `codex queue` exist in codex-cli 0.165).
   - **Relay-held sessions**: harnesses with no background mode (Gemini, OpenCode, …) and plain
     shells, held by `rl-session` so they survive terminal close.
   - **Agents in other terminals**, listed with status from hooks; enter carries an idle one on
     (see Calls).
6. **A native row shows 1:1 what the harness's own agents view shows** (instilled, 2026-11-07).
   - **Claude:** the same sessions as `claude agents --all` (completed ones included), and per
     row the same name, state (working / needs input / idle / completed / failed / stopped, and
     its icon meaning), summary (the one-line `detail`, seen in `~/.claude/jobs/<id>/state.json`),
     age (from creation, frozen when finished) and PR label (`#1234`, colour by PR state) that
     `claude agents` shows.
   - **Codex:** the same sessions as `codex agents`, with the same name, state (Need input /
     Working / Ready) and latest activity it shows.
   - Relay re-labels nothing: no state words or summaries of its own for native rows. Only
     Relay-held and other-terminal rows use Relay's own state words, and where they have no
     harness summary they show the last question or last line. Relay calls no model.
7. **New harness = one adapter** (start, list, attach, state, summary, resume), declared in the
   existing `detect/*.yaml` style where possible, with at most one Rust file.
8. **Messaging between agents (new, this round): dead-simple MCP.** Tools `send(to, text)`,
   `who()`, `inbox()`. Pull only: agents see mail when they call `inbox()`. No hook delivery, no
   typing into idle agents. Relay registers the MCP server with each harness it supports
   (user-level config, alongside the hooks it already installs). `who()` lists every session on
   the list with name, harness, folder and state.

## Architecture: no daemon (instilled, 2026-11-07)

There is no `rld`, no JSON-RPC protocol and no SQLite. Two binaries and plain files.

- **`rl`**: the list, `new`, `attach`, `stop`, `ls`, `hook`, `mcp`, `setup`.
- **`rl-session`**: one detached holder per Relay-held session (fork + `setsid`, as today). It
  owns the PTY, the in-memory ring and screen, and its socket. It now also **works out its own
  state** (working / needs input / idle, plus the last line) from its screen, moved in from
  `rl-detect` (`machine.rs`, `pty.rs`).

### Files (`~/.relay`, `RELAY_HOME` moves it)

| Path | Written by | Holds |
|---|---|---|
| `sessions/<name>/meta.json` | `rl` at start | harness, folder, argv, started |
| `sessions/<name>/sock` | holder | its socket (frame protocol as today) |
| `sessions/<name>/state.json` | holder, on change | state, last line, foreground |
| `sessions/<name>/exit` | holder, at exit | exit code, ended at |
| `state/<agent>/<session-id>.json` | `rl hook` | hook-reported state of any agent, with pid |
| `mail/<address>/<ts>-<from>.json` | `rl mcp` `send` | one message per file |
| `config.json` | `rl setup` | harnesses offered, badges, list prefs (grouping, order, hidden, pins) |

All writes are atomic (temp file + rename). A session name is claimed by creating its folder,
which fails if it exists, so two `rl`s can't take the same name.

### How each moment works

- **Start:** `rl` claims `sessions/<name>/`, writes `meta.json`, spawns `rl-session`, waits for
  its readiness pipe (as today), returns. The holder then lives on its own.
- **Running:** the holder rewrites `state.json` when its state changes. At exit it writes `exit`,
  lingers (24h, `--linger`, as today) so the last screen can still be attached to, then removes
  its socket. Hooks from any agent run `rl hook <agent> <event>`, which writes its state file and
  exits; no process has to be up for a hook to land.
- **List open:** reads `sessions/*` (a socket that won't connect means the holder is gone; `exit`
  means it ended) and `state/*` (checking the pid is alive, so a closed tab doesn't linger as
  working). Runs `claude agents --json` every few seconds and the Codex equivalent. Watches the
  folders (kqueue / inotify) to update live. Git and badges refresh on the same tick. Closing the
  list stops all of it: nothing polls while nobody looks. On start it sweeps leftovers from
  crashes (dead socket, no `exit`, holder pid gone).
- **Attach:** Relay-held → connect to the holder's socket directly. Native → run the harness's
  attach argv on a local PTY (as `held.rs` does for `claude attach`). ctrl-\ detaches both.
- **Messaging:** each harness launches `rl mcp` per agent as a stdio MCP server. `send` drops a
  file into `mail/<to>/`; `inbox` reads and removes the caller's files; `who` builds the same
  rows the list shows. Mail waits as files, so sender and receiver needn't overlap.

### Known risk

macOS limits a socket path to ~104 bytes. `~/.relay/sessions/<name>/sock` can overflow with a
long home or name. Keep names short, or put sockets under `$TMPDIR` with a link from the session
folder; the builder picks. (Relay has this risk today too.)

### Code structure

```
code/
├── rl-core/        ids, ~/.relay paths, atomic write, time
├── rl-session/     the holder: main, holder, backend (PTY), proto, client, screen, ring,
│                   state (NEW: own state detection)
├── rl-detect/      harness adapters: detect/*.yaml (claude, codex, gemini, opencode, shell),
│                   adapter.rs (trait: start, list_native, attach, resume, summary),
│                   claude.rs, codex.rs, hooks.rs (install hooks + MCP registration)
└── rl/             the one user binary
    ├── main.rs     subcommands
    ├── store/      sessions.rs, state.rs, mail.rs
    ├── sources.rs  merges the three sources into rows; the TUI never knows a row's source
    ├── git.rs      checkout + badges (moved from rl-daemon)
    ├── hook.rs     `rl hook`
    ├── mcp.rs      `rl mcp`
    ├── attach/     held.rs (Relay holder), native.rs (harness attach argv)
    └── tui/        app, rows (grouping/order/filter, moved from tui/rows/), draw, keys, new
```

`rl-daemon`, `rl-proto`, `rl-ledger`, `rl-flow` and `rl-cli` are deleted. Expected size
10–15k lines (from 96k). Build `code/rl` fresh beside the old crates, moving files in rather than
carving the old ones down; switch the workspace and delete the old crates once it does
everything above.

## Non-goals

- Flows, decisions, catch-up/`rl since`, digests, recaps, the fact ledger, cost, summaries Relay
  writes, project overview, leads/tags/presence, the assistant, notifications, an `rl setup`
  wizard beyond hook + MCP install and which harnesses to offer.
- **A daemon** of any kind. If something ever has to run with the list closed, it comes back as
  another reader of the same files.
- **The on-disk output journal** (instilled): `session_io`, `~/.relay/io/<run>/` logs, their GC, and
  the `--no-tty` pipes-replay path. The holder's in-memory screen + ring stay.
- Panes, splits, tabs, workspaces: Relay is not a terminal or multiplexer.
- Worktrees, diff review, PR workflow.
- Peek-and-reply inline in the list (nice in `claude agents`, not this round).
- Backward compatibility: no migration of old runs/ledger/flows, no old-protocol peers. Kill the
  old `rl`/`rld` after merge.

## Anti-goals (failure even if it works)

- Relay becomes another tmux/tuios: owns layout, draws chrome around the agent.
- Relay re-hosts sessions a harness already runs in the background (duplicates Claude's or
  Codex's supervisor).
- The slim version keeps the old weight in disguise (tables, protocol methods, TUI modules,
  a "small" daemon kept "in case").

## What exists and is reused (2026-11-07 map)

- **`rl-session`**: detached PTY holder, own binary frame protocol, depends only on rl-core.
  Keep; add state detection and the `state.json`/`exit` files; drop `--no-tty`.
- **Relay-run attach**: `rl-cli/src/attach.rs` (detach 0x1c). Today it goes through
  `rl-daemon/src/attach.rs`; now `rl` connects to the holder directly. Drop its
  decisions/ledger imports.
- **Native attach**: `rl-cli/src/held.rs` runs `claude attach <id>` on a local PTY. Generalise
  to "the adapter's attach argv".
- **Detection**: `rl-detect` (`detect/*.yaml`, registry, `user_hooks.rs` installing
  Claude/Codex/Gemini user hooks, `claude.rs` per-run settings) and the `claude agents --json`
  parsing in `rl-daemon/src/outside/claude.rs`. Move the parsing into the Claude adapter.
- **Hooks**: `rl-cli/src/hook.rs`; today it posts to the daemon, now it writes a state file.
- **List**: `rl-cli/src/tui/rows/` (grouping, order, filter, needs hint), `shortcuts.rs`,
  `palette.rs` keys. Move what serves the list; drop flows/questions/overview branches.
- No MCP or Codex app-server code exists yet.

## Decided

- `claude agents` is the model for the experience (one list, attach, detach, start from the
  list); its by-state grouping is borrowed; otherwise grouping, sorting, filtering and keys are
  relay's own, as today.
- ctrl-\ detaches on every attach path.
- Agent-agnostic via per-harness adapters; harness-native background sessions preferred over
  Relay holding them.
- Native rows mirror the harness's own agents view 1:1 (`claude agents --all`, `codex agents`).
- Messaging this round: MCP only, pull only.
- No daemon; plain files.
- No output journal.
- Delete, don't deprecate.

## Calls made in the write-up (Claude's, for instilled to check)

- **Carrying on an idle outside agent goes to the harness's own background where one exists.**
  An idle Claude session in another terminal resumes as a Claude background session
  (`claude --resume <id> --bg`), not a Relay-held one. The old "stop a Claude background session
  and re-host it in Relay" and "send a Relay run to Claude's background" paths go. Only
  harnesses with no background mode are carried on as Relay-held sessions.
- **Codex falls back to Relay-held if native attach can't be done.** If codex-cli 0.165 can't
  open one specific app-server session in its TUI, Codex ships Relay-held (like Gemini) this
  round, and still *lists* app-server sessions if that is cheap.
- **Names, and so addresses:** native sessions are addressed by the harness's own name (Claude's
  `name`, Codex's thread name), or their id when names clash; Relay stores nothing for sessions
  it doesn't host. Relay-held sessions get short stable names (`<folder>-<harness>`, deduped with
  a number), which are also their folder names.
- **An agent knows its own address** from an env var Relay sets on sessions it starts (today's
  `RELAY_RUN`, renamed as fits); for native sessions `rl mcp` identifies the caller from the
  harness's session id where the harness passes one, else `send` takes a `from`.
- **`^s` toggles grouping**; `--linger` stays so an ended shell's last screen can be seen.

## Supersedes

All open roadmap work is dropped (instilled, 2026-11-07): the report arc (J6 facts, push hook), the
pane redesign, the project overview's "everywhere" summary, notifications. The roadmap on
`chore/roadmap` is rewritten around this design. No refactoring of code this design deletes.

## Left to the builder

- Order of work and PR slicing (one worktree + PR per step; keep `main` green).
- How to talk to Codex's app-server (JSON-RPC over its unix socket vs. shelling to `codex`), and
  how to open one Codex session in its own TUI; verify against codex-cli 0.165.
- Where each harness view's fields come from: `claude agents --json --all` lacks `detail` and PR
  labels (they sit in `~/.claude/jobs/<id>/state.json`, undocumented); find the Codex fields in
  its app-server API. Prefer supported sources; if a format changes, show the field empty rather
  than invent one.
- Socket placement (see Known risk); file-watch library; poll intervals.
- Exact row layout; key bindings beyond those listed; the MCP tool schemas.
- README / ARCHITECTURE / PRODUCT docs rewritten to match.
