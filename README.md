# Relay

Relay is `claude agents` for every harness. One list of your agent sessions (Claude, Codex, Gemini
and the like) and your shells, in the terminal you already use. Enter attaches. Agents can send
each other messages.

Command: `rl`. No daemon.

Early; macOS and Linux. Relay calls no model.

## Install

Needs a recent stable Rust (CI uses 1.96) and `git`.

```sh
git clone https://github.com/instilled/relay && cd relay
cargo build --release
export PATH="$PWD/target/release:$PATH"   # rl and rl-session must stay side by side
```

Or link the dev shim, which rebuilds when the source changes: `ln -sf "$PWD/scripts/rl" ~/.local/bin/`.

Everything lives under `~/.relay` (`RELAY_HOME` moves it).

## First run

```sh
rl setup      # hooks and the MCP server, for each harness found on PATH
rl            # the list
```

`rl setup` adds Relay's hook to each harness's user-level settings and registers
`rl mcp --harness <name>` as an MCP server named `relay` (Claude's through `claude mcp add --scope user`,
so `claude` must be on PATH). It says what it changed, and `nothing to change` on a second run.
`rl setup --remove` takes both out again, also for a harness no longer installed; `--harness <name>`
(repeatable) limits it to one.
Codex asks you to trust the hook once (`/hooks` in codex); Claude may ask you to review it in `/hooks`.

## The list

Rows are agents and shells, under their project folder or under Needs input, Working and Done.
Claude's and Codex's own background sessions are on it too. `^h` or `?` shows the keys.

| key | does |
| --- | --- |
| `enter` | attach |
| `n` | new session |
| `↑↓` `^j` `^k` | move |
| `pgup` `pgdn` `home` `end` | page |
| `^f` | search |
| `^x ^x` | stop, then remove or hide |
| `^r` | rename |
| `^t` | pin |
| `⇧↑↓` | reorder |
| `a` `s` `h` | show agents, shells, hidden |
| `^s` | group by project or by state |
| `^c ^c` | quit |

`ctrl-\` leaves an attached session running and comes back to the list.

## Commands

```sh
rl new [--harness claude] [--cwd DIR] [--name-only] [-- ARGV...]   # a shell, or a harness; prints the name
rl ls [--json]                  # the list, once
rl attach <name>                # ctrl-\ detaches
rl stop <name>                  # close a running session
rl rm <name>                    # remove one that is not running
rl mcp --harness H              # serve send, who and inbox over stdio (agents call this)
rl setup [--remove] [--harness H]
```

`rl hook <agent> <Event>` is what an agent's hook calls. It is not for you to run.

A session lives in a small holder process (`rl-session`) that keeps its terminal, so closing your
terminal or the laptop lid costs nothing. Without a daemon, nothing else is running.

## Files

`~/.relay`:

| path | holds |
| --- | --- |
| `sessions/<name>/` | one held session: `meta.json`, `state.json`, `hook.json`, `exit`, `sock` |
| `state/<agent>/<session_id>.json` | what an agent's hooks said of a session Relay does not hold |
| `mail/<key>/` | messages waiting for an agent, one file each (key: a held session's name, else its conversation id) |
| `config.json` | harnesses offered, badges, how the list stands (grouping, pins, names, order, hidden) |
| `hooks/` | settings files `rl new` passes to a harness (Claude's `--settings`) |
| `detect/*.yaml` | your harness profiles |
| `plugins/gh-pr` | the badge script for project headers |

A socket too long for the OS limit goes under `$TMPDIR/rl-<uid>-<hash>/`.

## Adapters

A harness is a file, `detect/<name>.yaml`. The built-in ones are compiled in (`claude`, `codex`,
`cursor`, `gemini`, `opencode`, `sh`, with `generic` as the floor under all). Yours in
`~/.relay/detect/` merge over them field by field; a new name adds a harness.

Keys a profile accepts:

- `kind`: `agent`, `shell`, `command` or `plugin`
- `bins`: binary names to match
- `interactive_only`
- `hooks`: `dialect`, `args` (what `rl new` passes the binary; `{hooks_file}` is the settings file)
- `resume`: arguments that carry a conversation on, with `{session}`
- `icon`, `color` (`#rrggbb`)
- `patterns`: `needs_input`, `idle` (regexes over the screen, the fallback when hooks say nothing)
- `timing`: `idle_after`, `stale_after`
- `sessions`: the harness's own sessions
  - `hooks`: `file`, `timeout`, `unit`, `async`, `marker`, `after_install`, `from_env`, `events`, `settings`
  - `list`: `args` and `reader` (`claude-agents` or `codex-threads`)
  - `attach`, `start`, `name`, `stop`: argument lists with `{id}` and `{name}`

Unknown keys are an error. Old keys such as `headless`, `model_flag`, `small_model` and `says` no
longer load; remove them. See `detect/claude.yaml` and `detect/codex.yaml` for worked examples.

## For agents

`rl mcp` is a stdio MCP server with three tools:

- `who`: the list's rows, one line each (`address (title) · harness · state · folder`), you marked. The title is the one you gave the row with `^r`.
- `send(to, text, from?)`: leave a message for another agent, by address, title or conversation id. An unknown address is an error with close matches.
- `inbox(as?)`: take your waiting messages, oldest first. They are deleted as they are read.

Your address is found in this order: `RELAY_SESSION`, when the held session runs the harness the
server was started for (`RELAY_SESSION_AGENT`, so an agent started in a held shell is not the shell),
else (Claude) `CLAUDE_CODE_SESSION_ID` matched against the list, else you pass `from` or `as`
yourself. Codex has no automatic address yet, so it passes them.

## Known limits

Not verified against the live CLIs:

- What `claude --bg` prints. Relay takes the first 8-hex word as the session id; if there is none, the start says so.
- `claude stop <id>` from `^x`.
- That `codex resume <id>` rejoins a thread the Codex app-server has loaded, and when a thread counts as loaded.
- The Gemini MCP entry (`mcpServers.relay` in its `settings.json`).
- `claude agents` state mapping against its own screen.

## Docs

- [PRODUCT.md](docs/PRODUCT.md): what Relay is for.
- [ARCHITECTURE.md](docs/ARCHITECTURE.md): how the tree is wired, as built.
- [PRODUCT-ROADMAP.md](docs/PRODUCT-ROADMAP.md): done and next.

Relay is a rebuild of [instilled/ax](https://github.com/instilled/ax), kept archived as the legacy reference.
