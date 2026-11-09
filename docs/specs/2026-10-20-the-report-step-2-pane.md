# The report, step 2: the pane — a work-and-teams board

Status: design, for review · 2026-10-20

Refines step 2 of [the report arc](2026-10-17-the-report.md). Step 1 (the lead model,
`rl lead post`/`clear`, the built-in projection, `rl report`) is merged on `main`
(#183, #184). This step brings the report **into the TUI** as its home surface.

## Why this, now

`rl report` proved the model: three bands, leads from any source, self-clearing. But it is a
one-shot text dump you run from a terminal. The report's promise — *come back and see what
needs you, organised the way you think of your work* — only lands when it is a **place you
live in**, inside the panel, refreshing as the work moves.

And the way you think of your work is **work and teams**, not run-kinds. You do not think
"I have three agent runs and a shell"; you think "what's my **mobile developer** doing, and
what's waiting on me in **relay**." So the pane is a board whose lanes are the co-workers and
teams you've named, and whose columns carry the work. The agent/shell/flow distinction drops
to a small glyph — true, but secondary.

## The board

One full-width subpage. Lanes stacked top to bottom; each lane is a co-worker or a team, its
leads beneath it as rows. Always expanded — you scroll, you do not unfold.

**Before you tag anything.** A source you have not tagged sits under a default: Relay's own
leads under *my work*, a flow under its own name. It already reads as a board:

```
‹ esc   report — who's waiting on you                    ⚠ needs · • live · ✓ done

  my work                                    2 needs · 2 live · 1 done
    ⚠  resume — checking for updates           relay   claude         Oct 14 19:01
    •  need rework — post-merge cleanup…        relay   codex          10:10
    •  1009 % · resting at prompt               relay   shell          19:01
    ✓  split the one-list row library           relay   claude         Oct 14
  ────────────────────────────────────────────────────────────────────────────
  morning-brief                              1 needs · 6 done
    ⚠  notion — Failed to execute public API…   relay   morning-brief  Oct 13 08:41
    ✓  posted to your "Morning brief" space      relay   morning-brief  08:48
```

**After you tag sources into a team.** Tag claude + codex + shell as **relay**; the lanes
re-form around the tag. Nothing the sources do changes — the grouping is yours:

```
‹ esc   report — who's waiting on you                    ⚠ needs · • live · ✓ done

  relay                              team · 2 needs · 2 live · 1 done
    ⚠  resume — checking for updates           relay   claude         Oct 14 19:01
    •  need rework — post-merge cleanup…        relay   codex          10:10
    •  1009 % · resting at prompt               relay   shell          19:01
  ────────────────────────────────────────────────────────────────────────────
  briefings                          team · 1 needs · 6 done
    ⚠  notion — Failed to execute public API…   relay   morning-brief  Oct 13 08:41
```

### Anatomy

- **Lane = a tag.** The outer grouping is the co-worker/team tag on the lead's source (below).
  A lane header carries the tag name, a `team` marker when the tag groups more than one source,
  and a band tally (`2 needs · 2 live · 1 done`).
- **Row = a lead**, one line, as columns:
  - **state glyph** — `⚠` needs-you (amber), `•` live, `✓` happened. The band, which was a
    top-level section in `rl report`, becomes this per-row mark.
  - **lead** — `title` (+ `summary` when it fits); the flex column, truncated with `…` via the
    existing width-aware `fit()`.
  - **project** — the lead's `scope` folder, basename only (`relay`), elided first under pressure.
  - **co-worker** — the individual behind this lead (`claude`, `morning-brief`), with a small
    kind glyph folded in (a flow carries `⚙`); distinguishes members inside a team lane.
  - **when** — `fmt_clock` (today `HH:MM`, older `Mon dd HH:MM`), right-aligned.
- **Ordering.** Lanes holding a needs-you lead sort before lanes that do not. Within a lane,
  rows sort needs-you → live → happened, newest-first inside a band — so the thing waiting on
  you is always at the top of its lane, and the busiest-with-you lane is at the top of the board.

### needs-you stays prominent (a refinement to call out)

The arc spec pinned needs-you **across the whole report** — one global band at the very top. A
team-major board instead pins it **within each lane** and floats needs-bearing lanes up. That
keeps "what needs me" scannable without a separate global section, but it is a real change from
the arc spec's band-major mock. The breadcrumb also carries a global needs-you count
(`report — 3 need you`) so the come-back answer is one glance. If lane-local proves too weak in
dogfooding, a one-line global needs-you strip above the lanes is the fallback — noted, not built.

## The tag layer

This is the real new model work, and it settles two of the arc spec's open questions.

**Source identity — what you tag.** A tag keys on a stable source identity, resolved as:
- a **flow** → its name (`morning-brief`);
- an **`rl lead post --source`** lead → that source string;
- a **built-in** (Relay's own projection) → a per-project source for run/decision leads
  (so an untagged lane defaults to the project), collapsing to a single *my work* when no
  project applies.

Step 1 reserved `source = "relay"` for all built-ins; this step gives built-ins a resolved,
**stable** source identity instead (project-derived, or `my work`), because the tag store keys
on it. That is a step-1-model touch-up, not a wire break — built-ins are computed at read time.

**The tag store — a user-side overlay.** Tagging is persisted the same way reordering,
renaming, and pinning are: a user-side overlay, not something a source emits (the `Order`
overlay in `tui/rows/order.rs` is the model). The store maps `source-identity → tag` (a tag is
a free-form string — *mobile developer*, *relay*, *briefings* — born the moment you use it).
Set once, inherited by every future lead from that source; retag whenever. A tag that several
sources share **is** a team; a tag one source wears reads as a co-worker. There is no separate
"team" entity — "a team is just a tag," exactly as the arc spec says grouping is just a tag.

**Setting a tag:**
- `rl tag <source> <name>` / `rl tag --clear <source>` — the CLI surface, and the testable seam.
- In the pane, a `t` on a selected lane/row sets or changes its tag inline (a small prompt,
  the same `Mode`-driven input the TUI already uses for rename).

**Not to be confused with** the existing `rl … --tag` / `$RELAY_TAGS` mechanism, which filters
**runs** by a label at query time. That is unrelated to the co-worker/team tag; this store is new.

## Rendering in the TUI

The structural lift is smaller than the arc spec assumed: the TUI **already** renders
full-width subpages with no side column (`show_pane` → `draw_subpage_body`, the body taken
edge-to-edge). So this step adds a new subpage, it does not rebuild the pane framework.

- **A new `View::Report`** (beside `Work`, `Events`, `CatchUp`), entered by a command/key and
  shown full width like `draw_catch_up`. The breadcrumb header (`‹ esc  report — …`) is the
  existing `subpage_head`.
- **Data.** The TUI calls the existing `report.get` RPC (today only `rl report` does) on entry
  and on the same event cursor the panel already follows, applies the tag overlay, and renders.
- **Layout.** Columns budgeted with the row machinery that already exists — `fit()` for each
  cell, `wrap()`/`place_parts()`, ratatui `Layout::horizontal` as `areas()` uses. No borders;
  dim `─` lane rules, matching the TUI everywhere. Min width 80: the lead column flexes down and
  the right-hand columns (project, when) elide first, as the overview rows already degrade.
- **Act on a lead.** Selecting a row and acting runs the lead's `action` (an agent/command via
  the same path `rl lead` actions use) or opens the underlying run/decision in the panel. This
  is the genuinely new capability the arc spec flagged; it needs a focus target into `tui::run`.
  Scoped in: open the underlying run/decision, and run a lead's `action`. Scoped out of step 2:
  a general "focus the panel on any lead from the CLI" — left to the plan if it falls out cheaply.

## Scope

**In:** the tag store + `rl tag` + in-pane tag; built-in source-identity resolution
(project / *my work*); `View::Report` board rendered from `report.get`; lane/column layout,
ordering, needs-you prominence; act-on-a-lead (open run/decision, run action). One PR off `main`.

**Out (later steps / separate work):**
- The **facts collector (J6)** enriching *happened* — arc step 3.
- The **push hook** — arc step 4.
- The **god-module `draw`/`pane` split** — an independent TUI cleanup; the board is a new
  `*_lines` builder either way, so the split is not a prerequisite and is not bundled here.
- **Per-co-worker glyph/colour from a profile** (#152) — polish; a kind glyph is enough now.
- **Dropping the watch tile** (the pane-redesign direction) — folded in only if it is in the
  way; not a goal of this step.

## Dismissal, and a step-1 carry-forward

Step 1 left a known gap: a re-post overwrites a lead's `status`, and `assemble`/`list` do not
filter on status yet — so once the pane can **dismiss** a lead, dismissal needs a write-guard
(a re-emit must not un-dismiss) **and** a read-side status filter. The pane is where dismissal
becomes real (a `d` on a row), so this step owns closing that gap: dismissed leads drop from the
board, and a source re-emitting the same key does not resurrect a hand-dismissed lead.

## Review focus

- **No sources tagged yet** (first run): every lane is a default — must be coherent and useful,
  not empty or all-*my-work*-noise.
- **A lead whose `scope` is `None`**: the project column must render blank, not `/` or panic.
- **A co-worker working across two projects**: rows with differing project column values under
  one lane — must stay sorted and readable, not imply a single project.
- **80-column terminal**: the lead column must stay legible after project/when elide; no wrap
  into an unaligned mess.
- **A dismissed lead whose source re-emits the same key**: stays dismissed (the write-guard).
- **A tag set on a source with leads already in all three bands**: all of them move lanes at
  once (the tag is on the source, not the lead).

## Open questions — to settle in the plan

- **Entry key.** Which key/command opens `View::Report`, and whether it replaces or sits beside
  the existing catch-up-on-return. Leaning: a dedicated key, catch-up unchanged.
- **Tag store location.** A small `tags` table keyed on source identity, vs. riding the existing
  `Order`/overlay persistence. Leaning: its own table (clean key, survives unrelated overlay churn).
- **Built-in source identity.** The exact project-source string for a run/decision built-in
  (workspace path? basename? the `g:<folder>` key the panel already groups by — preferred, for
  consistency), and the precise *my work* fallback condition.
- **Refresh cadence.** Re-assemble on the panel's existing event tick, vs. a lighter report-only
  cursor. Leaning: reuse the panel tick; the report is cheap and read-time.
