# The report, step 3 — the facts collector and the prose digest

Status: design, for review · 2026-10-21

## Why

Step 2 gave the report a pane: a work-and-teams board where each lead is a row under its
co-worker, needs-first. But a row today says little past its title and last line —
`refactor-auth · done`. To decide "touch it or not" you still open it. And the board is a
thing you *read*: it can tell you nothing while you are away from the screen.

This step does two things that answer the same need — **coming back to the facts**:

1. **The facts collector (J6).** When a run ends, gather the deterministic facts about what it
   did — the commits and diff it produced, how long it actually worked, whether it errored,
   what it cost in tokens — and freeze them on the lead, so the *happened* row and its drill-in
   answer "touch it or not" without opening anything.
2. **The prose digest.** Render the whole report — needs-you, live, happened — as a short
   narrative you read in ten seconds: the standup, in sentences. `rl report --prose`, and the
   same prose in the pane's `/report`.

The prose comes first, deliberately. Writing the report as sentences is the forcing function
for the information architecture: if the sentence reads well, we collected and ranked the
right facts. **Audio is a later step built on this prose** — Relay emits the narrative, a
plugin voices it (macOS `say` or a real TTS), the same fire-a-command mechanism as the push
hook — and it waits until the prose reads right.

## The scenarios this sharpens

From the arc spec, unchanged but made real here:

- **Come back.** You open the report and the *happened* rows already carry their facts —
  `refactor-auth` finished, three commits in `auth/`, worked fourteen minutes, left a note on
  what's next. You decide without opening a thing.
- **Be told.** The prose digest is a thing that can be *spoken* — the next step pipes it to
  your ears. This step makes the sentence exist.

Nothing a source does changes. The collector reads Relay's own records; the prose renders the
model. Both are Relay's, downstream of the leads.

## Goals

- A run that ends carries its facts — **frozen**, keyed to the run, computed **once** from
  **local** sources.
- The facts are **kind-specific**: an agent, a shell, and a flow each surface a different
  handful.
- The *happened* **row** shows a one-line digest of those facts; the **drill-in** shows them
  in full.
- `rl report --prose` and the pane's `/report` render the whole report as a short, needs-first,
  co-worker-grouped **narrative**.
- Everything stays **offline and deterministic**, or **harvested** from records Relay already
  keeps — and **free at any scale** by default (see *What it costs*).

## Non-goals and anti-goals

- **No network call from Relay itself.** PR existence and review status are a **source**
  concern — a GitHub flow posts them as leads, already in the arc. The collector may
  *correlate* a run to such a lead by branch, but it never calls GitHub. (Confirmed: offline,
  correlation only, for now.)
- **No fresh reasoning in the collector, and no new per-run cost by default.** The collector
  never reasons. The background recap Relay already keeps is **free** — it reads the agent's
  *own* recap from the transcript (`away_summary` / last turn), no assistant call — so facts
  for any number of running agents cost nothing but disk reads. The *worked-on* line and the
  prose default use that **free recap blob**. The clean split into *worked-on · open
  questions · what's next* is computed **lazily — only when you open a run's drill-in** — as
  one small assistant call for that one run, then cached; so fifty running agents never trigger
  fifty calls. The structured-on-drill-in step is configurable (off = pure-free).
- **Audio is out of scope here** — it is the next step, and it is why the prose must read
  right first.
- **Flow failure-fix suggestions** (how to fix / re-schedule) are **deferred** — interpretive,
  a fast-follow.
- **No backfill.** Facts attach to runs that end *after* this ships; existing *happened* rows
  render gracefully without facts (and the ~24h happened window churns them out fast).
- **Anti-goal:** a row so dense it stops being skimmable. The row is one line; depth lives in
  the drill-in and the prose.

## The facts, by kind

The lens for every source is the run's **`[started_at, ended_at]`** window. Commits,
commands, log entries outside it are not this run's.

### An agent (a claude / codex run)

Drawn from git over the window, the **session log** (the Claude/Codex transcript) over the
window, and events:

- **worked on** — the agent's **own recap** read from the transcript (free). On the row and in
  the prose default, this blob *is* the "worked on" line.
