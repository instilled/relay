# Relay — architecture

**Status:** as built · **Date:** 2026-09-09 · **Tree:** `0610d31`

[PRODUCT.md](PRODUCT.md) says what Relay is for and where it is going. This file says how the
tree that exists is wired: which processes there are, who owns what, and what crosses which
boundary. Every diagram describes code under `code/`; what is not built yet is listed at the
end and marked in the diagrams with dashes.

---

## 1. Processes

Three kinds of process and two socket protocols. Nothing else talks to anything.

```mermaid
flowchart TB
    subgraph cli["rl — CLI + control panel (rl-cli)"]
        tui["TUI: since-you-left · fleet · detail · terminal"]
        cmds["run · stop · ls · since · events · watch · join · mark"]
        hookcmd["rl hook &lt;Event&gt; — hidden, called by the agent's hooks"]
    end

    subgraph rld["rld — daemon (rl-daemon)"]
        server["server — bind rld.sock, one task per connection"]
        router["router — method name → typed handler"]
        handlers["handlers — every method in one place"]
        sup["supervisor — start · follow · apply · reconcile"]
        attach["attach — scrollback → replay → live"]
        digest["digest — one line per run, and whether it is news"]
        gitm["git — read-only placement: workspace, branch"]
    end

    subgraph holder["rl-session — one holder process per run"]
        h["holder: PTY (or pipes) · ring buffer · exit sidecar<br/>detached with fork + setsid"]
        child["the child: claude · codex · sh · any command"]
    end

    detect["relay-detect<br/>registry (detect/*.yaml) · PTY heuristics<br/>Claude hooks · state machine"]
    db[("~/.relay/ledger.db — relay-ledger<br/>runs · events · sessions · session_io · presence")]

    cli -->|"JSON-RPC, one line per message<br/>~/.relay/run/rld.sock — relay-proto"| rld
    child -->|"hooks fire<br/>RELAY_RUN in the env"| hookcmd

    sup -->|"spawn, wait for the readiness line"| h
    sup <-->|"framed holder protocol on the run's socket<br/>Output · Input · Resize · Signal · Exit"| h
    attach -.->|"one holder connection per viewer"| h
    h --> child

    sup <--> detect
    rld <-->|"one writer thread"| db

    classDef store fill:#1f2933,stroke:#7b8794,color:#e4e7eb
    class db store
```

What the picture is enforcing:

- **`rld` never owns a child.** Every run lives under its own `rl-session`; the daemon is only a
  client of that holder. A daemon crash, restart or upgrade therefore costs no sessions —
  `Supervisor::reconcile` finds the open sessions in the ledger and resumes the journal exactly
  where it stopped.
- **The holder depends on nothing else in Relay.** Its protocol is deliberately not JSON-RPC and
  not shared with `relay-proto`: a session must stay reachable with only that one binary alive.
- **`rl` speaks only `relay-proto`.** No client reads the ledger, and `--json` is the same data
  the TUI draws.
- **The ledger is one SQLite file behind one writer thread.** Every access ships a closure to
  that thread, so a call that returns has been applied.

---

## 2. Crates

```mermaid
flowchart LR
    rl["rl-cli → bin rl<br/>client · attach · digest · rank · render · tui"]
    rld["rl-daemon → bin rld<br/>server · router · handlers · supervisor · attach · digest · git"]
    sess["rl-session → bin rl-session<br/>holder · backend · ring · exit | proto · client"]
    proto["relay-proto (shared-proto)<br/>codec · rpc · methods · types — the public API"]
    core["relay-core (shared-core)<br/>Home paths · ids · time"]
    ledger["relay-ledger (shared-ledger)<br/>migrations · runs · events · sessions · session_io · presence"]
    det["relay-detect (shared-detect)<br/>registry · pty · claude · machine · ansi"]

    rl --> proto
    rl --> core
    rl -->|"holder frames after run.attach"| sess
    rld --> proto
    rld --> core
    rld --> ledger
    rld --> det
    rld -->|"client half only"| sess
    ledger --> proto
    ledger --> core
    det --> proto
    det --> core
    sess --> core
    proto --> core
```

`rl-session` splits in two on purpose: the library half (`proto`, `client`, `exit`) is what `rld`
links, the binary half (`holder`, `backend`) is the process itself.

---

## 3. Starting a run

```mermaid
sequenceDiagram
    autonumber
    participant U as rl run
    participant D as rld
    participant G as git
    participant R as relay-detect
    participant S as rl-session
    participant C as child
    participant L as ledger

    U->>D: run.start{argv, cwd, kind, ref, tags, tty}
    D->>G: rev-parse --show-toplevel · symbolic-ref HEAD
    G-->>D: workspace + branch (read-only, never written)
    D->>R: registry.resolve(kind, argv)
    R-->>D: cli, kind, hook args
    Note over D: claude → splice --settings ~/.relay/hooks/claude.json<br/>env: RELAY_RUN, RELAY_HOME, RELAY_REF, RELAY_TAGS
    D->>S: spawn (fork + setsid), wait for the readiness line
    S->>C: PTY, or plain pipes with --no-tty
    S-->>D: ready — socket is live
    D->>S: attach as follower
    S-->>D: Hello{pid, holder_pid, tty, cols, rows}
    D->>L: runs.insert · sessions.insert · events.append(run.started)
    D-->>U: Run{id, state: working, source: pty, confidence: guess}

    loop while the child lives
        C->>S: output
        S->>D: Output frames
        D->>L: session_io.append(out)
        D->>R: step(current, PTY signal)
        R-->>D: transition or nothing
        D->>L: runs.update · events.append(run.needs_input, …)
    end

    C->>S: exits
    S->>D: Exit{code, signal}
    S->>S: write the exit sidecar, linger
    D->>L: terminal state · events.append(run.finished / run.failed)
```

