# A flow's own graph, the global graph dropped: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task by task.

**Goal:** a selected flow's pane draws its own wiring:
- what starts it
- its steps and how they connect, with `if` labels
- its last firing's mark on each step
- which flows it starts in turn

The global graph (`^g`, `/graph`, its layout, `graph.get`) goes.

**Roadmap line** (`docs/PRODUCT-ROADMAP.md`, Open): "a flow's own graph, the global graph dropped — a selected flow's steps drawn in its pane (`if` labels, its last firing's marks; what fires it and what it fires); `^g`, `/graph` and the global layout go".

**Architecture:** the pane already has everything it needs: `Flow.def` (a `FlowDef`), the shown firing (`FlowPane::shown()`), and every flow's definition (`self.flows`). So the flow graph is a pure function in the panel: definitions and a firing in, lines out. No RPC. It draws on a small char canvas, the one now in `tui/graph.rs`, moved to `tui/canvas.rs`. Then the global graph is deleted end to end: panel, protocol, daemon, ledger, docs.

**Tech:** Rust, ratatui. The crates are `rl-cli` (panel), `rl-flow` (pure flow logic), `rl-daemon`, `rl-proto` and `rl-ledger`.

## Global constraints
- Never touch the real `~/.relay` in tests or live checks; use `RELAY_HOME=/tmp/<unique>`. Never run a real agent.
- Never use a bare `git stash`.
- Code reads like its surroundings: plain-sentence doc comments, the same naming and idiom. Clean code in logical modules.
- Before each commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` all green.
- Commit messages are a plain-English summary like the git log's. They end with a blank line, then `Claude-Session: https://claude.ai/code/session_01J28o6ipYyGjMabKudJsm1y`.

---

### Task 1: the flow's graph in its pane

**Files:**
- **Create** `code/rl-cli/src/tui/canvas.rs`: `Canvas` and its box-joining, moved out of `graph.rs` unchanged. `graph.rs` imports it from there for now.
- **Create** `code/rl-cli/src/tui/flow_graph.rs`: the layout and drawing for one flow. Tests go in `flow_graph_tests.rs` beside it if they are long.
- **Modify** `code/rl-cli/src/tui/flows.rs` (`pane_lines`), `code/rl-cli/src/tui/pane.rs` (`flow_lines`, which passes the other flows), and `code/rl-cli/src/tui/mod.rs` (only where `flow_lines` is called).
- **Move to `code/rl-flow`**: `kind_matches` from `rl-daemon/src/flows/triggers.rs` (e.g. into `rl_flow::def` or a new `rl_flow::events`). The daemon then uses it from there, and the panel uses the same function.

**What the pane shows.** The steps list under ` steps` becomes a drawing:

```
 starts on   every 30m · on run.failed where cause flow:nightly ← nightly
 ╭──────────╮     ╭───────────╮
 │✓ fetch   │────▶│✓ summarise│
 ╰──────────╯  │  ╰───────────╯
               │ if errors > 0
               │  ╭──────────╮
               └─▶│? approve │
                  ╰──────────╯
 starts      triage (on decision.answered)
```

- **`starts on`** is the trigger in words: `when_words(&f.when())`, then each `on` pattern with its `where` in plain words. After a `←` come the flows that would start it (see below). With neither `when` nor `on`, it reads `by hand`.
- **Steps** are boxes in the order written, laid out by rank: a step with no `after` is rank 0, and one with `after` is one past the furthest of those.
  - Ranks go left to right. Steps of one rank stack top to bottom in written order.
  - An edge from each `after` step, or for rank 0 from nothing, has the step's `if:` as its label (`if <cond>`, using `Cond`'s `Display`), clipped to the space there is.
  - Each box shows its last-firing mark (`flows::glyph(status)`, coloured as the list does now) and the step's name, clipped.
  - With no firing, every step shows `glyph(Pending)` as today. An `ask` step's box shows `?` while it waits.
- **`starts`** names the flows this one starts (see below), each with the event that does it. It is left out when there are none.
- **Too narrow, or too many ranks to fit:** fall back to today's list exactly, with each line given the step's `after` names (`after fetch`) and its `if`. The test: the drawing's width exceeds the pane's.
- **The caret** (`at`, `Stop::Step`) still walks the steps in written order. The lit step's box border uses `theme::selected()`. In the list fallback it is today's `❯` line.
- **Kept as they are**, below the drawing: each step's action label and status words, on one line per step under the drawing, in written order. Each line is `name  status  action`, as the current list prints it but without the glyph. Nothing the list says today is lost.

