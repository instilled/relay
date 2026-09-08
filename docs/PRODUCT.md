# Relay — product brief

**Status:** draft for kickoff · **Date:** 2026-09-08 · **Authors:** Fabio, Claude

Relay (command: `rl`) is the fresh rebuild of ax: a native daemon and terminal control panel that keeps agents and long-running work alive when you are gone, tells you what they need and what they did, runs work on a schedule, and is extended by small plugins that run in their own process — plugins that agents can write.

It is a mashup: herdr's persistence and any-CLI ergonomics, old ax's daemon, journal and watch/join, plus three things neither has — a return digest, trustworthy work summaries, and a scheduler — on top of one noun: the **run**.

> **We start fresh.** This is a greenfield rebuild, not a refactor. The old code base is `instilled/ax` (local checkout `~/Projects/ax`), frozen as the legacy reference; it is a source of ideas and a few designs, never a code base to build on. Nothing is refactored in place, no module is "kept"; whatever survives is re-implemented from an empty crate tree against this brief. The socket-plus-daemon shape is the only thing carried over as given. The name changes too: the product is **Relay**, the command is `rl` (see §0).

---

## 0. Name

**Relay**, command **`rl`**, daemon **`rld`**. You hand the baton off, the machine runs the leg, it hands back with the result — both directions, which is the point. `rl` keeps the two-letter command that made `ax` pleasant; directories use the full word (`.relay/`, `~/.relay/`), binaries the short one (`rl`, `rld`, `rl-session`, `rl-plugin-host`), crates and env vars the full one (`relay-proto`, `RELAY_REF`).

Collisions accepted: Facebook's Relay (GraphQL) owns `relay` on npm and most search results, so the repo is `relay-dev` (or similar) and crates are `relay-*`; a small `rl` ("randomize lines") utility exists in Debian/Homebrew; "RL" reads as reinforcement learning for a beat. `rl` is not a typo-neighbour of `rm` (`l` and `m` are on different rows and fingers) and Relay never deletes files, so the worst case in either direction is a usage error.

**Home directory.** Everything user-level lives under `~/.relay/` — one place, discoverable, next to `~/.claude`, `~/.codex`, `~/.cursor`. No XDG split; `RELAY_HOME` overrides the location for anyone who wants it elsewhere.

```text
~/.relay/
  config.yaml        user defaults (worktrees, presence, notify)
  plugins/           user plugins
  schedules.yaml     user schedules
  ledger.db          SQLite ledger (runs, events, sessions, …)
  run/               rld.sock, session sockets, pids, exit sidecars
  log/               rld and plugin-host logs
```

Per-repo state is `.relay/` in the repo (`config.yaml`, `plugins/`, `schedules.yaml`); it is versioned and never holds runtime state.

"ax" from here on means the old code base in `instilled/ax`, frozen as reference.

## 1. Why a fresh start

**What we take from herdr.** A daemon holds real terminals so closing the laptop costs nothing. You get back in from any tty. Any agent CLI is detected and tracked with zero configuration. A socket API and a plugin ecosystem make the UI one client among many.

**What we take from old ax, as designs to re-implement.** Daemon + Unix socket + CLI/TUI as separate clients. PTY-owned sessions with `watch`/`join`. A SQLite journal as durable memory. The bundled-skill pattern (`ax-plan`) for teaching agents a contract. Attention-ranked fleet cards.

**What we leave in `legacy`.** Everything built to run agents inside an image: the compose backend, worker image, git shim, provider proxy, host-auth material, autobind/ports, the capability/grant engine, ATP. The `change` entity and `.ax/changes/` contract. The pane/layout registry. None of it is ported.

**What we reject from herdr.** The terminal as the unit. State inferred only by scraping output. Restoring *layouts* instead of restoring *attention*. No memory of what happened while you were away — the exact gap this rebuild fills.

**Why not refactor.** Roughly half of the ~20k lines exist to punch holes out of a container. The rest is shaped by that. A fresh tree with this brief as the spec is faster than deleting toward it, and it lets the first commit already have the right nouns.

---

## 2. Principles

