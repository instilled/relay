# The report, step 4 — recap intelligence

Status: design, for review · 2026-10-25

## Why

Step 3 froze the hard facts of a run and put them on the report. What a run *meant*, its recap,
it left to the agent: Claude's own `away_summary`, Codex's last turn, nothing for anything else.
Looking at the real data since then showed four problems:

- **The recap is often missing.** An agent writes one only sometimes (recaps can be switched
  off, a run can crash or be short, Codex keeps none, Gemini is not read at all). The two runs
  collected so far both have `recap: null`.
- **The numbers are wrong.** A 71-minute run reports **180,281,573** input tokens. Two causes:
  the same message's usage is repeated across entries and summed, and cache re-reads (every turn
  re-reads the whole context) are counted as input.
- **Coming back is one depth.** Two hours away and a weekend away get the same report. They need
  different amounts of reconstruction.
- **There are two recap engines.** `summary.rs` already tells "what happened, said for the person
  who was not looking": lazy, kept, since you last looked, brief or deep (deep reads the whole
  stretch in chunks, "so a weekend of a busy agent is still one answer"). Step 3 left it
  unchanged and built a second path beside it (the sweep folding the agent's recap into
  `run_facts`, and a `FactsStructure` call that splits it). Building this step from scratch
  would make a third.

This step makes **one recap engine** that writes the recap itself, from the session, for every
model, at a depth set by how long you were away. The report is what reads it.

Step 5 is the **push**: delivery of the needs-you and finished lines, with the channel decided
there. Folding the catch-up view and `rl since` into the report is a later step. Audio stays
after both.

## The scenarios this sharpens

- **Back after lunch (under ~2h).** You still hold the thread. The board shows each happened row's
  headline fact (`+180/−40 · 14m · 3 commits`) and nothing is generated.
- **Back the next morning (~2h to ~2 days).** Each happened run reads as *worked on · open
  questions · what's next*, from a generated brief.
- **Back after the weekend (over ~2 days).** The report opens in prose: every co-worker's
  runs, told from a deep read of the whole stretch, because you have lost the threads.

## Goals

- **One engine.** `summary.rs` is the only thing that writes a recap. The report's drill-in and
  prose read it; `/summary`, the catch-up's top and flows keep reading it too.
- **Generated from the session, for every model.** The recap is written by `assistants.summary`
  from a **bounded tail of the session plus the run's full facts**: commits, files changed, rows
  added and removed, errors, branch, active time. When the agent wrote its own recap in the
  stretch, it goes in as a **seed**. It is input, never the answer.
- **Depth by time away.** The length of the stretch (since you last looked) picks the depth:
  - **under 2h:** the headline, from facts only. **No call.**
  - **2h to 2 days:** *brief*, the bounded tail + facts (+ seed), answered as worked on · open
    questions · what's next.
  - **over 2 days:** *deep*, the whole stretch, read in parts and joined, + facts (+ seed).

  The thresholds are ~2h and ~2 days, configurable.
- **Never blank.** Generated recap, else the agent's own recap, else the raw tail (its last
  message or question), else facts only (`3 commits, +180/−40, 14m · no recap`).
- **Honest numbers.** Usage is counted once per message, and tokens are reported as **fresh in ·
  cached · out**, the same way for Claude, Codex and Gemini.
- **Gemini is read.** Its session file gives the same mechanical facts as the others (tokens,
  active time, the tail) and goes through the same generator.

## Non-goals and anti-goals

- **No push, no channel.** That is step 5.
- **The catch-up view and `rl since` stay as they are.** They get the better recaps because the
  engine they share improves, but folding them into the report is its own later step.
- **No audio.**
- **No new kind of cost.** The only paid thing is the assistant call `summary.rs` already makes.
  Nothing is generated until a rendering asks for it, and what is generated is kept. Nothing is
  generated per run as it runs, and nothing incremental.
- **No network call from Relay itself**, beyond the assistant call it already makes.
- **No backfill.** Old runs render with what they have.
- **Anti-goal: a surprise bill.** Many runs mean many calls (see *What it costs*). A rendering
  about to fan out says how many calls it will take first (`SummaryParams.dry` already counts
  them).
- **Anti-goal: a recap that replaces the facts.** The generator is grounded in the facts, and the
  facts are still shown as facts beside it, never only paraphrased.
- **Anti-goal: blank rows.** See *never blank*.

## The engine — `summary.rs`, changed

What stays: lazy (only ever asked for), kept (`summaries`, `summary_kept`, the `run.summarised`
event), the stretch as "since you last looked", brief and deep, deep read in parts past `CHUNK`
and joined, `dry`, `again`, the never-an-assistant's-own-run rule.

