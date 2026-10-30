# The report, step 4b — the review queue and the inbox

Status: design, for review · 2026-10-30

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
- **Three layouts by time away read badly.** The board, the blocks and the prose each answer
  the same question differently, and none gives an overview by team and a way into one item at
  the same time.

This step adds a **review queue**: one item per session that has come to rest since you last
reviewed it, *asking* or *finished*. An item is a **pointer**, not content: which session, what
kind of rest, when, and from where in its log you have not looked. Readers go to the session for
the rest.

And it makes the report **one layout, an inbox**: a list of teams on the left, a reading pane on
the right. Time away only picks where it opens. The push (step 5) and the briefer read the same
queue later.

Mockups: the "Relay report mockups" canvas, artboards 5 (a team selected) and 6 (all teams
selected) and 4 (one item opened). Artboards 1–3 are the retired per-time-away layouts.

## The scenarios this sharpens

- **Back after lunch.** The report opens on the first item that needs you. Its question is in
  the reading pane; `r` answers it, `x` moves on.
- **Back the next morning.** The report opens on *All teams*: one block per team with what moved
  and what waits. The NEEDS YOU strip lists every ask across teams; `j`/`k` through it clears
  them without opening a team.
- **Back after the weekend.** The same, and `w` gives the written summary: prose walking the
  queue, nothing generated. A deep read of any item or team is `p`, made when asked, never on
  open.

## Goals

- **Recorded when it happens.** `rld` queues a session the moment it comes to rest. No
  assistant call is needed to have an item.
- **One item per session.** A session that comes to rest again updates its item (kind, when);
  it never adds a second.
- **A pointer.** The item keeps what is needed to find and read the session again, and nothing
  of what it said.
- **Reviewing clears it.** Marking it reviewed (`x`), attaching to the session, or answering it
  clears the item and moves its *from* to now. When the session comes to rest again, it is
  queued again.
- **Asks or finished.** Every item is one of the two (see *Telling asks from finished*).
- **Questions in words count as needs-you.** A session whose item is *asks* is waiting on you,
  whether it asked through a permission prompt or in its reply.
- **One queue for every source.** Each item names its source; this step fills it from Relay's
  agent and shell runs, and later steps add the rest (see *Sources*).
- **One report layout.** The inbox, by team, at any time away.
- **Nothing urgent hides in a team.** Every *asks* item is visible without opening its team.

## Non-goals and anti-goals

- **Anti-goal: altering the agent.** Nothing is added to the agent's prompt, settings or
  instructions to make it report: no `ASK:`/`DONE:` markers, no injected wording. Only what the
  agent already sends and writes is read.
- **Anti-goal: content in the queue.** No message text, no summary, no facts are copied into an
  item. They are read from the session (and git, where there is one) when shown, so there is one truth. Hints are tags,
  not content.
- **Anti-goal: an assistant call to draw the report.** Team summaries, gists and the written
  summary are built from the queue and the logs (and git, where there is one) by templates. Only `p` calls an assistant.
- **A session's end is a hint, not a rest.** A process exiting adds `stopped` (or `failed`) to the
  session's item, queueing it as *finished* if it was not queued, so a run that died while you
  were away still reaches you. The run's existing end handling and `run_facts` stay as they are.
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
- **hints**: a few tags, never text, so a reader can sort and word an item without opening the
  session:
  - `question`: its reply asked something;
  - `permission`: it waits on an approval;
  - `stopped`: the session ended (closed or killed) since *from*;
  - `failed`: it ended with a non-zero exit or crashed;
  - `unsure`: *asks* only because the reply couldn't be read either way.

  The set is closed and grows by a spec change, not by the builder;
- **at**: its latest rest;
- **rests**: how many times it came to rest since *from*;
- **from**: where your last review left off: the time, the HEAD when the run works in a git
  repo (for what changed), and the log position where the session's reader has one (`transcripts.read_to`-style). The session's
  start when you have never reviewed it.

A reviewed item is gone from the queue: its row is cleared, keeping *from* for the next time
(the builder picks whether that is a flag or a separate row of positions).

## What is read from the pointer

When an item is shown, the reader goes to the session:

- **The last message**: the agent's last assistant message in its log, or for a permission
  *asks* what it wants to run (the run's `waiting_for`). For a shell run, the screen's last lines.
- **What changed**, only where there is a git repo: git since *from*'s HEAD: commits
  (subjects), `+X/−Y`, files, branch and pushed. Git is not a prerequisite: a run can work in
  Notion, a shell, a folder with no repo. Without one, what changed is simply left out (no
  "nothing in git" line), and the item stands on its kind, hints and last message.
- **The log since you looked**: the turns in the session's log after *from*, one line each (who,
  when, first line).
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

## The report: an inbox

One layout at any time away: a header, a list pane on the left, a reading pane on the right, and
a key line at the bottom.

**Teams** are today's lanes (`tui/report.rs`): a run's tag, else its project's folder name, else
"my work". Teams are arbitrary and can be many; nothing is hard-coded to a set.

**The header:** how long you were away, since when, how many items to review, how many runs are
working.

**Git figures are extras.** Commits, `+X/−Y`, branch and pushed show only for items whose run
works in a git repo, and a total counts only those. A team or item with no repo shows none of
them, with no placeholder. Everything else (kind, hints, last message, times) works without git.

### The list pane

From the top:

1. **All teams**, pinned. Its second line: needs you · failed · to review, and commits when any
   item has a repo, across teams.
2. **NEEDS YOU**, a strip with every *asks* item across teams, one line each:
   `team · session name` and its hint chip (`question`, `permission`, `unsure`). Oldest first.
   Hidden when empty.