1. **One noun: the run.** Agent session, shell, command, plugin invocation — all runs. One lifecycle, one journal, one screen. Everything else is a field on a run or a query over runs.
2. **Native.** No images, no proxies, no shims. Agent CLIs run as designed, with their own auth and config.
3. **Signals over scraping, scraping as the floor.** Exact state from hooks, transcripts and git where a CLI offers them; PTY heuristics for everything else; confidence is always visible.
4. **Ledger first, narrative second.** Facts (files, commits, commands, cost) are deterministic and always shown; the model-written summary is optional and cached.
5. **The first screen is "since you left."** Fleet is the second screen. The terminal is one keystroke away.
6. **The daemon is a client of sessions, not their owner.** `rl daemon restart` never kills work.
7. **Relay is a runtime and a memory, not a tracker.** Work has no state inside Relay. Git owns branches, GitHub owns review, your tracker owns intent and done-ness. Relay groups runs by a key and links out.
8. **Core stays small; plugins do the rest.** Notifications, summarisers, PR openers, tracker sync, nightly jobs are plugins. Plugins run out-of-process and only have a channel, never power.
9. **The system may extend itself, under review.** Agents can write plugins; a human enables them.
10. **Fresh start.** No code is refactored in place. Old ax is a reference, not a base.

---

## 3. Core model

```text
workspace   a directory Relay manages (a repo, or one checkout inside your isolated box)
run         anything rld starts and owns: agent | shell | command | plugin
            fields: kind, title, cwd, branch, worktree, ref?, tags[], state, source,
                    confidence, cost, started/ended, exit, schedule?, plugin?
ref         optional, exactly one, opaque string with an optional scheme prefix
            (notion:FL-123, gh:42). "What is this run part of." Never interpreted by Relay;
            plugins may claim a scheme.
tags        zero or many labels, key:value allowed (nightly, customer:tend).
            "How do I filter." Never interpreted by Relay.
group       DERIVED, never stored: ref → branch (+worktree) → workspace.
            The digest, facts rollup, summary cache, budgets and plugin context all key on it.
event       an append-only fact: run.started, run.blocked, run.needs_input, run.finished,
            run.failed, commit.landed, branch.done, schedule.fired, plugin.invoked, …
schedule    cron/interval → a run spec (command | agent brief | plugin command), with
            default tags and a branch-naming rule
plugin      a manifest + TypeScript, in .relay/plugins/ (repo) or ~/.relay/plugins/ (user),
            executed by rl-plugin-host in its own process, talking to rld over IPC
presence    last_seen_at per client; the boundary between "now" and "since"
```

`ref` and `tags` are set by flag (`--ref`, `--tag` repeatable), inherited by environment (`RELAY_REF`, `RELAY_TAGS`) so sub-runs started by agents or plugins carry them, or derived by a per-repo rule in `.relay/config.yaml` (e.g. branch `fl-123-auth-fix` → `notion:FL-123`).

```text
 ┌───────────── rl (CLI / TUI, or ssh + rl) ─────────────┐
 │ since-you-left · fleet · run detail · terminal (join) │
 └───────────────────────┬───────────────────────────────┘
                         │ unix socket, JSON-RPC
 ┌───────────────────────▼───────────────────────────────┐
 │ rld                                                   │
 │  run supervisor · scheduler · event ledger · presence  │
 │  detection registry · plugin broker · digest · facts  │
 └──┬──────────────┬──────────────┬──────────────────────┘
    │ attach       │ spawn        │ IPC (stdio JSON-RPC)
 ┌──▼───────┐  ┌───▼────────┐  ┌──▼─────────────────────┐
 │ session  │  │ command    │  │ rl-plugin-host (bun)   │
 │ holder   │  │ (pipes)    │  │ one per enabled plugin │
 │ (PTY)    │  └────────────┘  └────────────────────────┘
 │ claude / codex / cursor / sh                          │
 └───────────────────────────────────────────────────────┘
 signals in: hooks → socket · ~/.claude transcripts · git · PTY heuristics
```

---

## 4. Jobs to be done → features

Each job: **situation → want → so that**, then the herdr / old-ax baseline, the work to be done, and what "done" means.

