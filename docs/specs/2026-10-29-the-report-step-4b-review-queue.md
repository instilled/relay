# The report, step 4b — the review queue

Status: design, for review · 2026-10-29

## Why

Step 4 made `summary.rs` the one recap engine. It rebuilds what a run did after the fact: when
you open the report, it reads a tail of the session and the run's facts, and asks an assistant
to write it up. Reading the first real recaps showed that this is the long way round.

- **The moment is already known.** Every run's agent tells `rld` when a turn ends and when it
  waits on you (`rl hook <agent> <Event>`: Claude `Stop` / `Notification`, Codex `Stop` /
  `PermissionRequest`, Gemini `AfterAgent` / `Notification`). That is how runs show as needs-you
  or idle today. Nobody keeps the fact that it happened, for you to look at later.
- **The words are in the session log.** The agent's own wrap-up, or the question it is stuck
  on, is its last message. It does not need retelling to be read; a deep read can go to the log
  when you want one.
- **Generating on open costs a wait and a guard.** Step 4 plans recaps in a background task when
  the report opens, needs a named summariser, and asks before more than 3 calls
  (`Mode::ReportCost`). Every one of those exists because the recap is made late, for every
  run, whether you look or not.
- **A question in a reply is invisible.** A turn that ends on "should I also prune the
  branches?" is `Stop: idle`, the same as a finished one. Only permission prompts show as
  needs-you, so a run that asked you something in words reads as done.

This step adds a **review queue**: one item per session that has come to rest since you last
reviewed it, *asking* or *finished*. An item is a **pointer**, not content: which session, what
kind of rest, when, and from where in its log you have not looked. Readers go to the session for
the rest. The report reads the queue now; the push (step 5) and the briefer read it later.

## The scenarios this sharpens

- **Back after lunch.** The board shows each queued session: `finished · +180/−40 · 3 commits`
  and its last message's first line, read from the log and git as the board draws.
- **Back the next morning.** Each queued session shows its last message (trimmed) and what
  changed since you last reviewed it, however many turns that took. Sessions that asked you
  something in words sit in needs-you, with the question.
- **Back after the weekend.** The prose walks the queue, session by session. A deep read of any
  item reads its log from where you last looked, made when asked, never on open.

## Goals

- **Recorded when it happens.** `rld` queues a session the moment it comes to rest. No
  assistant call is needed to have an item.
- **One item per session.** A session that comes to rest again updates its item (kind, when);
  it never adds a second.
- **A pointer.** The item keeps what is needed to find and read the session again, and nothing
  of what it said.
- **Reviewing clears it.** Opening the item, attaching to the session, or answering it clears
  the item and moves its *from* to now. When the session comes to rest again, it is queued
  again.
- **Asks or finished.** Every item is one of the two (see *Telling asks from finished*).
- **Questions in words count as needs-you.** A session whose item is *asks* is waiting on you,
  whether it asked through a permission prompt or in its reply.
- **One queue for every source.** Each item names its source; this step fills it from Relay's
  agent and shell runs, and later steps add the rest (see *Sources*).

## Non-goals and anti-goals

- **Anti-goal: altering the agent.** Nothing is added to the agent's prompt, settings or
  instructions to make it report: no `ASK:`/`DONE:` markers, no injected wording. Only what the
  agent already sends and writes is read.
- **Anti-goal: content in the queue.** No message text, no summary, no facts are copied into an
  item. They are read from the session and git when shown, so there is one truth.
- **Anti-goal: a session's end as a rest.** A process exiting (closed, killed, crashed) does not
  queue anything by itself; the run's existing end handling and `run_facts` stay as they are.
- **Anti-goal: a missed ask.** When the reply can't be read either way, it counts as *asks*. A
  finished session shown as waiting costs a glance; a waiting one shown as finished stays stuck.
- **No push, no channel.** Still step 5.
- **No audio, no briefer. No backfill.**

## An item

One table, `review_queue` (a new migration). One row per session:

- **source**: what kind of thing it points at. In this step: `run` (a Relay run, agent or shell);
- **ref**: its id (the run id);
- **kind**: `asks` or `finished`, as of its latest rest;
- **how**: what said so: `permission` (a permission or tool-approval hook), `reply` (the
  turn-end hook, read from the final message), or `screen` (no hook; see *Runs without hooks*);
- **at**: its latest rest;
- **rests**: how many times it came to rest since *from*;
- **from**: where your last review left off: the time, the HEAD (for what changed in git), and
  the log position where the session's reader has one (`transcripts.read_to`-style). The session's
  start when you have never reviewed it.

A reviewed item is gone from the queue: its row is cleared, keeping *from* for the next time
(the builder picks whether that is a flag or a separate row of positions).

## What is read from the pointer

When an item is shown, the reader goes to the session:

