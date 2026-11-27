# Relay — architecture

The [README](../README.md) says what Relay is for; this file says how `code/` is wired and names the
code to read. The keys are one table in `rl/src/tui/help.rs`, the `/` commands one in
`rl/src/tui/slash.rs`.

## Processes

No daemon, no database: two kinds of process and one file tree.

```
rl (list, commands) ──Unix socket──▶ rl-session (one per held session: PTY, ring, screen, socket)
   │  └──lists, starts, stops──▶ claude agents / claude --bg · codex app-server
   ▼
~/.relay ◀── rl hook (an agent's hooks) · rl mcp (an agent's MCP server)
```

- `rl` is started by you: the list, or a command.
- `rl-session` is started by `rl new` (found next to `rl`), forks away, owns one terminal, outlives
  `rl`, and writes its session's state from the screen to `state.json`.
- `rl hook` and `rl mcp` are `rl` run by an agent. A hook writes a file and exits 0, quickly.

Folders are the truth: liveness is read from files and sockets; every write is `rl_core::atomic`.

| crate | holds |
| --- | --- |
| `rl-core` | home paths (`Home`), atomic writes, state words, time |
| `rl-session` | the holder binary: PTY, ring and screen, socket protocol, state from the screen |
| `rl-detect` | profiles, the `Harness` trait and its implementations, setup's parts, drift notices |
| `rl` | the `rl` binary: commands, the list (`tui/`), rows, hooks, MCP, mail |

## Files

`~/.relay` (`RELAY_HOME` moves it; `Home` names every path).

| path | holds |
| --- | --- |
| `sessions/<name>/` | a held session: `meta.json`, `state.json` (from the screen), `hook.json` (from its hooks), `exit`, `sock` |
| `state/<agent>/<id>.json` | a session Relay doesn't hold, as its hooks tell it |
| `mail/<key>/` | waiting messages; `delivered/` kept 7 days |
| `config.json` | harnesses, badges, declined setup, list prefs (grouping, toggles, order, hidden, pins, names, colours). Strict |
| `detect/*.yaml`, `plugins/` | your profile overrides; badge scripts (`gh-pr`) |
| `drift/` | what Relay noticed a harness change in |
| `trace.log` | phase timings, only under `RL_TRACE=1` |

## The list

- **Sources.** Held sessions from `sessions/`, hooked ones from `state/`, Claude's and Codex's own
  by asking them (`Harness::sessions`), each on its own task and interval (`tui::live`): a slow one
  holds up no other, a failing one keeps its last rows marked stale. The first frame waits for none
  of them. What the file watch, the 3s tick and a lister's answer make the list read again is read
  off the UI thread (`tui::Reads`), one read at a time; a read begun before a newer one is dropped.
- **Rows.** `rows/` merges and dedupes them (a native row hides the hooked row of the same session)
  and groups them under a project: a folder you named, else its git checkout (`git::project`;
  worktrees under their main one). State comes from the hooks, then the harness's listing, then
  regexes over a held session's screen.
- **Command line** (`tui/cmdline.rs`). A first word a profile claims starts that agent with the rest
  as its prompt; `!` runs the line in a held shell; `/` is a command.