### Leaving

#### J1 · Nothing dies when I leave

**Job.** When I close the laptop, switch machines, or my ssh drops, I want everything I started to keep running untouched, so that leaving costs nothing.

**Baseline.** herdr: holds PTYs in-process, checkpoints to survive restarts. Old ax: `axd` owned PTYs; a restart killed sessions.

**Work to be done.**
- `rl-session`: a tiny detached process (double-fork, `setsid`) that holds the PTY master for one session and exposes a two-message socket protocol (`attach`, `resize`) plus an output ring buffer. `rld` is a client that attaches on start and re-attaches on restart.
- Scrollback replay on attach from journaled `session_io`.
- `rl daemon restart` / upgrade never touches holder sockets.
- Command runs (non-PTY) use the same holder with pipes, so a scheduled `cargo test` survives too.

**Done when.** `kill -9 rld` while an agent is mid-turn; `rl daemon start`; `rl join` shows the same session with intact scrollback and the agent never noticed.

#### J2 · Hand off and forget

**Job.** When I kick off an agent or a long command, I want to hand it off and forget it, so that I do not babysit terminals.

**Baseline.** herdr: start in a pane, detach. Old ax: `ax plan` / `ax change dispatch`; `ax exec` one-shot only.

**Work to be done.**
- `rl run [-b <branch>|--worktree] [--ref <id>] [--tag <t>]… -- <cmd…>` (detached command run).
- `rl run --agent <cli> [same flags] [prompt]`. There is no separate "wrap" command: `rl run -- <anything>` always allocates a PTY under the holder, so an unknown CLI is just a run with heuristic-only state; `--no-tty` (pipes) is for headless/scheduled runs.
- Every run ends in a terminal state (`finished`, `failed`, `stopped`) with exit code, duration and cost in the ledger.

**Done when.** `rl run --tag e2e -- make e2e` returns immediately with a run id; an hour later `rl since` shows it finished with exit 0 and how long it took.

### Away

#### J3 · Tell me only when it needs me

**Job.** When something needs me — a permission, a question, a failure — I want to be told, and only then, so that I am not polling.

**Baseline.** herdr: shows blocked in the sidebar; no push. Old ax: `waiting_human`/`blocked` fed by a container ask pipeline (not ported).

**Work to be done.**
- `run.blocked` / `run.needs_input` / `run.failed` events with a reason and a confidence.
- A `notify` IPC capability for plugins; reference `notify-*` plugins (macOS notification, ntfy/Slack/Telegram).
- Debounce and dedupe in the broker: one notification per blocking episode, cleared on `run.resumed`.

**Done when.** Claude asks a permission question while I am out; my phone gets one message with the question text and the run title; answering via `rl join` from ssh clears it.

#### J4 · Work keeps starting without me

**Job.** When I am away, I want work to keep starting on its own on a schedule — a command, an agent on a brief, or a plugin — so that the machine works my hours and the other ones.

**Baseline.** Neither.

**Work to be done.**
- `schedules` + `schedule_runs` tables; scheduler loop in `rld`.
- Run spec enum: `command` (argv, cwd, env allow-list), `agent` (cli, prompt or prompt file, headless mode e.g. `claude -p … --output-format stream-json`), `plugin` (plugin id + command + args).
- Per schedule: default `tags`, optional `ref`, and a `branch` template (`deps/{date}`) so repeated agent runs are distinguishable and land on their own branch/worktree.
- Declarative `.relay/schedules.yaml` in the repo, hot-reloaded on `rl up`; `~/.relay/schedules.yaml` for user-level. Repo schedules require the same approval as plugins before they fire.
- `rl schedule ls|add|rm|pause|resume|run <id>`.
- Missed-run policy per schedule: `skip` or `catch-up-once`, evaluated on daemon start and wake-from-sleep.
- Overlap policy: `skip|queue|kill`.
- Every fired schedule is a normal run → appears in J5's digest for free.

**Done when.** A nightly `cargo test` schedule and a Monday "bump dependencies" agent schedule exist in the repo; both show up in the morning digest with their outcome under `nightly` / `deps/2026-09-14`; a missed night after sleep follows the declared policy.

