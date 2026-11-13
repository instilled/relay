# Relay — architecture

[PRODUCT.md](PRODUCT.md) says what Relay is for; this file says how the tree under `code/` is wired.
It names the code to read rather than repeating it. The keys are one table in `rl/src/tui/help.rs`
(the `ctrl+h` grid, the footer's hints and setup's key card), the `/` commands one in
`rl/src/tui/slash.rs`.

## Processes

There is no daemon and no database: two kinds of process and one file tree.

```
rl (list, commands) ──Unix socket──▶ rl-session (one per held session: PTY, ring, screen, socket)
   │  └──lists, starts, stops──▶ claude agents / claude --bg · codex app-server
   ▼
~/.relay ◀── rl hook (an agent's hooks) · rl mcp (an agent's MCP server)
```

- `rl` is started by you: the list, or a command.
- `rl-session` is started by `rl new` (found next to `rl`), forks away, owns one terminal, outlives
  `rl`, and writes its session's state from the screen to `state.json`.
- `rl hook` and `rl mcp` are `rl` run by an agent. A hook writes a file and exits 0, always, quickly,
  and prints nothing but Claude's news of mail.

Folders are the truth: liveness is read from files and sockets; every write is `rl_core::atomic`.

## Crates

| crate | binary | holds |
| --- | --- | --- |
| `rl-core` | | home paths (`Home`), atomic writes, state words, time |
| `rl-session` | `rl-session` | holder, PTY backend, ring and screen, socket protocol and client, exit sidecar, state from the screen (`detect/`) |
| `rl-detect` | | profile registry, the `Harness` trait and its implementations, hook settings files, MCP entries, setup's parts and settings, drift notices |
| `rl` | `rl` | commands, list (`tui/`), rows, hooks, MCP, setup, mail, drift sink |

`rl` depends on the other three; `rl-detect` on `rl-core` and `rl-session`; `rl-session` on
`rl-core`.

## Files

`~/.relay` (`RELAY_HOME` moves it; `Home` names every path). Folders are 0700.

