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
Claude's and Codex's own background sessions are on it too. `ctrl+h` or `f1` (for terminals that
send `ctrl+h` as backspace) shows every key as a grid under the command line, in place of the footer's
hints, the way `claude agents` does: `ctrl+r to rename`, `ctrl+f to find`, in up to four columns as
the terminal's width allows. `ctrl+h` again, `f1` or `esc` puts it away; `/help` opens it too. The
footer's hints are entries of the same table.

At the foot of the list is a command line. Type to start a session, in the folder of the row under
the cursor (else where `rl` was started; the line shows only the prompt `❯`):

- `claude fix the login`, `codex …`, or `agy …` (also `gemini` and `antigravity`, which mean
  Google Antigravity's `agy`): that agent, the rest of the line its first prompt. Claude starts in
  its own background (`claude --bg`); the others in a session Relay holds.
- `/` and a name: a command, run on the row under the cursor or on the list (below).
- Anything else is a shell command, run in a shell Relay holds, which stays a shell once it is done.

The right end of the line says which it is (`agent · claude`, `shell`, `command · pin`, `unknown
command`, or `agent · antigravity (agy not found)`, which will not start). `tab` completes a
harness, a program on `PATH`, or a path, and on a `/` line the command, then its colour or, for
`/rename`, the row's name; the best completion stands dim after the cursor, and `tab` again cycles.
Every letter types; the list's own keys are ctrl keys, the arrows and `enter`. `ctrl+f` searches.

| command | does |
| --- | --- |
| `/colour <name\|#rrggbb\|none>` (`/color`) | your colour for the row, as `ctrl+g` and `rl color` set it (a colour the harness set wins on screen) |
| `/rename <name>` | rename the row or project, as `ctrl+r`; with no name, back to its own. A Claude or Codex session is named in Relay only (the harness has no rename): your name, then the harness's own dim beside it, and the search finds both |
| `/pin` | pin or unpin, as `ctrl+t` |
| `/hide` | hide the row or project, or bring it back |
| `/stop` | stop a live session, held or the harness's own, at once (no second press) |
| `/rm` | remove an ended session Relay holds |
| `/agents` `/shells` `/hidden` | show agents, shells, hidden, as `ctrl+a` `ctrl+l` `ctrl+o` |
| `/group` | by project or by state, as `ctrl+s` |
| `/help` | the keys |
| `/quit` | quit |

A command that does not apply to the row (`/rm` on a live session, `/colour` on a project) says why
in the footer and changes nothing; an unknown one stays on the line to mend.

| key | does |
| --- | --- |
| `enter` | attach (on a line with text: start it) |
| typing | the command line; `esc` clears it, `home` `end` go to its ends |
| `↑↓` `ctrl+j` `ctrl+k` | move |
| `pgup` `pgdn` `home` `end` | page (`home` `end` with nothing typed) |
| `/` | a command (above) |
| `ctrl+f` | search |
| `ctrl+x ctrl+x` | stop, then remove or hide |
| `ctrl+r` | rename |
| `ctrl+t` | pin |
| `ctrl+g` | your colour for the row: none, red, blue, green, yellow, purple, orange, pink, cyan (a colour the harness set wins on screen; a `#rrggbb` set in `config.json` or by `rl color` goes to red) |
| `shift+↑↓` | reorder |
| `ctrl+a` `ctrl+l` `ctrl+o` | show agents, shells, hidden (the header shows which: `✦ ctrl+a  ❯ ctrl+l  │  ◌ ctrl+o`, icons alone on a narrow one-line header) |
| `ctrl+s` | group by project or by state (the header says which) |
| `ctrl+c ctrl+c` | quit |
| `ctrl+h` `f1` | the keys, as a grid; again or `esc` closes it |

`ctrl+\` leaves an attached session running and comes back to the list.

## Commands

```sh
rl new [--harness claude] [--cwd DIR] [--name-only] [-- ARGV...]   # a shell, or a harness; prints the name
rl ls [--json]                  # the list, once
rl attach <name>                # ctrl+\ detaches
rl stop <name>                  # close a running session
rl rm <name>                    # remove one that is not running
rl color <session> <name|#rrggbb|none>  # your colour for a row (a held name, or n:claude:<id> / o:claude:<id>)
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
| `config.json` | harnesses offered, badges, how the list stands (grouping, pins, names, colours, order, hidden) |
| `hooks/` | settings files `rl new` passes to a harness (Claude's `--settings`) |
| `detect/*.yaml` | your harness profiles |
| `plugins/gh-pr` | the badge script for project headers |

A socket too long for the OS limit goes under `$TMPDIR/rl-<uid>-<hash>/`.

## Adapters

A harness is a file, `detect/<name>.yaml`. The built-in ones are compiled in (`antigravity`,
`claude`, `codex`, `cursor`, `gemini`, `opencode`, `sh`, with `generic` as the floor under all). Yours in
`~/.relay/detect/` merge over them field by field; a new name adds a harness.

Keys a profile accepts:

- `kind`: `agent`, `shell`, `command` or `plugin`
- `bins`: binary names to match
- `words`: first words of the list's command line that start it (the first profile by name to
  claim a word has it)
- `prompt`: where a first prompt goes, with `{prompt}` (`["-i", "{prompt}"]`); without it, last
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

## Environment

A session Relay holds gets:

| variable | holds |
| --- | --- |
| `RL_SESSION` | the session's name |
| `RL_SESSION_AGENT` | the harness it runs (`sh` for a shell) |
| `RELAY_HOME` | the Relay home it belongs to|

To show it in a zsh prompt: `PROMPT='${RL_SESSION:+[$RL_SESSION] }'$PROMPT`.

## For agents

`rl mcp` is a stdio MCP server with three tools:

- `who`: the list's rows, one line each (`address (title) · harness · state · folder`), you marked. The title is the one you gave the row with `ctrl+r`; a native session's address stays the harness's name or id, whatever you named it.
- `send(to, text, from?)`: leave a message for another agent, by address, title or conversation id. An unknown address is an error with close matches.
- `inbox(as?)`: take your waiting messages, oldest first. They are deleted as they are read.

How a recipient learns it has mail depends on its harness:

- **Codex**: pushed. `send` also runs `codex queue --thread <id> --message "relay mail from <you>: call the relay inbox tool to read it" --resume` (the profile's `sessions.ping`) on the recipient's thread: a Codex row's id, or what a held Codex session's hooks said. `send` says `· pinged codex`, or why codex didn't take the ping; the message is left either way.
- **Claude**: told by Relay's hooks. With mail waiting, each prompt gets a line of context saying so (`UserPromptSubmit`), and at the end of a turn the `Stop` hook keeps Claude going one more turn to read it, once for the same mail. Needs the hooks `rl setup` installs (or a session Relay holds). When one Claude session sends to one of Claude's background sessions, `send` also suggests waking it now with Claude's `SendMessage`, by the name `claude agents` shows for it.
- **Antigravity, shells and the rest**: they find it when they call `inbox`.

Your address is found in this order: `RL_SESSION`, when the held session runs the harness the
server was started for (`RL_SESSION_AGENT`, so an agent started in a held shell is not the shell),
else (Claude) `CLAUDE_CODE_SESSION_ID` matched against the list, else you pass `from` or `as`
yourself. Codex has no automatic address yet, so it passes them.

## Known limits

Not verified against the live CLIs:

- What `claude --bg` prints. Relay takes the first 8-hex word as the session id; if there is none, the start says so.
- `claude stop <id>` from `ctrl+x`.
- That `codex resume <id>` rejoins a thread the Codex app-server has loaded, and when a thread counts as loaded.
- The Gemini MCP entry (`mcpServers.relay` in its `settings.json`).
- `claude agents` state mapping against its own screen.
- Antigravity (`agy`): no hooks and no screen patterns of its own yet, so its state is the generic guess.
- A Codex session Relay holds stays Relay's when you detach. Its thread can't be left with Codex's
  own daemon. Checked on codex 0.167: the daemon lists a TUI's thread as loaded while the TUI runs
  and for a few seconds after it exits, but not a minute later. A hand-off would leave a row that
  vanishes. `codex resume <id>` still finds the saved conversation.

## Docs

- [PRODUCT.md](docs/PRODUCT.md): what Relay is for.
- [ARCHITECTURE.md](docs/ARCHITECTURE.md): how the tree is wired, as built.
- [PRODUCT-ROADMAP.md](docs/PRODUCT-ROADMAP.md): done and next.

Relay is a rebuild of [instilled/ax](https://github.com/instilled/ax), kept archived as the legacy reference.