### Coming back

#### J5 · Tell me what happened, in the order I should look

**Job.** When I come back after any gap, I want to know what happened, what changed, and what is waiting on me, in the order I should look at it, so that I resume in one minute, not ten.

**Baseline.** Neither.

**Work to be done.**
- `presence` table: `last_seen_at` per client id, updated on dashboard/`join`/`watch` connect and heartbeat.
- `events` table (append-only): `id, at, workspace, run, group_key, kind, summary, ref_json`. Every writer in `rld` also appends here.
- Digest query: events since `last_seen_at`, grouped by group key, ranked: needs-input/asks → failed → finished-unreviewed → still working → schedule outcomes → commits landed.
- TUI: the digest is the first screen whenever there is at least one unseen attention item, otherwise fleet (see §9.1). `rl since [--json] [--since <ts>] [--tag …] [--ref …]` for ssh/phone.
- Leaving the digest screen advances `last_seen_at`; items can be pinned.

**Done when.** After a night away, opening `rl` shows: 1 question pending on `auth-fix`, 1 run failed under `nightly` (exit 1, link to output), 2 branches with new commits, everything else collapsed — and nothing else on screen.

#### J6 · Give me a trustworthy account before a narrative

**Job.** When an agent or scheduled run finishes, I want a trustworthy account of what it did — files, commits, commands, cost — before any narrative, so that I can decide to merge, redo, or drop without reading the transcript.

**Baseline.** herdr: nothing. Old ax: worker-written `report.md`; journal had execs and pane I/O.

**Work to be done.**
- Facts collector per run and per group: commits on the branch vs. base, `git diff --stat`, commands executed with exit codes, questions raised/answered, wall-clock, tokens/cost.
- Sources: git (native), Claude Code transcripts `~/.claude/projects/<slug>/*.jsonl` (tool calls, files touched, usage), Codex/other session logs where they exist, PTY output as the floor.
- Narrative summariser as a reference plugin: reacts to `run.finished`, calls `claude -p` with the facts, stores the summary in the ledger keyed by run and by group.
- Run detail view: facts on top, narrative below (marked model-written), transcript/terminal behind `enter`.

**Done when.** For a finished branch I can see, without opening a terminal: 3 commits (+412/−88 across 7 files), 14 commands (2 failed), 1 question, 41 minutes, $1.30 — and a three-sentence summary marked as model-written.

### Steering

#### J7 · Be in its terminal in one keystroke

**Job.** When I want to see what an agent is doing right now, I want to be in its real terminal in one keystroke, read-only or hands-on, so that observation and takeover are the same gesture.

**Baseline.** herdr: click agent → live terminal. Old ax: `ax watch` / `ax join`.

**Work to be done.**
- `enter` on any run opens watch; any keypress escalates to join; `esc` returns.
- Attach via the session holder (J1), replay scrollback, forward resize.
- Multi-client: several watchers, one joiner; the joiner is visible to others.
- Works over ssh with no extra protocol: `ssh box rl`.

**Done when.** From the digest, `enter` on a blocked run shows Claude's permission prompt; `y` answers it; `esc` returns to the digest with the item cleared.

#### J8 · Rank by attention, not by layout

**Job.** When several things run at once, I want to see them ranked by attention, not by pane layout, so that I always act on the right one first.

**Baseline.** herdr: sidebar with status dots, layout restore. Old ax: fleet cards with `Blocked > Failed > Working > Done > Idle`.

**Work to be done.**
- Fleet screen: rows not panes; sort by attention score (needs-input, failed, stale-working, finished-unreviewed, working, idle); group by group key, then workspace.
- Row: state + confidence, age in state, last meaningful line (transcript or PTY), cost so far, tags, plugin-contributed action keys.
- No layout persistence. Persist *selection* and *filters* only.
- Look and feel: monochrome by default, colour only for attention states; one line per run; no boxes inside boxes; the footer is the only chrome.

**Done when.** With six runs across three branches, the one that needs me is the top row and the screen fits in 80×24.

### Starting

