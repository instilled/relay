# The report, step 4b — the results queue

Status: design, for review · 2026-10-29

## Why

Step 4 made `summary.rs` the one recap engine. It rebuilds what a run did after the fact: when
you open the report, it reads a tail of the session and the run's facts, and asks an assistant
to write it up. Reading the first real recaps showed that this is the long way round.

- **The moment is already known.** Every run's agent tells `rld` when a turn ends and when it
  waits on you (`rl hook <agent> <Event>`: Claude `Stop` / `Notification`, Codex `Stop` /
  `PermissionRequest`, Gemini `AfterAgent` / `Notification`). That is how runs show as needs-you
  or idle today.
- **So are the words.** The turn-end hook's payload carries the agent's final message (Claude's
  and Codex's `last_assistant_message`; Gemini's response). That message is the agent's own
  wrap-up, or the question it is stuck on. Today `rld` drops it, then step 4 rebuilds it later by
  re-reading the transcript and paying an assistant to retell it.
- **Generating on open costs a wait and a guard.** Step 4 plans recaps in a background task when
  the report opens, needs a named summariser, and asks before more than 3 calls
  (`Mode::ReportCost`). Every one of those exists because the recap is made late.
- **A question in a reply is invisible.** A turn that ends on "should I also prune the
  branches?" is `Stop: idle`, the same as a finished one. Only permission prompts show as
  needs-you, so a run that asked you something in words reads as done.

This step **records the result when it happens**. Each time a run comes to rest, *asking* or
*finished*, `rld` writes one entry to a results queue, with the agent's own words and what changed.
The report reads the queue. The push (step 5) and the briefer read the same queue later.

## The scenarios this sharpens