- **what changed** — the commits the run produced, attributed by **HEAD delta**: `HEAD`
  captured at run start and end, the run's commits are `startHEAD..endHEAD`, with a `git diff
  --stat` of them — totals (`+X/−Y`) and the top files or dirs ("mostly `auth/`"). Cheap (two
  `rev-parse`s), immune to rebase/amend timestamp games. Concurrent runs in one shared worktree
  stay an accepted imprecision.
- **how long** — **active** time: the span of actual turn activity in the session log (idle
  gaps beyond ~2 min collapsed), not wall-clock, which would count the hours it sat waiting on
  you.
- **cost** — **tokens** from the transcript `usage` fields (free; no dollars — there is no
  reliable offline dollar figure for interactive/Codex runs). **Drill-in only**, never the row.
- **open questions / what's next** — the structured split of the recap, computed **lazily on
  drill-in** (one cached assistant call for the run you opened), not eagerly. Absent until you
  open the run, or when the step is configured off.
- **stuck?** — already the band: a run in `NeedsInput`/`Blocked` is *needs-you*, not
  *happened*. The fact just records it plainly.
- **a PR?** — *correlated, not fetched.* If a GitHub source has posted a lead for this run's
  branch, link it. Otherwise "no PR yet" is simply "no such lead." The collector records the
  branch and whether it is pushed (both local), nothing more.

### A shell

- **what it was about** — the command(s) / the shell's subject.
- **done?** — terminal state.
- **errors?** — **any** non-zero `shell.command` event in the window, not merely the holder's
  final exit (a shell can exit 0 with a failed command inside it). An event whose `exit` is
  absent is *unknown*, not a failure.

### A flow (a firing)

- **decisions to surface** — the open decisions it raised (already *needs-you*).
- **on failure, why** — the firing's exit and last line.
- *(deferred)* a suggested follow-up — re-schedule / how to fix.

## Collecting the facts

- **When — once, at run end.** The single hook is the supervisor's `finish()`, which already
  holds the `Run`, the `Exit`, and an open ledger transaction. The facts are a **snapshot
  frozen there**: commit more an hour later and the row does not silently change; the record
  is "what was true when it ended." A run that **resumes** re-collects (every run end fires
  `finish()`).
- **Collect-at-finish, with a sweep fallback.** git subprocesses and transcript reads are
  async/blocking I/O and cannot run inside the ledger `call` closure, and Claude flushes its
  own recap a minute or more after the turn — so `finish()` collects what is local-and-ready
  (the HEAD delta + diff, the `shell.command` events, token usage if the file is flushed) and
  writes the record, and the **existing recap sweep** (`recap.rs`, every 5s, 10-min window)
  refreshes the record's recap blob when it lands. Both are **free** of LLM calls.
- **From three cheap, local, windowed sources:**
  - **git** — capture `HEAD` at start and end; extend the read-only `git.rs` wrapper with
    `log startHEAD..endHEAD` + `diff --stat`; no-op cleanly when the cwd is not a repo.
  - **the session log** — the transcript, already located (`summary.rs::locate`); a one-shot
    windowed read at collection (not the keyword-filtered incremental read the recap uses)
    harvests token usage, the active span, and command/failure counts.
  - **events** — the `shell.command` events already emitted per command.

## What it costs

- **The background is free.** The recap sweep makes **no** assistant call — it reads the
  agent's own recap off the transcript. 50 running agents swept every 5s is disk reads, not
  tokens. Token counts are likewise read from the transcript, not computed.
- **The only paid step is the structured soft-field split, and it is lazy.** One small
  assistant call per run, fired **only when you open that run's drill-in**, then cached. You
  pay for the runs you actually inspect, not for the fleet. Off switch available.
- The paid summariser that already exists (`summary.rs`: `/summary`, catch-up `auto`, a flow on
  `run.finished`) is unchanged by this step.

## Storing the facts

Built-ins stay computed from `decisions` / `runs` at read time, as today; **the facts are the
one materialised thing**, because they are expensive to recompute and must be frozen. Shape
(to be settled in the plan):

- Emit a **`commit.landed`** event per commit in the HEAD delta — the event kind exists,
  defined and unemitted — so commits are first-class and queryable.
- Write a compact per-run **facts record** — a `run_facts` row (migration `0029`) or a
  `facts_json` column: diff totals and top paths, active seconds, token counts, command and
  error counts, branch and pushed-state, and the free recap blob. The cached structured split
  (when a drill-in produced it) rides alongside. The board and the prose read from it.

## Rendering

Two tiers on the board (step 2), plus the prose — all reading the frozen record.

- **The row** — a one-line digest from the **free** facts only: the state glyph, the title, and
  the single headline fact for its kind. Agent: `+180/−40 · 14m · 3 commits`. Shell: `ok` or
  `1 error`. Flow: `fired · 1 decision`. Terse — "touch it or not" at a glance.
- **The drill-in** (focus a lead — the focus capability the arc gives the pane) — the full
  facts: the commits and the files, active time, tokens, the agent's recap. Opening it is what
  triggers the lazy structured split (*worked-on · open questions · next*), cached thereafter.
- **The prose** — `rl report --prose` and the pane's `/report` prose view: the whole report as
  a short narrative, needs-first, grouped by co-worker, one or two sentences per lead, from the
  **free** facts + recap blob (the structured split only for runs already drilled into). A
  concrete sample:

  > **While you were out (2h):**
  >
  > **Needs you.** Your *mobile developer* has a review requested on #412. And `prod-logs` is
  > blocked, waiting on your yes to file the 5xx task.
  >
  > **Live.** Your *researcher* is gathering sources — 3 of 8.
  >
  > **Happened.** `refactor-auth` finished: 3 commits, `+180/−40` across `auth/`, worked 14
  > minutes, and left a note to wire up the token refresh next. Two shells ran clean.

  The prose is generated from the leads and their facts — deterministic phrasing, no network.
  It is the same text a later audio plugin pipes to `say`. Both prose surfaces live in `rl-cli`,
  so prose is a **client-side** renderer over `Report` + facts; the facts ride the wire.

## Surfaces

One model, the renderings grow by two:

- **`rl report --prose`** — a new mode; the existing flat `rl report` text stays as it is.
- **pane `/report`** — gains a prose view alongside the board.
- **the board rows + drill-in** — enriched with the facts.
- *(next step)* **audio** — a TTS plugin over `rl report --prose`.

## Constraints

- **No backward compat.** Facts ride the `Report`/`Lead` over the wire, so bump
  `PROTOCOL_VERSION` 7 → 8 and kill the running `rld`/`rl` after merge so they relaunch.
- Migration `0029` is append-only.
- CI gates verified locally before pushing: `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Relay makes no network call; delivery (and later, voice) is a plugin's job.

## Open questions — to settle in the plan

- **Facts storage** — `run_facts` table vs `facts_json` column; exactly which facts are events
  (commits) and which live in the record; where the cached structured split sits.
- **Prose templates** — the phrasing per band and per kind, and how terse the row's one-line
  digest is for each kind.
- **Pane toggle** — how `/report` switches between the board and the prose (a key, a sub-mode).
- **Active-span reader** — claude and codex transcripts differ; whether the ~2-min-gap active
  span comes out of one reader or needs a per-agent one.