#### J9 · Everything hangs off something I can name

**Job.** When I start a piece of work, I want everything the machine does for it to hang off something I can name and find later, so that runs, facts and summaries are never loose.

**Baseline.** Old ax: a `change` entity with its own state machine and `.ax/changes/` files (left in `legacy`; in practice every brief and report was empty).

**Work to be done.**
- No unit-of-work entity. The group key (`ref → branch → workspace`) is the name.
- `rl run -b <branch>` creates the branch (and worktree if `--worktree` or the repo default says so) and starts the run in it; `rl done <branch>` offers merge/rebase and removes the worktree, emitting `branch.done`.
- `.relay/config.yaml`: `worktrees: always|never|ask`, `ref.from_branch: <regex → template>`.
- Intent lives in the prompt, the branch name, or the tracker; Relay reads all three, stores none as state.

**Done when.** `rl run -b fl-123-auth-fix --worktree --agent claude "fix the token refresh"` yields a run whose group is `notion:FL-123` (derived from the branch rule); the digest, facts and summary for that group are one row.

#### J10 · Any CLI, zero configuration

**Job.** When I use whichever CLI I want — Claude, Codex, Cursor, a plain shell — I want Relay to recognise it and track it without configuration, so that the tool is never in the way.

**Baseline.** herdr: 21 CLIs auto-detected. Old ax: Claude only, via container bootstrap.

**Work to be done.**
- Detection registry (`detect/*.toml`, user-overridable): binary names, argv patterns, per-CLI signal sources (hooks available? transcript path? prompt regexes for idle/blocked?).
- Generic PTY heuristics as the floor: output-idle timers, prompt-line patterns, "waiting for input" patterns, exit.
- Exact signals where offered: Claude Code hooks (`Stop`, `Notification`, `PreToolUse`, `SessionEnd`) seeded into the project `.claude/settings.json`, posting to the rld socket; transcript tailing for Claude; Codex equivalents as they exist.
- `RELAY_REF` / `RELAY_TAGS` exported into every run's environment so sub-runs inherit their group.

**Done when.** Starting `claude`, `codex` and `bash` under Relay shows three runs with correct, differently-sourced states; adding a new CLI is one TOML file.

### Trusting

#### J11 · Real signals, visible confidence

**Job.** When I look at Relay's status or summary, I want it to come from real signals, with a visible confidence when it is a guess, so that I never act on a wrong "idle".

**Baseline.** herdr: scraped, no confidence. Old ax: driven by an ask pipeline that does not exist natively.

**Work to be done.**
- Every state transition carries `source: hook|transcript|git|pty|manual` and `confidence: exact|likely|guess`.
- TUI renders guesses distinctly (e.g. dim `idle?`); digest ranks exact signals above guesses.
- `stale` state when a "working" run has produced nothing for N minutes.
- Manual override: `rl run mark <id> done|blocked` for CLIs with no signals at all.

**Done when.** A CLI with only PTY heuristics shows `idle?`; Claude with hooks shows `blocked` with the question text; the difference is obvious without reading docs.

#### J12 · The TUI is one client

**Job.** When I want to script or extend Relay, I want the same socket API the TUI uses, exposed as a small typed SDK, so that the TUI is one client among many.

**Baseline.** herdr: socket API + plugins. Old ax: shared request/response types between CLI and TUI.

**Work to be done.**
- Versioned, documented JSON-RPC over the socket from day one; `--json` on every CLI command.
- `rl events --follow` for anything that wants to react.
- The plugin SDK (`relay.d.ts`, §5) is a thin typed wrapper over the same messages, brokered rather than direct.

**Done when.** A shell script and a plugin can both list runs, start one, and subscribe to events, using the same documented messages.

### Extending

#### J13 · Bend Relay with a few lines of code

**Job.** When Relay does not do something I need, I want to write a few lines of TypeScript that run inside Relay with access to my installed tools, so that Relay bends to my workflow instead of me waiting for a feature.

**Baseline.** herdr: plugin ecosystem (986). Old ax: none.