- **Back after lunch.** The board shows each run's latest entry: `finished · +180/−40 · 3
  commits`, and its first line. Nothing is generated, and nothing ever was.
- **Back the next morning.** Each run that came to rest while you were away shows its latest
  entry in full (trimmed), plus "and 4 earlier" when there were more. Runs that asked you
  something in words now sit in needs-you, with the question.
- **Back after the weekend.** The prose is told from the entries, run by run, in order. A deep
  read over them is one key away, never made unless asked.

## Goals

- **Recorded, not reconstructed.** An entry is written when a run comes to rest, from what the
  agent already sends. No assistant call is needed to have one.
- **Asks or finished.** Every entry is one of the two (see *Telling asks from finished*).
- **The agent's words, beside the facts.** The entry carries its final message, and the facts
  that changed since the run's previous entry, so a claim sits next to what really changed.
- **One queue for every reader.** The report's board, drill-in and prose read it now; the push
  and the briefer read it later.
- **Questions in words count as needs-you.** A run whose latest entry is *asks* is waiting on
  you, whether it asked through a permission prompt or in its reply.

## Non-goals and anti-goals

- **Anti-goal: altering the agent.** Nothing is added to the agent's prompt, settings or
  instructions to make it report: no `ASK:`/`DONE:` markers, no injected wording. The queue reads
  only what the agent already sends.
- **No push, no channel.** Still step 5. This step makes what the push will send.
- **No audio, no briefer.**
- **No backfill.** Runs before this step render as step 4 left them.
- **Anti-goal: a session's end as a result.** A process exiting (closed, killed, crashed) is not
  a turn coming to rest. It writes no entry; it only marks the run's latest entry as its last.
- **Anti-goal: a missed ask.** When the reply can't be read either way, it counts as *asks*. A
  finished run shown as waiting costs a glance; a waiting run shown as finished stays stuck.

## An entry

Written by `rld` when a run comes to rest. One table (`results`, a new migration), append-only:

- **run**: the Relay run it belongs to;
- **at**: when it came to rest;
- **kind**: `asks` or `finished`;
- **how**: what said so: `permission` (a permission or tool-approval hook), `reply` (the
  turn-end hook and its final message), or `screen` (no hook; see *Runs without hooks*);
- **says**: the agent's words: the final message, or for a permission prompt what it wants to
  run (the hook's detail, e.g. `approve: shell` or Gemini's `{message}`). Kept whole up to a cap
  (the builder picks it; a few KB), never rewritten;
- **changed**: the facts since the run's previous entry (or its start): commits (subjects),
  `+X/−Y`, files, and the branch and whether it is pushed. Read from git at write time, from
  the HEAD kept with the previous entry;
- **answered**: set when the next prompt for that run arrives (`UserPromptSubmit`,
  `BeforeAgent`) or the next entry is written. An *asks* entry that is answered no longer
  counts as waiting.

**Which hooks write an entry.** Only these: the turn-end events (Claude and Codex `Stop`,
Gemini `AfterAgent`) and the permission events (Claude `Notification` `permission_prompt`, Codex
`PermissionRequest`, Gemini `Notification` `ToolPermission`). Claude's `elicitation_dialog`, a
question shown as a dialog, is an *asks* too. Other events that leave a run idle write nothing:
Claude's `idle_prompt` (it comes a minute after `Stop` with no message), and a turn you
interrupt (Codex `Interrupt`), since you were there.

**One rest, one entry.** In `rld` the entry is written beside `recap::pick_up`, on the same
`pauses` path in the `RunHook` handler, after its guards (foreign runs, `carries_on` on an idle
run). Hooks win over the screen: see *Runs without hooks*.

**Answered.** An entry is answered by the run's next prompt (`UserPromptSubmit`, `BeforeAgent`),
or, for a permission *asks*, by the run going back to work (`PostToolUse`, `AfterTool`), or by
the run's next entry. Codex's hooks are async and can arrive out of order: entries are ordered by
arrival at `rld`, and a turn-end entry that arrives while the run is already working again (a
later prompt got there first) is written already answered.

## Telling asks from finished

- **Certain:** a permission or tool-approval hook is always *asks*.
- **From the reply:** on a turn-end hook, read the final message.
  - *Asks* if its last paragraph ends on a question, or asks leave or a choice ("should I…",
    "want me to…", "shall I…", "which…", "do you want…", "let me know if/whether…").
  - *Finished* otherwise.
  - Cannot tell (empty, cut off, only a code block) → *asks*.
  - **No final message in the payload:** read the transcript's last assistant message once
    (`summary::locate`), and classify that. Only when that fails too is it *asks*.
- The rules are a small, tested function over the text, with fixtures for each phrasing, so
  they can be tuned without touching the plumbing.

## Runs without hooks

Shell runs, an agent with no hooks in its profile, or a hook that never arrived: `rl-detect`
already sees the run go needs-you or idle from its screen (the PTY stream `rl io` reads). That
transition writes the entry, `how: screen`, with the screen's last lines as **says**. Needs-you
is *asks*, idle is *finished*.

A hooked run gets screen guesses too (`supervisor::apply` takes both). A screen rest writes an
entry only when no hook entry for that run follows within a short grace (the builder picks it;
tens of seconds). A hook that never arrived still leaves an entry; a hook that did leaves only
its own.

## What the report does with it

- **The board** (any depth): each run shows its latest entry: kind, the headline of **changed**,
  and the first line of **says**. The needs-you band is the runs whose latest entry is an
  unanswered *asks*.
- **Under ~2h away:** the board, as above.
- **~2h to ~2 days:** under each run that came to rest while you were away, its latest entry,
  trimmed (first paragraph, capped), and "and N earlier" when there were more. An *asks* entry
  leads with the question.
- **Over ~2 days:** the prose (`tui/prose.rs`, today's layout, needs you · live · happened, by
  co-worker), told from the entries since you left, each run in order. Nothing is generated.
- **The drill-in** (`enter`): every entry for the run since you left, in full, each with its
  **changed**.
- **A deep read is asked for, not made.** `/summary` and `/summary deep` keep `summary.rs`, now
  reading the run's entries (and the transcript only where there are none). The step-4 content
  answers apply to what it writes: quick to grasp; caveats and attribution doubts under *Open
  questions*; unverified claims flagged against **changed**; deep as 3–5 plain sentences; a
  waiting run as the decision and a recommendation.
- **Trimming** "says": markdown kept as text, first paragraph for a line, a cap for a block. A
  message that says nothing ("Done.") falls back to **changed**.

## Retired from step 4

- Planning recaps on report open: the background task, the count-first guard
  (`Mode::ReportCost`, `ASK_OVER`) and `report.summary: auto | ask`.
- The need for a named summariser to see what happened.
- The recap sweep's seeds for runs whose hooks bring the final message. Whether the sweep stays
  for runs without one, or goes entirely, the plan settles from what each agent actually sends.

What stays: `summary.rs` for `/summary`, `rl summary`, catch-up and flows; `run_facts` frozen at
the end; the depth thresholds (`report.brief_after`, `report.deep_after`), which now pick the
layout only.

## Constraints

- **No backward compat.** The report's read gains the entries: bump `PROTOCOL_VERSION` 9 → 10;
  kill the running `rld`/`rl` after merge.
- Migration `0031`, append-only.
- `rl hook` is unchanged: it already forwards the whole payload (`RunHookParams.payload`). The
  entry is written by `rld`.
- Reading git for **changed** happens in `rld`, off the hook's clock.
- CI gates verified locally: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`.
- Tests use synthetic hook payloads for each agent, never real session content.

## My calls (not discussed; check these)

- **The prose over 2 days is told from entries, with no deep read made on open.** This reverses
  step 4's "generate on open" for the weekend. A deep read is `/summary deep`, or `p` on a run.
- **Runs only.** Outside sessions (`outside.hook`, the agent's own sessions shown as theirs)
  get entries in a later step, once the report shows them.
- **"changed" is git since the previous entry**; tokens and active time stay with `run_facts`.
- **An answered *asks* stays in the queue**, marked answered; nothing is deleted.
- **"and N earlier"** rather than showing every entry on the board.
- **Only turn-end and permission hooks write entries**; `idle_prompt` and `Interrupt` don't;
  `elicitation_dialog` is *asks*.
- **A missing final message** is read from the transcript before it counts as *asks*.
- **Hooks win over the screen**, with a short grace before a screen entry is written.
- **Answered** by the next prompt, by going back to work after a permission, or by the next
  entry; a turn-end that arrives after the next prompt is written already answered.

## Open questions — to settle in the plan

- **Gemini's final message.** The `AfterAgent` field that carries the response (`prompt_response`
  is expected); confirm on a real payload.
- **Claude's field on older versions.** Whether `last_assistant_message` is always present on
  `Stop`; where it is not, read the transcript's last assistant message once.
- **The cap** on **says**, and the trim for a line and a block.
- **The sweep:** keep it for runs without a final message, or retire it.
- **Repeated *asks*:** a run that asks, gets no answer, and asks again (a second permission
  prompt) — a new entry each time, or the latest replaces the open one.
