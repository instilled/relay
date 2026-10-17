# The report: what happened, what's live, and what needs you — from any source

Status: design, for review · 2026-10-17

## Why

Relay's promise is that you start work, leave, and come back to the facts — with the
actions that matter left to your yes. The part that is missing is the coming back. Today
you learn what needs you by watching the panel, or by running `rl since` and reading it
yourself; the things that need you are scattered down the list among everything that does
not; and the signals that matter most on a real workday — an email waiting, a review
requested, feedback to triage, a cloud alert — do not live in Relay at all. You check five
places to know whether anything is on fire.

This is one place. A **report** of what happened, what is happening, and what needs you —
fed not only by Relay's own runs and flows but by **any source you point at it**, and
organised the way you think of your work: by the **co-worker** doing it.

Three scenarios are the measure of this design:

- **Come back.** You were out three hours. You open the report and see, at the top, the one
  decision waiting on your yes; below it, two runs still working and a flow mid-firing; below
  that, what finished while you were gone, with its facts. You say yes and move on. You did
  not scan anything.
- **Be told.** A scheduled flow found a prod error, drafted a Notion task, and is now blocked
  on your approval. You are not at your desk. A notification reaches you — *"prod-logs needs a
  yes: create task 5xx spike on checkout?"* — and you at least know to come back.
- **Your team.** A flow checks GitHub and a lead appears: *"review requested on #412."* You
  tag its source **mobile developer**. From then on, everything that flow surfaces stands
  under *mobile developer* in the report, beside your *researcher*'s findings and your
  *assistant*'s drafts — a standup you can read in ten seconds.

Nothing a source does changes because of the second or third scenario. A source only ever
emits. Everything else — the banding, the grouping, the delivery — is Relay's.

## A lead

The one object is a **lead**: a single thing worth your attention, from one source.

```
lead
  source     who raised it — a flow name, an `rl emit` source, or Relay itself
  key        the source's own id for this lead, stable across re-emits (e.g. pr:412)
  band       needs-you | live | happened
  title      one line
  summary    a little more, optional
  scope      a project/folder, optional
  action     how to act on it, optional — a command to run or a thing to open
  status     open | resolved | dismissed
  at         created / last updated / expires?
```

**Bands** are the only thing that ranks a lead:

- **needs-you** — it cannot proceed without you: an open decision, a failed or blocked run, a
  review requested, feedback waiting. Always pinned to the top.
- **live** — it is happening now and may change: a run working, a flow firing, an agent
  reporting progress.
- **happened** — it is done and here for the record: a finished run with its facts, a flow's
  firing and outcome, a decision since taken.

**Lifecycle is source-owned and keyed.** A source re-emits a lead under the same `key` to
refresh it — so a check that runs every fifteen minutes keeps **one** fresh lead, never a
pile. The source clears it when it is resolved; a lead may carry a TTL and expire on its own;
and you may always dismiss one by hand. Relay's own leads obey the same rule: a decision's
lead clears the moment the decision is taken.

## Sources

A lead comes from a **source**, and sources come in two kinds. Neither kind knows anything
about bands beyond the one it emits, or about grouping at all.

**Built-in — Relay itself, no configuration.** Relay projects its own state into leads, read
from the ledger it already keeps:

- an open decision (`DecisionStatus::Open`, raised as a `decision.raised` event) → a
  **needs-you** lead, its action `rl decide <id>`;
- a failed or blocked run (`RunState` in the needs-you set — `NeedsInput | Blocked | Failed`)
  → **needs-you**;
- a run starting/working/idle, a flow firing → **live**;
- a finished run, a flow's ended firing → **happened**, carrying its facts once the facts
  collector (below) exists.

These are computed from the `decisions`, `runs` and `events` tables at read time, so they
track the truth and self-clear; Relay stores no lead of its own.

**User-defined — a flow emits one.** A flow (`when` on a schedule, or `on` an event) runs
your command, and the command posts a lead. This is the existing `rl emit` extended from a
bare signal into a structured lead:

```
rl lead post --source feedback --key notion:triage \
  --band needs-you --title "5 feedback items to triage" \
  --scope flint-platform --action "claude -p 'triage the feedback queue'"
rl lead clear --source feedback --key notion:triage      # when the queue is empty
```

Your example flows already run these checks (`examples/flows/*` call gchat, Notion, GitHub);
today they can only start runs, not surface a standing lead. With one verb, each becomes a
source:

| flow checks… | posts a lead like |
|---|---|
| GitHub reviews requested | needs-you · "review requested on #412" |
| inbox | needs-you · "3 emails to reply" |
| Notion feedback log | needs-you · "5 items to triage" |
| cloud logs | needs-you · "2 new 5xx in the last hour" |
| a long agent run | live · "refactor — 60% through" |

The source stays dumb: it emits under a key and clears when done. It never learns which
co-worker it belongs to.

## The co-worker layer

How the report is **organised** is yours alone, and lives entirely in Relay — the same kind
of persisted, user-side overlay as reordering a project, renaming a row, pinning or hiding
(the `Order` overlay in `tui/rows/order.rs` and the `run.rename` / `run.pin` / hide methods).
Sources contribute nothing to it.