What changes:

- **Brief no longer takes the agent's recap as the answer.** Today a Claude run with an
  `away_summary` in the stretch is told by that recap, with no call. Now, with
  `assistants.summary` set, every run, whatever the model, is read by the assistant: the bounded
  tail (the same bounds brief uses today), the facts, and the seed when there is one.
- **Facts go in.** For an ended run, its `run_facts`: the commits (subjects), totals `+X/−Y`, the
  files and top paths, the error count, branch and pushed, active time, tokens. A run still live
  has no frozen facts; it is read from its tail alone.
- **A run's brief answers in three parts:** *worked on*, *open questions*, *what's next*. The parser
  step 3 wrote for that (`split_structured`) moves to read the brief, so the drill-in and the
  prose can show the parts. The labelled three-part text is the one stored shape. Consumers
  that want a single line (the list row via `summaries::lines`) take the *worked on* part
  without its label; `/summary`, `rl summary`, the `run.summarised` event and flow steps get the
  labelled text.
- **Summaries over many runs generate each run's brief first** (a flow, a project, everything:
  the catch-up's top, `rl since`, `/summary` on them, flows on `run.finished`), then join, as
  accepted in *What it costs*. Runs whose brief is kept are not read again. With
  `catch_up.summary: auto` coming back makes those calls without asking; `auto` is your opt-in
  to that, and `ask` shows the count.
- **Deep gets the facts and the seed** next to the whole stretch.
- **Without `assistants.summary`**, nothing is generated. A run is told down the ladder: the
  agent's own recap, else the raw tail, else facts only.

## The seed and the sweep

The recap sweep (`recap.rs`) keeps running, free, as today: it picks up the agent's own recap
when a turn ends or the run waits on you, so the list's row and the needs-you text have
something to show live, with no call.

What it keeps is now the **seed**: kept apart from a generated recap, and never given back as
one. A generated recap, once there, wins over the seed everywhere. Until then the seed is what
the row shows, as today.

Step 3's `merge_recap`, which folds the recap into `run_facts`, goes.

## The facts, made honest

- **Counted once per message.** Claude repeats a message's `usage` on every entry it writes for
  that message; Gemini's chat file is an append log that rewrites each message as it streams (in
  one real session: 33,929 records, 1,234 messages, one message written 52 times). The reader
  keeps the **last** usage per message id.
- **Fresh, cached, out.** `tokens_in` today adds `cache_read_input_tokens`. Reported instead:
  - **fresh in:** input + cache creation;
  - **cached:** cache reads;
  - **out:** output.

  Each vendor maps its own fields to these three.
- **The headline and the drill-in show fresh and out**, with cached in the drill-in.

## Gemini

- **Which run.** A run whose `cli` is Gemini's (the name as the detect profile gives it) gets
  `Agent::Gemini`.
- **Where.** `~/.gemini/projects.json` maps a working directory to a name; sessions are
  `~/.gemini/tmp/<name>/chats/session-<time>-<id>.jsonl`. The run's session is the newest
  touched since the run started (the same `SLACK` rule as Claude). Older installs used a
  hashed folder name; the builder settles whether to support it.
- **What.** A header record (`sessionId`, `startTime`, `lastUpdated`, …), then records with
  `id`, `timestamp`, `type` (`user`, `gemini`, `info`), `content`, `toolCalls`, `thoughts`, and on
  `gemini` records `tokens` {`input`, `output`, `cached`, `thoughts`, `tool`, `total`}.
- **Read for:** tokens (deduped by `id`), active time (from `timestamp`), the tail for the
  generator (`content` and `toolCalls`), and deep's whole-stretch read. Gemini writes no recap, so
  there is no seed.

## Depth, where it applies

- **Time away** in the pane is the one the catch-up already uses: the presence value read when
  `rl` starts (`away_since`), fixed for the session. `last_seen_at` is touched every minute while
  the pane is open, so it cannot be the measure mid-session. Coming back still opens the
  catch-up, as today; the report opens with `/report`.
- **`rl report --prose`** defaults its stretch to when you last looked (as `rl since` does), not
  24h; `--since` still overrides. The flat `rl report` keeps its 24h default.
