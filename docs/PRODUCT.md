# Relay — product brief

**Status:** as built · **Date:** 2026-11-08

Relay (command: `rl`) is Claude Code's agents view, for every harness. One full-screen list of your
agent sessions and shells, in the terminal you already use. Enter attaches. Agents can message
each other. There is no daemon.

## Why

`claude agents` is the best way to watch a pile of agents, and it is Claude-only. Codex now has
background sessions of its own too. The agent-agnostic tools (tuios, herdr, agent-deck and the
like) each reinvent the terminal, with their own panes, and see only agents started inside them.
Nobody shows the harnesses' own sessions together. Relay is a thin view over those, run in your own
terminal, with its own holder only where a harness has no background of its own.

## What it is

- **One list.** Every agent session across harnesses, and your shells. A row has a state, a name,
  the harness, the folder and an age. Grouped by project, or by state (Needs input, Working, Done).
- **Attach.** Enter opens the session. For a session Relay holds, that is its terminal; for one in
  Claude's or Codex's background, it is that harness's own attach. `ctrl-\` leaves it running.
- **Keeps running.** A held session lives in its own small process, so closing the terminal or the
  laptop costs nothing.
- **Messages.** `rl mcp` gives any agent three tools: `who`, `send`, `inbox`.
- **Any harness.** A harness is a YAML profile. Adding one is a file, not code.

## What it is not

- No daemon, no database, no network.
- No model calls. Relay writes no summaries and asks nobody anything.
- No flows, no scheduler, no decisions queue, no catch-up, no cost.
- No tiling, panes or multiplexer. The terminal you have is the terminal.
- No plugin host. The one badge on a project header (`gh-pr`) is a script.

## Who it is for

Someone who runs several agents at once, in more than one harness, and wants to see which need
them and get back into any of them. It is a tool for one person on one machine.

## How it feels

`rl setup` once. Then `rl`: the list, already right. `n` starts something, enter goes in,
`ctrl-\` comes out. State is live where the harness has hooks and read from the screen where it
does not, so a row is never blank, only less sure.

## Where it is going

See [PRODUCT-ROADMAP.md](PRODUCT-ROADMAP.md). How it is built is in [ARCHITECTURE.md](ARCHITECTURE.md).
