# The project overview: flows off the list, managed from the project

## Why

The list is for what comes and goes: runs and agents' own sessions, which
start, finish and disappear, and which you put in your own order with
`⇧↑↓`. Flows are not like that. They stand there, so mixing them in among
runs makes the list busier without telling you anything new.

`enter` on a project line shows nothing useful today, just the pane listing
its flows. The moment you press it you want to know:
- which flows are running, and what each one last did;
- what is going on in the project overall.

You also want to manage flows from there: see them as a list, change one,
and make a new one. Nobody wants to write a flow by hand, so the assistant
does the writing and you steer it with words.

## Goals

1. **The list shows only runs and agents' own sessions.** Flows are no
   longer rows. `⇧↑↓` keeps working on what is left, as it does now.
2. **The project line sums up its flows,** and lights up when one needs you.
3. **`enter` on a project line opens the project overview:**
   - a prose summary of the project at the top;
   - its flows below that, as a list you can move through.
4. **You manage flows from that list:**
   - open one;
   - change one with words;
   - make a new one with words;
   - hand-edit one as a fallback;
   - remove one.

## Non-goals

- How a flow runs, how it is defined (`flow.md`), and how flows are scheduled
  all stay as they are.
- The Codex outside-row removal bug is a separate piece of work.

## Anti-goals

These would make it a failure even if it technically works:
- A flow that needs you gets buried. Losing its row must not lose the signal.
- Changing a flow means writing YAML or markdown by hand.
- The overview takes over the screen with a wait and no sign of life. #189
  fixed that for asking, and this must not bring it back.
- The list jumps around when flows fire. Removing flow rows should make it
  calmer, not noisier.

## What was decided

### The list

- **Flow rows go:** both a flow's folded row (`Row::Flow`) and the runs it
  folds. A run a flow started does not appear on the list on its own.
- **A run from a flow that needs you shows nowhere on the list.** That is
  the user's call: the project line lights up instead (below). Any question
  it asks is answered from the overview.
- **This supersedes PR #190** (flows at the bottom by name), which should be
  closed unmerged.