After a daemon restart, `reconcile` walks `sessions` where `closed_at IS NULL`: re-attach where
the holder still answers (resuming the journal, with a `note` line if bytes were lost while the
daemon was down), otherwise close the run from its exit sidecar — or as lost.

---

## 4. Watching and joining

```mermaid
sequenceDiagram
    autonumber
    participant V as rl watch / rl join
    participant D as rld
    participant L as ledger
    participant S as holder

    V->>D: run.attach{id, mode, client, scrollback, take}
    D->>L: runs.get · sessions.get · presence.touch
    D-->>V: AttachInfo{cols, rows, tty, live, joiner, journaled, replay_from}
    Note over V,D: after the ok reply the connection stops speaking JSON-RPC —<br/>it carries rl-session frames both ways

    D->>L: read the journal tail
    D-->>V: Output (scrollback)
    D->>S: attach with replay, from where the journal stops
    S-->>D: Output … ReplayEnd
    D-->>V: Output … ReplayEnd, then live output

    V->>D: Input · Resize — the one joiner only, a watcher's are dropped
    D->>S: Input · Resize
    D->>L: session_io.append(in)
```

Several watchers, one joiner; a second `join` is refused unless `take` turns the previous joiner
into a watcher. Each viewer gets its own holder connection, and the supervisor's follow
connection journals regardless — so a viewer leaving costs nothing and the run never notices.

---

## 5. How a run's state is decided

Every source produces a `Signal{state, source, confidence}`; `relay_detect::step` is the only
place a `(current, signal)` pair becomes a transition, and it decides against the ledger row.

| Source | Confidence | Wired today | Where from |
|---|---|---|---|
| `hook` | exact | yes | Claude Code hooks → `rl hook <Event>` → `run.hook` |
| `manual` | exact | yes | `rl mark done \| blocked \| working` |
| `pty` | guess / likely | yes | output-idle timers + prompt/question patterns over the byte stream |
| `transcript` | exact | not yet | `~/.claude` transcripts |
| `git` | exact | placement only | commits landed, branch done |

The rules, in order: terminal states never change here; `stale` only ever replaces
`working`/`starting`; a PTY guess never overrides an exact state from another source; the same
state again upgrades source/confidence at most; anything else is a transition, journaled as an
event.

```mermaid
stateDiagram-v2
    direction LR
    [*] --> starting: run.start
    starting --> working
    working --> idle: quiet ≥ idle_after, prompt line
    working --> needs_input: question pattern, or a Notification hook
    working --> stale: quiet ≥ stale_after
    idle --> working: output
    needs_input --> working: answered
    stale --> working: output
    idle --> blocked: rl mark blocked
    working --> blocked: rl mark blocked
    blocked --> working: rl mark working
    working --> finished: exit 0
    working --> failed: exit ≠ 0
    working --> stopped: rl stop
    finished --> [*]
    failed --> [*]
    stopped --> [*]
```

`needs_input`, `blocked` and `failed` are the attention states: they sort to the top of the
fleet and are what `rl since` leads with. Confidence rides along in the UI, always visible.

Grouping is derived, never a table: `group_key = ref → branch → workspace`, materialised onto
each event row at write time so the digest groups without a join.

---

## 6. On disk

```text
~/.relay/                     ($RELAY_HOME overrides; tests point it elsewhere)
  ledger.db                   SQLite: runs, events, sessions, session_io, presence
  run/                        0700 — rld.sock, rld.pid, <run>.sock, <run>.exit
  log/                        rld.log, <run>.log
  hooks/claude.json           written at daemon start; passed to claude as --settings
  detect/*.yaml               user overrides, merged field by field over the built-ins
  plugins/                    (empty — see below)

detect/*.yaml                 in-tree, compiled in: generic, claude, codex, cursor, sh
scripts/{rl,rld,relay-dev}    dev shims: build if needed, run the debug binaries
```

Sockets normally live in `run/`; a long `$RELAY_HOME` would blow the ~104-byte `sockaddr_un`
limit, so both sides fall back to the same short hashed directory under the temp dir.

---

## 7. Not built yet

Named here so the diagrams above are not read as the whole plan — see §7 of the brief for
phasing.

- **Scheduler** — `schedules`, `schedule_runs`, cron → run spec. `Run.schedule_run` exists, unused.
- **Plugins** — `rl-plugin-host` (Bun), the broker, `relay.d.ts`, the manifest/approval flow,
  action keys and the detail tab. `Run.plugin` and `~/.relay/plugins/` exist, unused.
- **Facts and summaries** — the collector, transcript ingestion, cached narrative, cost. No
  `facts` or `summaries` tables yet; `Run.cost` is carried but never set.
- **Worktrees and ref rules** — `--worktree`, branch-naming, `.relay/config.yaml` ref derivation.
  `Run.worktree` and `Run.parent_run` are carried, never set.
- **Notifications** — the notify reference plugins.