3. **TEAMS**, one summary row per team that has a queued item or a working run:
   - its name;
   - its counts: needs you, failed, done, working, and commits where there is git (zeros left
     out);
   - a one-line **gist**, built by a template from its items: what waits first, then what
     failed or stopped, then the latest finished, by session name and the item's state ("docs
     summary stopped; auth migration done"). Where an item has a repo, its git state is added
     ("not pushed"). Never the agent's words.

   Sorted by urgency: teams with needs-you first, then failed, then to review, then by latest
   rest. Name breaks ties.

The list pane never shows every item of every team. A team's items show when you step into it.

**Inside a team** (`enter` on a team row): the list pane shows that team's items, grouped
needs you → failed → done → working, newest first within a group. Each item: a mark (`●` asks,
`✕` failed, `✓` done, `◐` working), the session name, the time of its rest (or how long it has
worked), and a second line: hint chips, then `+X/−Y · N commits` where there is git, and the
last message's first line. `esc` goes back to the teams.

### The reading pane

It shows whatever is selected:

- **All teams:** one block per team, in the list's order: its name, to review, working, its
  git totals where there are any, and a line on what moved and what waits (the gist, longer).
- **A team:** a *since you looked* block (a templated paragraph from the team's items, with
  git totals where there are any), then its items as rows under NEEDS YOU, DONE (failed first)
  and WORKING: mark, name, `+X/−Y` where there is git, the last message's first line, time.
- **An item** (from the NEEDS YOU strip or inside a team): the item view.
  - *Asks:* a box with the last message, the question picked out; for a permission, what it
    wants to run.
  - *Finished:* the last message, trimmed (first paragraph, capped). A message that says nothing
    ("Done.") shows what changed alone, or just itself when there is no git.
  - *Changed since you looked:* git since *from*; the box is left out when there is no repo.
  - *Deep read:* the kept one if there is one, else "not made, `p` reads the log".
  - *The log since you looked*, filling the rest.
  - A title line: team · name, the agent, the branch if any, "N turns since HH:MM" when it rested
    more than once.

### Keys

| Key | On a team / All teams | On an item |
|---|---|---|
| `j` / `k` | move | move |
| `enter` | step into the team | attach to the session (clears it) |
| `esc` | — | back to the teams |
| `r` | — | reply: send the session a prompt (clears it) |
| `x` | mark every item of the team reviewed | mark it reviewed, move to the next |
| `p` | read the team deeply | read it deeply |
| `w` | the written summary | the written summary |

Selecting something only shows it; it never clears an item.

### Where it opens

Time away picks only the starting selection:

- under `report.brief_after` (~2h) with something in NEEDS YOU: the first item in the strip;
- otherwise: *All teams*.

### The written summary

`w` swaps the reading pane for prose (`tui/prose.rs`): needs you first, then each team in the
list's order, walking its items with their counts, git where there is one, and the last
message's first line. Built from the queue and the logs; nothing is generated. `w` again goes back.

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
- The three layouts by time away: the board, the blocks, and the prose as a layout (it stays as
  `w`). `report.deep_after` goes; `report.brief_after` only picks where the inbox opens.
- Whether the recap sweep and its seeds stay (for the list row's live line) or go, the plan
  settles once the inbox reads the last message from the log.

What stays: `summary.rs` for deep reads, `/summary`, `rl summary`, catch-up and flows;
`run_facts` frozen at the end; the lanes in `tui/report.rs`, as the teams.

## Constraints

- **No backward compat.** The report's read carries queue items: bump `PROTOCOL_VERSION` 9 → 10;
  kill the running `rld`/`rl` after merge.
- Migration `0031`, append-only.
- Reading the log (and git, where there is one) happens in `rld`, off the hook's clock.
- CI gates verified locally: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`.
- Tests use synthetic hook payloads and logs for each agent, never real session content.
- The look follows the canvas: the existing TUI theme, amber for asks, blue for done, red for
  failed, green for working.

## My calls (not discussed; check these)

- **The written summary walks the queue, with no deep read made on open.** This reverses step
  4's "generate on open". A deep read is `/summary deep`, or `p`.
- **Reviewing** is `x`, attaching, or replying; selecting an item to read it is not. `x` on a
  team clears all its items.
- **Where it opens:** the first ask when you were away under ~2h, else *All teams*.
- **The NEEDS YOU strip** lists *asks* only; failed runs show in their team's counts and gist,
  not in the strip.
- **Teams are the existing lanes**, and a team row shows only when it has a queued item or a
  working run.
- **Gists and team blocks are templates**, by session name and item state (git state added
  where there is a repo), never the agent's words.
- **Only turn-end and permission hooks queue**; `idle_prompt` and `Interrupt` don't;
  `elicitation_dialog` is *asks*.
- **Hooks win over the screen**, with a short grace.
- **What changed is git since your last review, where there is a repo**, and left out where
  there isn't (no other change source in 4b); tokens and active time stay with `run_facts`.
- **Sources beyond runs come in 4c.**
- **The hint set:** `question`, `permission`, `stopped`, `failed`, `unsure`. A session's end
  queues it with `stopped`/`failed` if it wasn't queued.

## Open questions — to settle in the plan

- **Gemini's final message.** The `AfterAgent` field that carries the response (`prompt_response`
  is expected); confirm on a real payload.
- **Claude's `last_assistant_message`** on `Stop`: always present, or read the log.
- **The log position in *from*** for each agent's log, so a deep read starts where you left off.
- **The trim** for a line and a block.
- **The sweep:** keep it for the live line of a working item, or retire it.
- **Narrow terminals:** below what width the reading pane stacks under the list, or the list
  hides while an item is open.