**Work to be done.** See §5. Manifest + `index.ts`, `rl plugin new|enable|disable|ls|run|dev`, commands invocable from CLI (`rl x <plugin> <command>`) and TUI, brokered capabilities.

**Done when.** A 30-line plugin that runs `gh pr create` for the current branch, with `tools: [gh]` declared, works from `rl x pr open` and from the `p` key on a group row.

#### J14 · Code runs in response to events

**Job.** When something happens — an agent blocks, a run finishes, a schedule fires, a commit lands — I want code to run in response, so that follow-ups happen without me.

**Baseline.** Neither.

**Work to be done.**
- `events` capability in the manifest with an allow-list of kinds; resident plugin hosts receive `event` messages; at-least-once delivery with ack and a replay cursor so a restarted host catches up.
- Reference reactions: notify-on-blocked (J3), summarise-on-finished (J6), open-PR-on-`branch.done`, `notion-sync` (posts facts + summary to the task named by `ref`).

**Done when.** With the summariser and `notion-sync` plugins enabled, every finished agent run gets a summary within a minute and the Notion task named by its `ref` gets a comment, with no manual step.

#### J15 · The system extends itself, under review

**Job.** When an agent notices a chore I keep doing, I want it to write a plugin, hand it to me on a branch I review, and only then have it enabled, so that the system extends itself under my control.

**Baseline.** Neither.

**Work to be done.**
- Bundled `relay-plugin` skill: the contract, `relay.d.ts`, manifest schema, examples, `rl plugin dev` for a dry run against recorded events.
- Agent-authored plugins land in `.relay/plugins/<name>/` on a branch; review is `git diff`; `rl plugin enable` shows the manifest's capabilities and requires confirmation.
- Plugins are never auto-enabled; a plugin cannot enable another plugin; `schedule.register` from a plugin needs the `schedule` capability and creates a *pending* schedule until approved.
- Budget and kill switch: per-plugin `exec` quota and runtime cap; `rl plugin disable --all` in one keystroke.

**Done when.** "Every morning, post yesterday's digest to Slack" typed as a prompt produces a reviewed branch containing a plugin; enabling it shows `net: [slack.com], events: [digest.ready], schedule: 08:00`; it runs the next morning.

---

## 5. Plugin system

**Model.** Electron/Raycast style: each enabled plugin runs in its own `rl-plugin-host` process (vendored Bun, single binary shipped with Relay). The process starts with a scrubbed environment, no `$HOME` credentials, cwd in a per-plugin scratch dir, and no access to the daemon socket. The only channel is stdio, length-prefixed JSON-RPC. All capabilities are messages; `rld` brokers every one against the manifest. Runtime-level fs/net restrictions are a bonus, not the boundary.

**Layout.**

```text
.relay/plugins/<name>/          (repo; versioned; agent-writable on a branch)
~/.relay/plugins/<name>/        (user; personal)  — same name: repo wins
  relay-plugin.json             manifest
  index.ts                   default export { commands, on, schedules, ui }
  README.md
```

**Manifest.**

```json
{
  "name": "pr-open",
  "version": "0.1.0",
  "description": "Open a PR for the current branch",
  "capabilities": {
    "tools":    ["gh", "git"],
    "fs":       ["workspace:rw"],
    "net":      ["api.github.com"],
    "events":   ["branch.done"],
    "schedule": false,
    "runs":     ["command"],
    "notify":   true,
    "kv":       true
  },
  "commands": [{ "id": "open", "title": "Open PR", "key": "p", "context": "group" }],
  "ui": { "detail": { "title": "PR", "context": "group" } }
}
```

**SDK surface (`relay.d.ts`, brokered).**

```ts
relay.exec(tool, args, opts)              // only tools declared; journaled under the plugin run
relay.fs.read/write/list(path)            // within declared roots
relay.fetch(url, init)                    // only declared hosts
relay.events.on(kind, handler)            // declared kinds; ack + cursor
relay.events.emit(kind, payload)          // namespaced: plugin.<name>.<kind>
relay.runs.start(spec) / list / get       // declared run kinds; runs inherit ref/tags
relay.schedule.register(cron, command)    // creates a pending schedule
relay.notify(title, body, level)
relay.kv.get/set(key)                     // per-plugin store
relay.group.current()                     // { key, ref?, branch?, worktree?, workspace }
relay.ui.detail.set(lines[])              // declarative text panel for the detail tab
```

