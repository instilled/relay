# Relay — product brief

Relay (command: `rl`) is Claude Code's agents view for every harness: one full-screen list of your
agent sessions and shells, in the terminal you already use. Enter attaches. Agents can message each
other. There is no daemon.

## Why

`claude agents` is the best way to watch a pile of agents, and it is Claude-only. Codex has
background sessions of its own too. The agent-agnostic tools (tuios, herdr, agent-deck and the like)
each reinvent the terminal with their own panes, and see only agents started inside them. Nobody
shows the harnesses' own sessions together. Relay is a thin view over those, in your own terminal,
with its own holder only where a harness has no background of its own.

## What it is

- **One list.** Every agent session across harnesses, and your shells: state, name, harness, folder,
  age. Grouped by project, or by state (Needs input, Working, Done).
- **Attach.** Enter opens the session: its terminal for one Relay holds, the harness's own attach
  for one in Claude's or Codex's background. `ctrl+\` leaves it running.
- **Keeps running.** A held session lives in its own small process; closing the terminal or the
  laptop costs nothing.
- **Messages.** `rl mcp` gives any agent three tools: `who`, `send`, `inbox`.
- **Any harness.** A harness is a YAML profile. One that uses the integration Relay already has is a
  file; one with a session protocol of its own (like Codex's app-server) needs code.

## What it is not

- No daemon, no database, no service of its own.
- No model calls. Relay writes no summaries and asks nobody anything.
- No flows, scheduler, decisions queue or catch-up.
- No tiling, panes or multiplexer. The terminal you have is the terminal.
- No plugin host. The one badge on a project header (`gh-pr`) is a script.

## Who it is for

Someone who runs several agents at once, in more than one harness, and wants to see which need them
and get back into any of them. One person, one machine.

## How it feels

`rl` once, and setup walks you through. Then `rl`: the list, already right. Type a prompt to start
an agent, enter goes in, `ctrl+\` comes out. State is live where the harness has hooks and read from
the screen where it does not, so a row is never blank, only less sure.

See [PRODUCT-ROADMAP.md](PRODUCT-ROADMAP.md) for what's next and [ARCHITECTURE.md](ARCHITECTURE.md)
for how it is built.