- `f` / `/flows` (show or hide flows' runs) loses its purpose. My call: remove
  it.
- An order saved before this that names flows (`f:` keys in `row_order`)
  just ignores them.
- My call: only the list loses flow rows. The catch-up keeps them (`catch_up:
  top` still heads it) and so does search (`kind:flow`, `by:<flow>`); keep
  `Row::Flow` for those and filter it out of the list only.
- My call: `everywhere` is treated like a project. Its flow rows go too, its
  line gets the same tally and light, and `enter` on it opens the same overview
  for the flows with no `in` (its summary is the one for runs outside every
  project, or none).

### The project line

- **Its flow chips become one compact tally,** for example
  `3 flows · 1 running · 1 needs you`. Parts that are zero are left out. With
  no flows there is nothing. The exact wording is my call.
- **When any flow needs you, the project line lights up.** This uses the
  amber the list already uses for "needs you". "Needs you" means a flow
  waiting for its yes (`NeedsApproval`), or a firing that has asked a
  question, is blocked or has failed.
- The header's existing `attention` count of off-list items needing you
  counts these flows too.
- My call: the light goes out the way a row's `unseen` mark does today — once
  you have opened that flow (`enter` on it in the overview), or it is answered,
  said yes to, or fires again and is fine. Opening the overview alone does not
  clear it.

### The overview (`enter` on a project line)

A full-width subpage (`show_pane`), as the other subpages are:

1. **The summary at the top:** the prose `/summary` already writes for a
   project (`SummaryTarget::Project`).
   - It is asked for when the overview opens, unless there is a recent one.
   - While it is coming, the overview says so, with elapsed time, the way
     the proposal pane (#189) shows its wait.
   - The flow list below is usable at once; it does not wait for the summary.
2. **The flows below the summary,** one line each:
   - name;
   - state: on, paused, running, or needs you;
   - when it next fires, or last fired;
   - what it last did: one line from its last firing (a step's summary, else
     how it ended).
   - Those that need you first, then by name.
   - My call: a flow's open question shows as its own line under it, as a
     question row does under a run on the list today. With that line selected,
     typing answers it (`1 and a note`), exactly as on the list; typing changes
     the flow only with the flow's own line selected. A flow waiting for its
     yes is said yes to where it is today, on its page (`enter`).
3. **A last line, `+ new flow`.**

### Keys in the overview

- `↑↓` move.
- `enter` on a flow opens it, as `enter` on a flow row does today (its steps
  and last firings).
- **Typing on the line, with a flow selected, changes that flow with
  words** (below). With `+ new flow` selected, it makes a new one.
- `^e` on a flow opens its `flow.md` in `$EDITOR`, as `/flow edit` does. This
  is the hand-editing fallback.
- `^x` parks a flow, armed and fired like any row's remove (see "Parking a
  flow" below).
- `esc` goes back to the list.
- My call: pausing and resuming stay `/flow pause` and `/flow resume`, which act
  on the selected flow; no new keys, since letters on the line are words.

### Changing and making flows with words

- **Words on a flow:** the assistant is given the flow's current `flow.md`
  and your words. It answers with a changed `flow.md`.
- **The proposal shows in the pane,** as #189's proposal does:
  - the wait shown while it works;
  - then what would change against what is there now.
- You can then:
  - type more words, which refine the proposal and keep it in the pane;
  - press `enter` to save it. It is written, and said yes to as `/flow edit`
    does on return;
  - press `^e` to open the proposed text in `$EDITOR` before saving;
  - press `esc` to drop it, which leaves the flow as it was.
- **Words on `+ new flow`:** this is `/flow new <words>` with the folder set
  to this project's. The proposal is shown and refined the same way, and the
  name is taken from the proposal. It builds on the existing draft path
  (`Want::Flow`, `Drafted`, `write_draft`) and moves it into the same pane.
- A proposed flow that would not load (`flow::check`) says why in the pane
  and cannot be saved until it is fixed, by more words or by `^e`.

### Parking a flow (the user's call)

Removing a flow takes it off the schedule and archives it, without deleting
anything:
- **Its folder is renamed** from `~/.relay/flows/<name>` to
  `~/.relay/flows/_<name>`.
- **Its runs are archived,** as `^x` on a flow row does today
  (`archive_flow`).
- **Then the flows are read again.**
- **`_` marks a parked flow.** The loader already skips any folder whose name
  is not a valid flow name, and `_` makes it invalid. So a parked flow never
  fires, and the reload marks it removed (`removed_at`), as a vanished folder
  is marked today.
- **This needs a daemon method** that renames, archives and reloads in one go,
  so the TUI never touches `~/.relay/flows` itself.
- My call: if `_<name>` is already taken by an earlier park, the new one gets
  `_<name>-2`, then `-3`, and so on. Nothing is overwritten.
- **Parked flows can come back.** My call on how:
  - The overview's flow list ends with a dim `parked: 2`, which opens them.
  - `enter` on a parked flow unparks it: the folder is renamed back and read
    again. Its runs stay archived. It needs a yes again before it fires, as
    any changed flow does.
  - If a flow by that name exists again, it refuses and says so.
- **Teach the rest of `rl` the convention.** `docs/ARCHITECTURE.md` and the
  palette's `/flow` text say that `_` folders are parked. My call: a
  `/flow park` and a `/flow unpark <name>` for the same thing from the line.

## Left to the builder

- Splitting the work into PRs. The suggested order:
  1. the list without flows, and the project line's tally and light;
  2. the overview with the flow list and its keys;
  3. changing and making flows with words in the pane;
  4. the summary at the top, if it does not fit in step 2.
- How "what it last did" is picked from a firing's data.
- How "recent enough" is decided for reusing a summary.
- How a proposed change to a flow is drawn: a diff or the whole new text.
- Updating `docs/ARCHITECTURE.md` and the palette's text as things move.