- **The board rows** are always the headline. Free.
- **Opening the report generates** (decided: generate on open). After being away:
  - **under 2h:** the board, nothing generated;
  - **2h to 2 days:** the board, with each happened run in the stretch showing its brief (*worked
    on · open questions · what's next*) inline, written as the report opens;
  - **over 2 days:** the prose, from deep reads written as the report opens.

  Generating behind the fan-out guard: kept recaps are reused, and past a handful of new calls
  the report shows the count and waits for you, unless set to auto. Until they arrive, rows show
  the ladder (seed, tail, facts). `p` and `enter` still go to any depth at any time.
- **Per-run reads use each run's own stretch** (from `seen_at`, else the whole run), never the
  report's moving `since`, so what is kept matches the next time and a second look is free.
- **The drill-in** (`enter` on a run) shows the run's recap at the depth its own stretch calls
  for, at least brief. A stretch's length is now − its start (`seen_at`, else `started_at`). A
  run you have not seen in three days gets a deep read; a deep read of a short run is still one
  call.
- **The prose** keeps today's client-side layout (`tui/prose.rs`: needs you · live · happened,
  grouped by co-worker), with each happened run's generated text in place of step 3's
  `recap`/`worked_on` clause: its brief from 2h to 2 days, its deep read past 2 days. There is no
  extra join call over the whole report. The client fetches each run's recap with `Summary`; the
  `Report` does not carry it. The lead-in says how long ("While you were out (2 days):").

## What it costs

- **Under 2h: nothing.** Facts and the seed are disk reads.
- **One run opened: one call** for brief, a few for deep (in parts), once, then kept.
- **Many runs: up to one call per run, plus the join**, once, then kept. A weekend's prose over 30
  happened runs is 30 deep reads. Today the catch-up's brief top over 50 Claude runs costs about
  one call, because it joins their own recaps; after this step it costs up to 50, plus the join.
  **That is the price of always generating, and it was accepted.** Two guards:
  - kept recaps are reused, so a second look costs nothing until a run moves on;
  - a rendering about to make more than a handful of calls says the count first (`dry`) and
    makes them when you go on, unless it is set to auto.

  The catch-up's `catch_up.summary: auto | ask` is the model for that setting.

## The content — open, to find by reading real output

What a generated recap says, and how it is framed, is **not decided**. It is the next thing to
work out, and like the prose in step 3 it is best found by reading real output rather than
specified up front. What is fixed:

- the inputs (bounded tail or whole stretch, the facts, the seed);
- the shape of a brief (three labelled parts);
- the depth per time away;
- grounding: it does not contradict or replace the facts.

What is open:

- the voice (who it is told to: you, coming back);
- what each part leads with;
- how much of the facts it restates;
- how the deep read is organised over a long stretch;
- how a needs-you run reads differently from a finished one.

The prompts are written so they can be changed without touching the plumbing. The plan's first
task after the engine works produces recaps for a handful of real runs at each depth, for
reading together before the wording is settled.

## Retired from step 3

- `FactsStructure` (the method, its handler, the TUI call on `enter`) and the facts-side prompt.
  `split_structured` stays, moved to read briefs.
- `merge_recap`, and the soft fields on `RunFacts`: `recap`, `worked_on`, `questions`, `next`.
  `run_facts` keeps hard facts only.

## Constraints

- **No backward compat.** `RunFacts` changes shape and a method goes, so bump `PROTOCOL_VERSION`
  8 → 9, and kill the running `rld`/`rl` after merge so they relaunch.
- `run_facts` is JSON in one column; a new migration only if the plan needs one, append-only.
- CI gates verified locally before pushing: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`.
- Tests use synthetic fixtures for each vendor's shape (a repeated-usage Claude message, a
  rewritten Gemini message), never real session content.

## My calls (not discussed; check these)

- **Fresh / cached / out** as the token split, with fresh + out on the headline.
- **The fan-out guard:** say the call count first above a handful, `auto | ask` like catch-up.
- **A live run is read from its tail alone**, with no live git delta.
- **Brief keeps its current bounds** for the tail.
- **The prose stays client-side**, today's layout with per-run generated text, and no join call.
- **Time away in the pane is `away_since`**, fixed at launch. **`rl report --prose` defaults to
  since you last looked.**
- **The three-part brief is the one stored shape.** The row takes *worked on*.
- **Per-run reads key on the run's own stretch**, so they are kept. A stretch's length is now −
  its start.

## Open questions — to settle in the plan

- **The seed's storage** — a marker on the `summaries` row or its own place; how the row, `/summary`
  and the drill-in pick generated over seed.
- **The brief prompt** — wording so a run's brief comes back in three labelled parts, and how the
  facts are laid out in it.
- **The thresholds' config keys** and where they live (beside `catch_up`, or under `report`).
- **The fan-out threshold** — what "a handful" is.
- **Codex's cached tokens** — confirm how `token_count` reports them, and map to the three.
- **Gemini's `input` field** — whether it already includes `cached`; map so fresh is not
  double-counted.
- **Older Gemini installs** — the hashed folder names, in or out.