You **tag a source** with a co-worker role. Tags are free-form and come into being the moment
you use one — *mobile developer*, *product manager*, *researcher*, *assistant* — exactly as a
project name exists the moment you rename. Relay remembers the tag against the source, so you
set it **once** and every future lead from that source inherits it; retag whenever you like. A
source you never tag sits under a default — its project, or *my work* for Relay's own leads.

The report then **groups by co-worker**, needs-you still pinned across the whole report:

```
needs you
  mobile developer · review requested on #412
  assistant        · 5 feedback items to triage
live
  researcher       · gathering sources — 3 of 8
  my work          · refactor-auth (working), prod-logs (firing)
happened
  …
```

A co-worker may later earn a glyph and a colour the way an agent does from its profile
(#152); that is polish, not needed to stand this up. Grouping is just a tag.

## Surfaces

One model, rendered three ways.

**The report pane — the home.** The report is a place you enter, full width, not a column
beside the list. It is the catch-up grown up, and it is built on the pane redesign
(full-width subpages, the watch tile dropped, an ended run's output a static render of its
scrollback — see the pane-redesign direction). The list stays lean: it already carries
needs-you lightly (questions lead it, the headline counts), and the report is where the
whole picture lives. In the pane you can **act on a lead** — run its action, open its run or
decision — and **focus** it. Acting on a lead and focusing the panel on a specific thing are
genuinely new (nothing today takes a target into `tui::run`); they get their own design in
the plan.

**`rl report [scope] [since]` — the same model, printed.** From any terminal, the report as
text: running, stuck, needs-you, since whenever. It falls out of the model almost for free and
is the test harness for the model before any pixels exist.

**The push hook — being told.** When the **needs-you** band gains a lead (or one you have not
seen lingers), Relay fires a command from `~/.relay/plugins/` with the lead as its payload —
the same shell-plugin mechanism as workspace badges (`badges.rs`), the same mechanism your
example flows already use to reach Slack, mail and Notion. Relay makes no network call itself;
your script delivers. Dedup is keyed on the lead's `key`, so a flow asking every thirty
minutes pings you once; a per-source and global **quiet** setting silences what you do not
want. Answering from the channel is a non-goal here — a local daemon has no inbound endpoint —
so the push tells you to come back; the yes is still Relay's to take.

## How it's built

Five steps, one PR each, under this one spec. Each plugs into shapes that already exist.

0. **This spec.**
1. **The lead model, the `post`/`clear` verb, the built-in projection, and `rl report`.**
   Pure backend and CLI, fully testable with no UI. Built-ins compute from `decisions` /
   `runs` at read time; the `post` / `clear` verb (an extension of `rl emit`) writes
   user leads to their store; a background task spawned in `serve_with` (beside
   `decisions.serve` / `flows.serve`) maintains them and later drives the push, polling
   `events::after` the way the flow engine does (`flows/mod.rs` → `serve` / `drain`).
   Ships use-case *come back* as text and makes every external source possible at once.
2. **The report pane.** The pane-redesign structural change, with the report rendered in it —
   needs-you, live, happened, grouped by co-worker; acting on a lead; focusing one. Closes the
   `drawing`/`pane` cluster deliberately held out of the god-module split.
3. **The facts collector (J6).** The deterministic facts that enrich the *happened* band:
   git commits and `diff --stat` per run, commands with their exit codes, transcript
   ingestion (Claude/Codex `*.jsonl`), tokens and cost.
4. **The push hook.** Fire-a-plugin-command on needs-you changes; dedup on key; the quiet
   setting; one or two example notifier plugins.

The co-worker layer (tagging sources, grouping) rides on step 1's model and step 2's pane; it
needs no new source-side anything.

## What this replaces

This collapses three roadmap items into one arc. *Notifications* becomes step 4's push hook.
The *facts collector (J6)* becomes step 3, enriching a band rather than standing alone. The
*pane redesign* becomes step 2, with the report as the first real thing the new pane shows.
The existing **catch-up** is the seed of the report pane, and its **leads** (#155) are this
spec's leads made first-class. The roadmap's build order is resequenced to match once this
spec is approved.

## Open questions — to settle in the plan

- **Source identity.** Precisely what you tag: a flow by its name, an `rl emit --source`
  name, and for Relay's own leads a per-project source or a single *my work*. The tag store
  keys on this, so it must be stable.
- **Lead store.** Whether user-posted leads are a new `leads` table or ride the `events`
  table with a cursor and a resolution rule; and whether built-ins stay computed (preferred)
  or are materialised alongside.
- **Act on a lead · focus the pane.** New capability — a lead's `action` run from the pane or
  CLI, and the panel opened on a given lead/run/decision. Needs a CLI arg into `tui::run` and
  possibly an RPC method; nothing carries a focus target today.
- **Naming.** *lead* vs *entry* vs *signal*; *report* vs *catch-up* vs *inbox*. Leaning on the
  words already in the code (lead, catch-up).