**Lifecycle.** `rl plugin new <name>` scaffolds. `rl plugin enable <name>` shows capabilities, asks for confirmation, records the approved manifest hash; a changed manifest requires re-approval. Resident hosts (reactions, schedules, UI) start with the daemon; one-shot commands spawn on invoke and exit. Crashes are per plugin; `rld` restarts the host with backoff and journals it.

**UI contributions (v1).** Two kinds only: an **action key** on a run/group row (`commands[].key` + `context`), and a **detail tab** whose content is plain text lines set via `relay.ui.detail.set` — no widgets, no layout. Everything else surfaces through runs, events and notifications.

**Agent authoring.** The bundled `relay-plugin` skill and `relay.d.ts` are the whole spec. `rl plugin dev <name>` runs the plugin against recorded events and a fake broker that logs every capability call.

---

## 6. Architecture of the new tree

Greenfield. Crate names below are proposals; nothing maps one-to-one onto `legacy`.

```text
crates/
  rl            CLI + TUI (ratatui): since, fleet, detail, watch/join, --json everywhere
  rld           daemon: run supervisor, scheduler, event ledger, presence, detection,
                plugin broker, digest and facts queries
  relay-proto   the JSON-RPC protocol (requests, events, versioning) — the public API
  rl-session     session holder binary (PTY / pipes), attach protocol, ring buffer
  relay-ledger  SQLite schema, migrations, writers, queries
  relay-detect  CLI registry (TOML), PTY heuristics, hook + transcript ingestion
plugins/
  host/         rl-plugin-host (Bun): RPC shim, relay.d.ts, SDK
  ref/          reference plugins: notify-macos, notify-ntfy, summarise, pr-open, notion-sync
skills/
  relay-plugin/ bundled skill for agents writing plugins
detect/         *.toml per CLI
```

**Ledger schema sketch.**

```sql
runs(id, workspace, kind, title, cli, cwd, branch, worktree, ref, tags_json,
     state, source, confidence, started_at, ended_at, exit_code, cost_json,
     schedule_run, plugin, parent_run)
events(id, at, workspace, run, group_key, kind, summary, ref_json)
sessions(id, run, holder_socket, cols, rows, attached_at)
session_io(session, seq, direction, data, ts)
facts(group_key, run, computed_at, facts_json)          -- cached collector output
summaries(group_key, run, model, computed_at, text)     -- cached narrative
schedules(id, scope, cron, spec_json, tags_json, ref, branch_template,
          missed_policy, overlap, enabled, approved_at)
schedule_runs(id, schedule, fired_at, run)
plugins(name, scope, manifest_hash, approved_at, enabled, state)
presence(client, last_seen_at)
```

No `changes`, no `units`, no `groups` table. A group is `COALESCE(ref, branch, workspace)` at query time.

What is consulted in the ax checkout (`~/Projects/ax`), and only consulted — read for the idea, then write it again:

| Idea | Where in ax |
|---|---|
| PTY launch, pane I/O plumbing | `crates/ax-exec/src/{pty_process,pane_shell,launch,engine}.rs` |
| Daemon socket framing, request/response types | `crates/ax-ipc/src/lib.rs`, `crates/axd/src/server.rs` |
| Attention ranking (`Blocked > Failed > Working > Done > Idle`) | `crates/axd/src/fleet.rs`, `crates/ax/src/dashboard/cards.rs` |
| Dashboard input model, fleet + detail views | `crates/ax/src/dashboard/{input,state,view/fleet,view/detail}.rs` |
| Watch/join streaming | `crates/ax/src/commands.rs`, `crates/axd/src/handlers.rs` |
| Journal schema style, writers/queries split | `crates/ax-journal/src/{schema.sql,writer/,query/}` |
| Bundled-skill scaffold (write-if-absent on `up`) | `crates/axd/src/skills.rs`, `crates/axd/assets/skills/ax-plan/SKILL.md` |
| Claude settings seeding (basis for hook seeding) | `crates/axd/src/workspace/claude_bootstrap.rs` |
| Worktree create/remove | `crates/axd/src/workspace/worktree.rs` |
| Spec/plan writing convention | `docs/superpowers/{specs,plans}/` |