- **The last message**: the agent's last assistant message in its log, or for a permission
  *asks* what it wants to run (the run's `waiting_for`). For a shell run, the screen's last lines.
- **What changed**: git since *from*'s HEAD: commits (subjects), `+X/−Y`, files, branch and
  pushed.
- **A deep read**: `summary.rs` over the log from *from* to now, when asked (`/summary deep`,
  `p` on an item). It is kept as today, so asking twice costs once.

The step-4 content answers apply to what a deep read writes: quick to grasp; caveats and
attribution doubts under *Open questions*; unverified claims flagged against what changed; deep
as 3–5 plain sentences; a waiting session as the decision and a recommendation.

## Which hooks queue a session

- **Turn ends:** Claude and Codex `Stop`, Gemini `AfterAgent`. Kind from the final message (the
  payload's `last_assistant_message` / response where it has one, else the log's last assistant
  message).
- **Permission prompts:** Claude `Notification` `permission_prompt`, Codex `PermissionRequest`,
  Gemini `Notification` `ToolPermission`: always *asks*. Claude's `elicitation_dialog`, a question
  shown as a dialog, is *asks* too.
- **Nothing else.** Claude's `idle_prompt` (a minute after `Stop`, no message) and a turn you
  interrupt (Codex `Interrupt`) queue nothing.

In `rld` this sits beside `recap::pick_up`, on the `pauses` path in the `RunHook` handler, after
its guards (foreign runs, `carries_on` on an idle run). `rl hook` is unchanged: it already
forwards the whole payload.

Codex's hooks are async and can arrive out of order: a turn end that arrives while the run is
already working again (a later prompt got there first) queues nothing.

## Telling asks from finished

- **Certain:** a permission or tool-approval hook is always *asks*.
- **From the reply:** read the final message.
  - *Asks* if its last paragraph ends on a question, or asks leave or a choice ("should I…",
    "want me to…", "shall I…", "which…", "do you want…", "let me know if/whether…").
  - *Finished* otherwise.
  - Cannot tell (empty, cut off, only a code block, no message in payload or log) → *asks*.
- The rules are a small, tested function over the text, with fixtures for each phrasing, so they
  can be tuned without touching the plumbing.

## Runs without hooks

Shell runs, an agent with no hooks in its profile, or a hook that never arrived: `rl-detect`
already sees the run go needs-you or idle from its screen. That transition queues the session,
`how: screen`: needs-you is *asks*, idle is *finished*.

A hooked run gets screen guesses too (`supervisor::apply` takes both). Hooks win: a screen rest
queues only when no hook for that run follows within a short grace (the builder picks it; tens of
seconds).

## What the report does with it

- **The board:** the needs-you band is the queued sessions whose kind is *asks*. Every queued
  session shows its kind, what changed, and its last message's first line.
- **Under ~2h away:** the board.
- **~2h to ~2 days:** under each queued session, its last message trimmed (first paragraph,
  capped) and what changed, with "N turns" when it rested more than once. An *asks* leads with
  the question. A message that says nothing ("Done.") shows what changed alone.
- **Over ~2 days:** the prose (`tui/prose.rs`, today's layout, by co-worker), walking the queue.
  Nothing is generated.
- **The drill-in** (`enter`): the session's log from *from*, and its deep read when asked.
- **Reviewing:** `enter` on an item, attaching to its run, or sending it a prompt clears it.
  Seeing it on the board does not.

## Sources

The table is general (`source`, `ref`) so every "this needs you / this is done" lands in one
place. This step fills `run`. Later:

| Source | Queued when | Step |
|---|---|---|
| Relay runs, agent | asks / finished, from hooks | 4b |
| Relay runs, shell | needs-you / idle, from the screen | 4b |
| Flows | a flow finishes or fails, or waits on a step | 4c |
| Decisions (`rl ask`) | a run's question waits for your pick | 4c |
| Outside sessions | asks / finished, from `outside.hook` | 4c |

## Retired from step 4

- Planning recaps on report open: the background task, the count-first guard
  (`Mode::ReportCost`, `ASK_OVER`) and `report.summary: auto | ask`.
- The need for a named summariser to see what happened.
- Whether the recap sweep and its seeds stay (for the list row's live line) or go, the plan
  settles once the board reads the last message from the log.

What stays: `summary.rs` for deep reads, `/summary`, `rl summary`, catch-up and flows;
`run_facts` frozen at the end; the depth thresholds (`report.brief_after`, `report.deep_after`),
which now pick the layout only.

## Constraints

- **No backward compat.** The report's read carries queue items: bump `PROTOCOL_VERSION` 9 → 10;
  kill the running `rld`/`rl` after merge.
- Migration `0031`, append-only.
- Reading git and the log happens in `rld`, off the hook's clock.
- CI gates verified locally: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`.
- Tests use synthetic hook payloads and logs for each agent, never real session content.

## My calls (not discussed; check these)

- **The prose over 2 days walks the queue, with no deep read made on open.** This reverses step
  4's "generate on open". A deep read is `/summary deep`, or `p` on an item.
- **Reviewing** is `enter`, attaching, or sending a prompt; seeing it on the board is not.
- **Only turn-end and permission hooks queue**; `idle_prompt` and `Interrupt` don't;
  `elicitation_dialog` is *asks*.
- **Hooks win over the screen**, with a short grace.
- **What changed is git since your last review**; tokens and active time stay with `run_facts`.
- **Sources beyond runs come in 4c.**

## Open questions — to settle in the plan

- **Gemini's final message.** The `AfterAgent` field that carries the response (`prompt_response`
  is expected); confirm on a real payload.
- **Claude's `last_assistant_message`** on `Stop`: always present, or read the log.
- **The log position in *from*** for each agent's log, so a deep read starts where you left off.
- **The trim** for a line and a block.
- **The sweep:** keep it for the list row's live line, or retire it.