- **Attach.** A held session: its socket, the screen, then live bytes; `ctrl+\` detaches: the byte
  `0x1c`, or the escape a terminal sends once the program has switched on an enhanced keyboard
  (kitty `ESC[92;5u`, xterm `ESC[27;5;92~`; `attach::find_detach`). A native
  one: its harness's attach argv in this terminal (`attach::native`). Which a row gets is
  `Harness::attach`. Coming back, the list takes the screen in one synchronized update and draws its
  first frame while the terminal settles (60 ms); what the terminal sent in the handover is dropped
  after, before any input is read.
- **Timing.** `RL_TRACE=1` writes each phase's milliseconds to `$RELAY_HOME/trace.log` (`trace`):
  the start, key → frame, reads, attach and return. Off, it costs nothing.
- **Actions.** Stop, remove and rename are whatever `Harness::actions` offers; a refusal's stderr
  stands in the footer and the row stays. A name a harness can't take stays Relay's.
- **Peek** (`tui/peek.rs`). `space` opens a pane under the list on the session under the cursor:
  its name, harness and state, then its last lines as `Harness::peek` gives them. Asked on a
  blocking thread once the cursor has rested on a row (150 ms), again every 1.5 s while it stays;
  an answer for a row the cursor has left is dropped, so a slow harness never holds up a key.
  Read only: nothing is sent to the session. `space` or `esc` shuts it.

## Harnesses

A harness is `detect/<name>.yaml` plus the code that binds it. The built-in profiles are compiled in
(`rl-detect/src/registry.rs`) and `~/.relay/detect/*.yaml` merges over them field by field; unknown
keys are an error. A profile is data (commands, arguments, words); what Relay does with them is
code, on stable contracts only: a CLI's documented words, `claude agents --json`, Codex's app-server
schema, the hooks a harness calls. Relay never reads a screen to act or types into one, so a new
harness version can turn a feature off (noticed as drift) but never misfire.

| profile key | holds |
| --- | --- |
| `kind`, `bins`, `words` | agent, shell or command; binaries to match; first words that start it |
| `prompt`, `resume` | where a first prompt goes; arguments that carry a conversation on |
| `hooks`, `sessions.hooks` | the hook dialect, the events and the settings file Relay writes them to |
| `patterns`, `timing` | regexes over a held session's screen (its state only) |
| `sessions.list` | the listing and its `reader` (`claude-agents`, `codex-threads`) |
| `sessions.attach` `start` `stop` `remove` `ping` `logs` `rename` | argv or RPC for each, with `{id}`, `{name}`, `{text}` |
| `mcp`, `settings` | where the `relay` MCP server is registered; global keys setup sets |

A profile alone adds a harness that uses integration Relay already has (held sessions, a hook
dialect, an MCP style, a listing a reader parses). A session protocol of its own, like Codex's,
needs code.

**The trait** (`rl-detect/src/harness.rs`) is a set of intents; each harness binds its own steps,
and callers ask an intent and show what came of it, never a harness's name:

```rust
pub trait Harness: Send + Sync {
    fn info(&self) -> &Info;
    fn sessions(&self, wait: Duration) -> Listed;
    fn start(&self, s: &Start) -> Result<Started>;     // Background(id) | Here(argv) | Held(args)
    fn attach(&self, s: &Session) -> Attach;           // Held | Argv | CarryOn | No
    fn actions(&self, s: &Session) -> Actions;         // stop, remove, rename
    fn stop(&self, s: &Session) -> Result<()>;
    fn remove(&self, s: &Session) -> Result<()>;
    fn rename(&self, s: &Session, name: &str) -> Renamed;
    fn notify(&self, s: &Session, mail: &Mail) -> Notified;
    fn peek(&self, s: &Session, rows: usize) -> Peek;  // Lines(text, newest last) | Nothing(why)
    fn setup(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<setup::Part>>;
    fn caller(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String>;
    fn check(&self) -> Compat;
}
```

| | Claude (`claude_native`) | Codex (`codex_native`) | held (`harness::Held`: Gemini, Antigravity, shells, …) |
| --- | --- | --- | --- |
| list | `claude agents --json --all` | app-server `thread/list`, `thread/read` | `sessions/` |
| start | `claude --bg [-n name] [prompt]` | `thread/start`, `turn/start`, then lets go | `rl-session` with the profile's argv |
| attach | `claude attach <id>` | `codex resume <id>` | Relay's own |
| stop / remove | `claude stop` / `claude rm` | `turn/interrupt` / `thread/delete` | close the holder / remove its folder |
| rename | at its next start (`-n`) | `thread/name/set` | Relay's only |
| mail | woken: `claude stop`, then `--resume <session> --bg "<line>"`; busy: its hooks | `codex queue` | its ping, else at the next `inbox` |
| peek | `claude logs <id>`, read back on a 300×100 screen | `thread/read` with its turns, in words (`›` prompt, `$` command, `~` file); never starts the daemon | the holder's screen (attach for the screen, read to `ReplayEnd`, hang up: no input, no resize) |

One open in another terminal is peeked at only where it is a thread on Codex's daemon; elsewhere the
pane says it runs there: Relay reads no other terminal's screen.

Codex is never held: JSON-RPC over a WebSocket to `codex app-server proxy`, starting the daemon if
none runs. Waking a Claude starts a new session on the same conversation; Relay carries the row
over (name, pin, colour, place, mail) and hides the old one (`ListPrefs::carry`).

## Mail and MCP

`rl mcp` is hand-written JSON-RPC 2.0 on stdio with three tools: `who`, `send(to, text)`, `inbox`.
The caller is `RL_SESSION` under a held session, else the harness's answer (`Harness::caller`), else
the `from` / `as` it passes. `send` writes a file under `mail/<key>/`, then asks the recipient's
harness to `notify` and says what came of it. `inbox` claims files by rename and moves them to
`delivered/` once the reply is written: delivery is at least once. A busy Claude hears of mail
through its hooks (`UserPromptSubmit`, `Stop`), the only output `rl hook` writes.

## Setup and drift

`rl setup` applies each harness's parts (`setup::Part`: hooks, MCP entry, settings key), each
answering "would a run change this?" from the files alone, so `--check` runs no harness. Files are
edited in place, keeping key order and comments; declined changes are remembered. On a terminal it
is the wizard (`tui::wizard`, a pure state machine); otherwise plain lines.

An expectation of a harness that fails where it is used is noticed (`rl_detect::drift::notice`) with
a stable check id, kept in `drift/events.json` with the harness and Relay versions, redacted. The
footer shows the latest; `rl doctor` lists all; `/report` posts one as a GitHub issue only on your
yes.

## Tests

Tests never touch the real `~/.relay`, `~/.claude`, `~/.codex` or `~/.gemini` (they set
`RELAY_HOME`, `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`). Codex's daemon is a stand-in
(`fixtures/stand-in-codex-daemon`); tests against the real CLIs are `#[ignore]`. CI runs
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` and the install checks.

## The README's pictures

`scripts/demo-stage` builds Relay and stages a made-up machine in a temp dir: stand-ins for
`claude`, `codex` and `agy`, projects `moonbase` (with a worktree), `pixel-pets` and `relay`, and
`PATH` cut down so no real harness runs. `scripts/screenshots` captures the list in tmux to SVG
(`scripts/ansi2svg.py`); `scripts/demo-video` records the GIF (`scripts/record.py`, needs
`rsvg-convert` and `ffmpeg`). Both fail if a real path shows up in a frame.