Everything is rewritten.

---

## 7. Phasing

**P-1 — freeze.** `instilled/ax` is archived as the legacy reference (no further commits); `instilled/relay` starts with this brief and `relay-proto` first.

**P0 — survive and see.** J1 holder, J2 `rl run`, J10 generic PTY detection + Claude hooks, J5 `events` + presence + `rl since`, J8 fleet as attention-ranked rows, J7 watch/join over the holder.

**P1 — remember and report.** J6 facts collector + transcript ingestion, J4 scheduler + `schedules.yaml`, J11 confidence everywhere, J9 branch/worktree/ref rules, J12 protocol stabilised + `--json` + `events --follow`.

**P2 — extend.** J13 plugin host + broker + SDK, J14 reactions with replay, J15 `relay-plugin` skill + review/enable flow, J3 as the notify reference plugin, summariser, pr-open and notion-sync reference plugins, action keys + detail tab.

Each phase ends with one real piece of work end to end — `rl run --agent claude -b … "…"` → commits → facts → digest — using a recorded-transcript player for CI and a real Claude session by hand.

---

## 8. Non-goals

- Not a terminal multiplexer, not an IDE, not a model provider, not a task tracker.
- No unit-of-work entity or state inside Relay; no `change`, no `task`, no `unit`.
- No image or container management; no port publishing; no stack orchestration.
- No layout persistence.
- No plugin marketplace in v1; plugins are files in repos.
- No custom widget rendering for plugins in v1.
- No multi-machine dashboard in v1 (one `rld` per box; reach it over ssh). Revisit after P1 if the isolation setup makes this the real shape of J1.
- No porting of `legacy` code.

---

## 9. Open questions — with proposed answers

Each item states the question, the proposed answer, and what would change our mind. Unstruck items are still open.

1. **When is the digest the first screen, and what counts as "seen"?**
   *Proposed:* no time threshold. The digest is first whenever there is at least one unseen attention item (needs-input, failed, finished-unreviewed); otherwise fleet. "Seen" is per item, not per session: an item is seen when it was rendered in a digest the user dismissed, or when the user `join`ed that run. A `join` from a phone marks only that run seen, nothing else. `last_seen_at` becomes a fallback for `rl since` with no argument.
   *Would change it:* if per-item seen state turns out to be noisy in practice, fall back to a 20-minute gap rule.

2. **Cost for CLIs with no transcript?**
   *Proposed:* show `n/a`, never estimate. Facts are facts (principle 4); duration and command count are shown instead. If a CLI later exposes usage, its `detect/*.toml` declares the source.

3. **P0 test harness?**
   *Proposed:* a recorded-transcript player, not the old stub agent (a container-era artefact). Fixtures are Claude `stream-json` recordings for hook/transcript ingestion, plus a tiny `fake-cli` script (prints prompts, sleeps, exits) for PTY-heuristic and holder tests. Both run in CI without a model.

4. **Where does the summariser authenticate?**
   *Proposed:* it is a plugin, so it declares `tools: [claude]` and runs the host's `claude -p` with whatever login the user has. Relay core never holds API keys or talks to a provider.

5. **Are repo-level schedules enabled on clone?**
   *Proposed:* no. Same flow as plugins: `rl up` shows the schedules the repo declares, you approve, Relay records the file's hash; a changed file requires re-approval. Nothing declared by a repo ever fires on its own.

6. **Holder binary name and entry points?**
   *Proposed:* `rl-session`, internal only, spawned by `rld`; users never call it. `rl run` is the single entry point for every kind of run; there is no `rl wrap` (see J2).

7. **`ref` schemes: registered or conventional?**
   *Proposed:* conventional. A plugin *may* declare `refs: ["notion:"]` in its manifest so the TUI can offer it as the detail tab for matching groups, but Relay never validates a scheme.
