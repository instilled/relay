# Messaging between agents

Relay lets your agents leave each other messages. You don't learn a new command: you ask an agent
in plain words, and it uses Relay's tools.

## How it works

`rl setup` registers a small MCP server, `relay`, with each harness it finds (Claude, Codex,
Gemini, Antigravity, …). It gives every agent three tools:

| tool | does |
| --- | --- |
| `who` | the sessions it can message, one line each: address (and your name for the row), harness, state, folder. Its own is marked. |
| `send(to, text)` | leaves a message for one of them, by address or by the name you gave the row (`ctrl+r`). A wrong address is an error with close matches. |
| `inbox` | its waiting messages, oldest first, which it then has read. `inbox(recent: true)` shows again those of the last 7 days and changes nothing: for an agent that lost them (context compacted, client crashed). |

A message is a file under `~/.relay/mail/` and waits there until the recipient calls `inbox`. How
the recipient learns of it depends on its harness:

- **Codex** is pinged: `send` also runs `codex queue … --resume` on its thread, which queues a turn
  telling it to read its inbox. An idle thread wakes up and reads it.
- **Claude** depends on how the session stands:

  | the Claude session | how it learns | `send` says |
  | --- | --- | --- |
  | in Claude's background, idle | stopped (`claude stop`) and carried on in the background with `relay mail from <you>: call the relay inbox tool to read it` as its prompt (`claude --resume … --bg`) | `· woke claude` |
  | in Claude's background, ended | carried on the same way | `· woke claude` |
  | in Claude's background, working | Relay's hooks: at the end of its turn it is kept going one more turn to read the mail | `· claude is busy; it will see it at the end of its turn` |
  | in Claude's background, waiting on you | Relay's hooks, once you answer | `· claude waits on you; it will see it once you answer` |
  | attached from the list | Relay's hooks, at your next prompt | `· you are attached to it; it will see it at your next prompt` |
  | open in your terminal, or held by Relay | Relay's hooks: each prompt carries a line saying mail waits, and the end of a turn keeps it going to read it | nothing more |

  Carrying a Claude on starts a new background session on the same conversation, under a new id:
  Relay moves your name, pin, colour and place for the old row to the new one and hides the old
  (`ctrl+o` shows it; nothing is removed), and mail for the old one, or sent to it later, reaches
  the new one. Where it could not wake it, `send` says why and, from another Claude, how to wake
  it with Claude's own `SendMessage`.
- **Gemini, Antigravity and the rest** find it when they call `inbox`; ask them to.

Replies are messages too: an agent that wants an answer asks for one. Relay never types into a
session or reads its screen: it uses only the harness's documented commands and its hooks.

The sender is known automatically in a session Relay holds and in Claude. A Codex agent passes its
own address (`from`, `as`): tell it which row it is if it asks.

## Use cases

What you type to an agent. Name the other session by what the list shows for it: its name, the name
you gave it, or its folder ("the codex session in shop-api"); the agent calls `who` for the address.

**A review, findings back.**

> Ask the codex session in shop-api to review my uncommitted diff in this repo and send its
> findings back to you. Then fix what it found.

Codex is pinged, reviews and sends back; Claude reads the findings at the end of its turn (the hook
prompts it), or at once if it had gone idle in its background (Relay wakes it).

**A hand-off when something is done.**

> Run the test suite until it passes. When it does, tell the claude session "web" to rebase on
> main and rerun its build.

**A planner and workers.**

> You're the planner. Split the migration plan in docs/plan.md into three independent tasks. Send
> one each to the sessions "worker-1", "worker-2" and "worker-3", and ask each to reply when done
> with a one-paragraph summary. Then wait for all three replies in your inbox and write the
> summary to docs/progress.md.

Start the workers first (`claude …` or `codex …` on the list's command line, or `rl new --harness
claude`) and name them with `ctrl+r`.

**Two repos, one API change.**

> I'm changing the `/orders` response in this repo (shop-api): `total` becomes `total_cents`.
> Send the codex session in web the new shape and ask it to update the client and tell you when
> it's done. Don't merge until it replies.

**Status from everyone.**

> Use who to list the other agents. Ask each one for a two-line status (what it's doing, what
> it's blocked on), then read the replies from your inbox and give me a table.

## Good to know

- Messages are plain text, read only by the recipient. A message is marked delivered only once the
  reply carrying it is written; if that fails, or `rl mcp` dies in between, the next `inbox` gets
  it again. Delivered messages are kept 7 days, then deleted. The hooks' "you have N messages"
  counts only those not yet delivered.
- `who` lists what the list shows: sessions Relay holds, Claude's and Codex's own background
  sessions, and agents in other terminals that Relay's hooks told it of.
- An agent that isn't set up (`rl setup --check`) has no `relay` tools: run `rl setup`, or `/setup`
  in the list. Codex asks you to trust Relay's hook once (`/hooks`); Claude may ask you to review it.