| path | written by | holds |
| --- | --- | --- |
| `sessions/<name>/meta.json` | `rl new` | name, harness, kind, cwd, start |
| `sessions/<name>/state.json` | holder | state read from the screen |
| `sessions/<name>/hook.json` | `rl hook` | what the agent's hooks last said (and its conversation id) |
| `sessions/<name>/exit`, `stopped`, `sock` | holder, `rl stop` | how it ended, that it was stopped, its socket |
| `state/<agent>/<session_id>.json` | `rl hook` | a session Relay does not hold; removed on `ended` |
| `mail/<key>/<ns>-<pid>-<n>-<from>.json` | `rl mcp` | a waiting message |
| `mail/<key>/delivered/` | `rl mcp` | messages read, kept 7 days (`mail::KEPT`) |
| `mail/<key>/.told` | `rl hook`, `rl mcp` | the newest mail a Claude was told of |
| `mail/<key>/.moved`, `.was` | `rl mcp` | the key a woken Claude carries on under; the keys it had before |
| `attached/<harness>-<id>` | the list | that you are attached to a native session from the list (the attach's pid) |
| `config.json` | `rl setup`, the list | harnesses, badges, `setup.declined`, list prefs (grouping, filters, order, hidden, pins, names, colours, `default`, `projects`). Strict |
| `hooks/` | `rl new` | per-run settings passed to a harness (Claude's `--settings`) |
| `detect/*.yaml` | you | profile overrides |
| `plugins/gh-pr` | `rl` | the project header's badge script |
| `drift/` | `rl` | `events.json`, `versions.json`, `codex-schema.json` |

A socket path over the OS limit (about 100 bytes) goes under `$TMPDIR/rl-<uid>-<fnv1a(home)>/`.

## The list

- **Sources.** Held sessions from `sessions/`, hooked-only ones from `state/`, Claude's and Codex's
  own from asking them (`Harness::sessions`). While the list is open each harness is asked on a task of
  its own at its own interval (`tui::live::Native`): its answer replaces only its own rows, a slow
  one holds up no other, one that fails keeps its last rows marked stale. Attaching pauses them;
  `ask_now` wakes them. `rl ls` (`cmd::listing`) and `rl mcp` ask each on a thread for at most 5s
  (`sources::list_all`); a slow or failing one is left out and named.
- **Rows.** `rows/` merges and dedupes them (a native row hides the hooked row of the same session;
  one a harness lists as open elsewhere, `NativeRow::elsewhere`, and its hooks tell of is one row
  keyed `o:<harness>:<conversation>` as before, with the harness's name and the hooks' state and
  summary, and ctrl+x only hides it, saying it runs in another terminal) and groups them under a project: a folder you named, else its git checkout (`git::project`: a
  linked worktree's `.git` followed through `commondir`; a submodule is its own). A folder you named
  or added (`/project`, `list.projects`: canonical, its checkout's project if in one) has a header
  even with no rows (`Head::empty`); by state it has none. State comes from
  the hooks, then the harness's listing, then regexes over the screen. `rows::build` runs once per
  change and is kept in `App`; the loop drains waiting input before drawing and draws only visible
  rows. The mouse is asked for clicks, drags and the wheel, not motion.
- **Command line** (`tui/cmdline.rs`). A first word a profile claims (`words`) starts that agent
  with the rest as its prompt, by its harness's `start`; `!` or the `shell` default runs the line
  held as `$SHELL -ic` with the line and then `exec $SHELL` on a line of its own (`spawn.rs`), a
  session of kind shell; `/` is a command (`tui/slash.rs`), each naming a
  key's action and running it through the same code. Completion reads `PATH` once. Multi-line input:
  `shift+enter` where the terminal tells it apart (Relay asks for the kitty keyboard protocol),
  `alt+enter`, or `ctrl+j` (what iTerm2 sends when mapped for Claude Code's `shift+enter`, so it
  works through tmux).
- **Attach.** A held session: connect to its socket, get the screen, then live bytes; `ctrl+\`
  detaches. A native one: run its `attach` argv (`claude attach <id>`, `codex resume <id>`) in this
  terminal (`attach::native::hold`), marked under `attached/` while it runs; it ends when the
  harness's attach does. Which of these a row gets is `Harness::attach`'s answer (`Attach::Held`,
  `Argv`, `CarryOn`: an idle conversation open elsewhere carried on in the harness's background or
  held, `No`).
- **Stop and remove.** The second `ctrl+x` puts one question in the footer for every harness
  (`tui::remove::Ask`): `s stop` while it runs, `r remove`, `enter` the first offered; what is
  offered is `Harness::actions`. A row no harness can remove is hidden instead. A held stop marks
  `stopped` and closes the holder (SIGHUP, 3 s; then SIGTERM, 2 s; then SIGKILL, so it always ends,
  its exit written within the 10 s `rl stop` waits), a held remove stops it then removes its folder
  (`cmd::rm`), both through the holder `rl` installs; a native one's are its harness's own
  (`Harness::stop` / `Harness::remove`). A refusal's stderr (`harness::said`) stands in the footer
  and the row stays. `/rm` asks the remove alone; `/stop` stops at once.
- **Rename.** The name goes into `config.json`'s `names`; where `Harness::actions` says `Now`, the
  list runs `Harness::rename` in the background (Codex's `thread/name/set`). Done, the `names` entry
  goes; refused, the name stays Relay's and the footer says why. `AtNextStart` (Claude's): Relay
  keeps it and passes it as `-n` when it next starts the session (a carry-on, a wake); the footer
  says so. Whenever a listing names a row as `names` does, the entry goes (`App::heard`). An empty
  name clears a Codex thread's name on the daemon; a Claude's only in Relay.
- **Hooks.** `rl hook <agent> <Event>` reads the hook JSON and the profile's `sessions.hooks.events`
  and writes `hook.json` (under a held session, `RL_SESSION` set) or `state/<agent>/<id>.json`.
  The user-level hook says `--user`; under a held session only the session's own hook is heard.

## Harnesses

A harness is `detect/<name>.yaml` plus the code that binds it. `rl-detect/src/registry.rs` compiles
the built-in profiles in and merges `$RELAY_HOME/detect/*.yaml` over them field by field; `generic`
is the floor. Every struct is `deny_unknown_fields`. A profile is data — commands, arguments, words;
what Relay does with them is code, and Relay depends only on stable contracts: a CLI's documented
words and exit status, `claude agents --json`, Codex's schema'd app-server protocol, the hooks a
harness calls. It never reads a harness's screen to act: a new harness version degrades a feature
at worst (noticed as drift), never misfires.

| key | holds |
| --- | --- |
| `kind` | `agent`, `shell`, `command` or `plugin` |
| `bins` | binary names to match |
| `words` | first words of the command line that start it (first profile by name wins) |
| `prompt` | where a first prompt goes, with `{prompt}` (`["-i", "{prompt}"]`); default last |
| `interactive_only`, `icon`, `color` | |
| `hooks` | `dialect`, `args` (what `rl new` passes; `{hooks_file}`) |
| `resume` | arguments that carry a conversation on, with `{session}` |
| `patterns`, `timing` | `needs_input`, `idle` regexes over a held session's screen (its state only); `idle_after`, `stale_after` |
| `sessions.hooks` | `file`, `timeout`, `unit`, `async`, `marker`, `after_install`, `from_env`, `events`, `settings` |
| `sessions.list` | `args`, `reader` (`claude-agents` or `codex-threads`: which implementation binds it) |
| `sessions.attach` `start` `name` `stop` `remove` | argv with `{id}`, `{name}` (Claude's remove `[rm, "{id}"]`) |
| `sessions.ping` | what tells a session of mail from outside, with `{id}`, `{text}` |
| `sessions.rename` | `rpc`: the daemon method that renames one from outside (Codex's) |
| `mcp` | `file` (first whose variables are set), `cli` (the harness's own add/remove words), `note` |
| `settings` | `file`, `set`: `{key, value, check}` keys set in the harness's global config |

A profile alone adds a harness that uses integration Relay has: held sessions, a hook dialect it
reads, an MCP style it writes, a listing one of the readers parses, a ping. A session protocol of
its own (as Codex's app-server) needs an implementation of `Harness`.

**The trait** (`rl-detect/src/harness.rs`) is one set of intents; each harness binds its own steps
behind them, and callers (the list, `rl mcp`, `rl ls`, `rl new`, setup, `rl hook`, `rl doctor`) ask
an intent and show what came of it. They never compose steps nor ask a harness's name or kind.

```rust
pub trait Harness: Send + Sync {
    fn info(&self) -> &Info;                          // name, kind, icon, colour, bin, found, mail_by_hooks; version()
    fn sessions(&self, wait: Duration) -> Listed;     // Rows(Vec<NativeRow>) | Unavailable(why); none for a held one
    fn start(&self, s: &Start) -> Result<Started>;    // Start{cwd, name, prompt, resume, words} → Background(id) | Here(argv) | Held(args)
    fn attach(&self, s: &Session) -> Attach;          // Held | Argv(argv) | CarryOn(Start) | No
    fn actions(&self, s: &Session) -> Actions;        // what the list may offer: stop, remove, rename (Now | AtNextStart | RelayOnly)
    fn stop(&self, s: &Session) -> Result<()>;
    fn remove(&self, s: &Session) -> Result<()>;      // one still going stopped first
    fn rename(&self, s: &Session, name: &str) -> Renamed;    // Now | AtNextStart | RelayOnly(why)
    fn notify(&self, s: &Session, mail: &Mail) -> Notified;  // Woken | Queued | ByHooks(why) | AtNextInbox | Failed(why)
    fn setup(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<setup::Part>>; // each need / apply / remove
    fn caller(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String>; // who calls `rl mcp`
    fn check(&self) -> Compat;                        // self-check against what Relay uses; what is off is drift
}
```

A `Session` is every row a harness owns, whoever runs it: its harness, `owner` (`Relay { live,
dir }` held, `Own` in the harness's background, `Elsewhere { listed }` open in a terminal of its own
and told of by hooks), its id, conversation id, folder, state and words, its name, the name you gave
it in Relay, and whether you are attached to it from the list. `sources::rows` builds one per row.
`harness::of(profile, bin)` picks the implementation by `sessions.list.reader`;
`harness::harnesses` gives every profile's; `sources::harness_of` falls back to `generic`'s for a
held command no profile names. Relay's own sessions are stopped and removed through the holder `rl`
installs (`harness::set_holder`, `cmd::Store`). Runs go through the bounded `harness::run`.

| intent | `claude_native::Claude` | `codex_native::Codex` | `harness::Held` (Gemini, Antigravity, cursor, opencode, shells, `generic`) |
| --- | --- | --- | --- |
| `sessions` | `claude agents --json --all` plus each session's job file; one open in a terminal (`kind: interactive`) as `elsewhere`, by its conversation id, with its name and `status` | `thread/loaded/list`, `thread/read`, plus Relay's unloaded threads (`thread/list`, originator `relay`) as `resting`; empty threads skipped | none |
| `start` | `claude --bg [-n name] [prompt]`, or `claude --resume <session> --bg [-n name]` to carry one on; the id it printed once listed (`begin`). `words`: held as given | `thread/start {cwd}`, `thread/name/set`, `turn/start`, then lets go; no prompt: `Here(codex)`; carry on: `Here(codex resume <id>)`; a flag in `words` refused | `Held` with the profile's `prompt`, `resume` or the words given |
| `attach` | own: `claude attach <id>`; elsewhere, idle, once Claude has listed: `CarryOn` in its background with your name | own, or elsewhere idle: `codex resume <id>` | held: Relay's attach; elsewhere idle with a `resume`: `CarryOn`, held |
| `actions` | stop while not ended; remove; rename `AtNextStart` | stop while a turn runs; remove; rename `Now` — each where the schema still has it | held: stop while live, remove; rename `RelayOnly` |
| `stop` / `remove` | `claude stop <id>` / `claude rm <id>` (never `--discard-unpushed` / `--force-remove-worktree`) | `turn/interrupt` / `thread/delete` (interrupting first) | holder closed / stopped if live, folder removed |
| `rename` | Relay keeps it, `-n` at its next start or carry-on → `AtNextStart` | `thread/name/set` → `Now`; else `RelayOnly(why)` | `RelayOnly` |
| `notify` | working / needs input / you attached: `ByHooks`; idle: `claude stop`, then `--resume <session> --bg [-n name] "<line>"` → `Woken`; ended: the resume alone; held or elsewhere: `ByHooks` | `codex queue --thread <id> --message <line> --resume` → `Queued` | `sessions.ping` if any, else `AtNextInbox` |
| `setup` | hooks; MCP through `claude mcp add --scope user`; `leftArrowOpensAgents: false` | hooks; `[mcp_servers.relay]` in `config.toml` with `env_vars` | hooks; `mcpServers.relay` in `mcp.file`, or `mcp.cli` (`agy mcp add`) |
| `caller` | `CLAUDE_CODE_SESSION_ID` | — | — |
| `check` | `claude agents --json` read for its shape | `generate-json-schema` against `USES` (`compat`) | its version only |

Codex is never held: JSON-RPC over one WebSocket to `codex app-server proxy` (`codex_native::connect`;
`codex app-server daemon start` first if none runs), introducing itself as `relay`. The daemon runs
a turn with no client; an approval leaves it `active` / `waitingOnApproval` until `codex resume`.
`rl new --harness codex` takes the same `start` and prints the thread id.

A held session runs what its harness's `start` said, with its hooks' settings where its profile has
a dialect (`harness::held_argv`: Claude's `--settings`).

## Drift

An expectation of a harness that fails in use is noticed where it fails (`rl_detect::drift::notice`)
with a stable check id, and kept by the sink `rl` installs (`rl::drift::install`) in
`drift/events.json`: one event per check id and harness version, counted, first and last seen,
with the harness version (asked once per binary path and mtime), Relay's version and commit, and
the OS. Without a sink (tests) notices go nowhere; `drift::capture` collects them.

| check id | noticed in |
| --- | --- |
| `claude.agents_json` | `Claude::list_within` and `check`: not a list of objects, or entries lacking `REQUIRED` fields (left off the list) |
| `claude.bg_id` | `claude_native::begin`: `--bg` printed no id |
| `codex.schema.<method>` | `codex_native::compat`: once per binary, `generate-json-schema` against `USES`; what is gone turns off its `Feature` |
| `codex.rpc.<method>` | `Rpc::call_all`: JSON-RPC -32601 or -32602 |
| `<harness>.cli.<word>` | `harness::run`: a usage error |

Evidence is one structural line, redacted (`drift::redact`: home as `~`, paths as `<project>`,
quoted text as `…`). The footer leads with the latest pending event; `rl setup --check` lists them
under `drift:`; `rl doctor` lists all, and each harness's `check` (what is `off`). `rl report` / `/report` renders one issue per event
(`drift::title` carries check id and version, so `gh issue list --search` finds an open one to
comment on), shows it, and posts only on a yes through `gh` (`--label drift`, retried without),
else prints a new-issue link cut to `MAX_URL`. The repository is Cargo.toml's `repository`.

## Mail and MCP

`rl mcp` is hand-written JSON-RPC 2.0, one message per line on stdio (logs to stderr): `initialize`,
`ping`, `tools/list`, `tools/call`. Tools: `who`, `send(to, text, from?)`, `inbox(as?, recent?)`; a
failure is an `isError` result.

**Caller**, in order: `RL_SESSION` when `RL_SESSION_AGENT` is the server's `--harness`; else the
harness's own answer (`Harness::caller`, Claude's `CLAUDE_CODE_SESSION_ID`) matched
against the list; else `from` / `as`. A known caller wins. **Addresses**: a held session's name, a
hooked-only one's session id, a native one's name (or id when empty or repeated); `send` also takes
the row's title or a conversation id. A send to a held name asks no harness.

**Send** writes a file under `mail/<key>/` (a held session's name, else its conversation id; keys
encoded one to one, `[a-z0-9-]` kept, every other byte `%xx`). **Inbox** takes from every key the
caller has, oldest first, claiming each by a rename; once the reply is written the claims move to
`delivered/`, and if the write fails they go back. Claims left by a dead server are released by the
next `take` (`release_stale`): delivery is at least once. Delivered files older than 7 days are swept
on every call; `inbox(recent: true)` reads them and moves nothing.

**Telling the recipient**, after the file is left, is the recipient's harness's `notify`, given the
session (its owner, state, the name you gave it, whether you are attached) and who sends. `send`
says what came of it in its text; none is a failure:

- **Queued** (Codex's `sessions.ping`, `codex queue --thread <id> --message <line> --resume`), and
  any harness's ping, run with `RELAY_ASKING` set within `PING_WAIT`; the id is a native row's or a
  held session's `hook.json`. A held harness with no ping: `AtNextInbox`.
- **Woken** (Claude's, a session in its background): idle, it is stopped (`claude stop <id>`) and
  carried on with `claude --resume <session> --bg [-n name] "<line>"`; ended, the resume alone. The
  line names the sender and asks it to call the inbox tool. `send` records it as told
  (`mail::set_told`). Claude's resume starts a new background session on the same conversation
  (`Woken(Some(Forked))`: a new id and conversation id), so `send` carries the row over: the old
  row's name, pin, colour and place become the new row's and the old row is hidden as `/hide` does
  (`ListPrefs::carry`; never `claude rm`, which would take its worktree), and its mail folder
  forwards to the new one (`mail::moved`: `.moved` in the old, `.was` in the new, so the new one
  reads what was left for the old and mail to the old address reaches the new). The list reads
  `config.json` again when it changes (`App::heard_config`). A carry-on from the list
  (`Attach::CarryOn`) carries its row over the same way.
- **ByHooks** (Claude's, `info().mail_by_hooks`): working, waiting on you, or attached from the list
  (`attached/`), it is left to the hooks, and `send` says when it will see it. `rl hook claude
  UserPromptSubmit` prints `additionalContext` while mail waits; `rl hook claude Stop` prints
  `{"decision":"block",…}` for mail newer than `.told`, never with `stop_hook_active`. Only the heard
  copy of the hook prints. These are the only output `rl hook` writes.
- **Failed**: what the harness said. From a Claude caller to a Claude Relay could not wake, it adds
  Claude's `SendMessage` by the name `claude agents` shows.

## Setup

`rl setup` applies each harness's parts (`Harness::setup` → `setup::Part`: `Hooks`, `Mcp`,
`Setting`, each with `need` / `apply` / `remove`). It registers `rl mcp --harness <h>` where the
harness says; a non-default
home adds `RELAY_HOME`. `claude mcp add` runs with a timeout in its own process group;
`~/.claude.json` is only read. Hook files are edited in place keeping key order, TOML comments and
mode; a file that does not parse is left alone and reported. Then the profile's `settings` keys
(Claude's `leftArrowOpensAgents: false`), written by `rl_detect::settings` after `claude mcp` has
written the file, keeping every other key; `--remove` leaves them.

Each part (`user_hooks::install_needed`, `mcp_entry::Place::need`, `settings::Place::need`) first
answers "would a run change this?" from the files alone; a run writes only that, and `--check`
(`setup::check`) stops there, running no harness CLI (agy's entries read from its
`mcp_config.json`). Declined changes are `setup.declined` ids (`claude/settings/leftArrowOpensAgents`),
left out by `Place::without`. The list runs the check off its UI thread at start and leads the footer
with `setup needed` while anything does.

On a terminal, `rl setup`, the list's first start (no `config.json`, not inside a held session) and
`/setup` are the wizard (`tui::wizard`): a pure state machine (`wizard::model`, keys in,
`Effect::Apply` out) over `Host::need_of` and `setup::apply` (on a thread, `wizard::engine`), drawn
in a ratatui inline viewport, its summary left in the scrollback; `/setup` hands the terminal over as
an attach does. Otherwise (`--yes`, `--check`, `--remove`, `--harness`, no terminal, a held session)
`setup::run` prints plain lines. The first start writes `config.json` whatever the answer.

## Not verified live

None of these is exercised by the tests:

- `claude rm <id>` (its `--help` read on 2.1.322) and what it prints when it refuses.
- `turn/interrupt` and `thread/delete` on a live thread (codex 0.167's schema), and `codex
  app-server daemon start`.
- The Gemini MCP entry; whether `agy` passes `RL_SESSION` to its MCP servers.
- Antigravity's screen: only `agy --help` was read.
- Claude's hooks firing on a session it started with `--resume … --bg` (a woken one).

Checked live on Claude 2.1.322: `claude --bg -n <name> "<prompt>"` prints the id and is listed
idle once done; `claude stop <id>` leaves it listed as done; `claude --resume <session> --bg -n
<name> "<line>"` carries the conversation on in a new session (new id and session id, the name
applied, the earlier turns in its transcript) while the old one stays listed; `claude rm <id>`
removes either.

Checked live on codex 0.167: a turn runs with no client; an idle thread unloads after about two
minutes; an approval waits for `codex resume`; the thread's `originator` is `relay`.

## Tests and gates

Live tests against real CLIs are `#[ignore]`. Codex's daemon in tests is a stand-in
(`fixtures/stand-in-codex-daemon`, Python 3) speaking the WebSocket and JSON-RPC, copied to
`target/debug/deps/stand-ins/<pid>/`. Tests never touch the real `~/.relay`, `~/.claude`, `~/.codex`
or `~/.gemini`; they set `RELAY_HOME`, `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`. CI runs
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace`.