**What starts it and what it starts.** This is computed in the panel from every loaded flow's `FlowDef`, with no ledger. Put it in a small module, e.g. `tui/flow_links.rs`, as pure functions with tests:
- **What a flow's firings raise:** its steps' runs raise `run.*` with cause `flow:<name>`. Its `ask` steps raise `decision.*`. Its last runs' `emitted` kinds (from `DigestItem.emitted` for runs whose cause is this flow) are its signals.
- **A flow B is started by flow A** when, for some event A raises, one of B's `on` patterns takes it (`kind_matches`) and B's `where` doesn't rule it out on cause. That means `cause` is unset, or a prefix of `flow:A`. Other `where` fields can't be known ahead, so they don't rule it out.
- **A flow never starts itself:** the daemon's `ItsOwnDoing` rule.
- **Flows with no `where` cause are not listed as started by every flow:** an unfiltered `on: run.failed` would hear any flow's failures. Such a link is listed only where B's `where.cause` names A (`flow:A` or a prefix that is exactly `flow:A`), or the event is one of A's own `emitted` signal kinds, or a `decision.*`.
- **Words:**
  - `← nightly` after `starts on`, several joined by `, `.
  - `starts      triage (on decision.answered)`, several joined by ` · `.

**Tests** (in `rl-cli`):
- Each case renders the drawing to plain text (`Canvas::plain` or `Line`s to strings) and asserts the exact text:
  - a flow with `fetch` then two steps after it, one with `if`
  - the last firing's marks
  - no firing
  - the caret on the second step
  - a pane too narrow, so the list fallback is used, with `after` and `if`
- **`starts on` words:** `every 30m`, an `on` with `where`, and a flow with neither (`by hand`).
- **Links:**
  - B with `on: run.failed`, `where: cause: flow:A` is started by A, and A's pane says `starts B (on run.failed)`.
  - B with an unfiltered `on: run.failed` is not listed under A.
  - B on A's emitted kind `deploy.done` is listed.
  - A flow never links to itself.
- The daemon's existing trigger tests still pass with `kind_matches` from `rl-flow`.

**Docs:** `docs/ARCHITECTURE.md` §5's pane bullet says a flow's pane draws its wiring, what starts it and what it starts. The graph bullet stays until Task 2.

---

### Task 2: the global graph goes

Remove it end to end. Nothing else's behaviour changes.

- **Panel (`code/rl-cli/src/tui/`):**
  - Delete `graph.rs` and `graph/` (`words.rs`, `parts.rs`, `lane_tests.rs`). Keep `canvas.rs`. Move anything from `graph/words.rs` that the flow graph uses into `flow_graph.rs` or `flows.rs`, or delete it.
  - In `mod.rs`, remove `mod graph`, the `GraphGet`/`GraphParams` imports, `View::Graph`, the `graph`/`graph_at` fields, `Place::Graph`, the fetch in refresh, and the graph arms and helpers: `graph_plan`, `selected_node`, `graph_step`, `graph_enter`, `flow_of_node`, `toggle_graph`, the graph answer handling and `draw_graph`. Also the pane branch for nodes, the hints, `^g` (key handling and the chord list), and the graph tests. Any comments that mention the graph are reworded.
  - In `palette.rs`, remove `Cmd::Graph`, the `/graph` entry, its key and its test, and change "list or graph" to fit.
  - In `shortcuts.rs` (`^h`), remove `Place::Graph` and its list, `Cmd::Graph`, `^g graph` from the hint lines, and the tests' expectations.
  - In `rows.rs`, remove `· /graph` from the headline strings and update their tests.
  - In `pane.rs`, the `^g shows its wiring` hints go: the wiring is in the pane now.
  - Also reword the comments in `query.rs` and `events.rs`.
- **Protocol:** delete `code/rl-proto/src/graph.rs` and its `lib.rs` line, and `GraphGet`/`GraphParams` from `methods.rs`. Any type used only by it goes with it (`Tally`, graph `FlowStatus`, …). Check for other users first.
- **Daemon:** delete `code/rl-daemon/src/graph.rs`, its `lib.rs` line, the `graph.get` handler and import in `handlers.rs`, and `code/rl-daemon/tests/graph.rs`.
- **Ledger:** delete `code/rl-ledger/src/graph.rs` (`runs_since`, `events_since`, `asks_since`) and its `lib.rs` line, unless something else uses them. Check first.
- **Free key:** `^g` becomes free. Don't rebind it.
- **Docs:**
  - In `docs/ARCHITECTURE.md`, update the §1 mermaid (`digest · summary · recap`) and the §5 chord list. Remove §5's graph bullet. If §6 mentions the graph, say there that a flow's wiring is drawn in its pane.
  - `README.md`, if it mentions `^g` or `/graph`.
  - Plans, specs and roadmap history stay as written.
- **Tests:** after removal, `rg -n 'graph' code/` shows no stale mention of the global graph. `canvas` and the flow graph remain. `cargo test --workspace` is green. Removing the graph's own tests is expected; no other test is weakened.

**Order:** Task 1, then Task 2, so the panel always has a way to see a flow's wiring.
