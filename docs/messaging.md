# Messaging between agents

Relay lets your agents leave each other messages. You don't learn a new command: you ask an agent
in plain words, and it uses Relay's tools.

## How it works

`rl setup` registers a small MCP server, `relay`, with each harness it finds (Claude, Codex,
Gemini, Antigravity, …). It gives every agent three tools:

| tool | does |
| --- | --- |
| `who` | lists the sessions it can message, one line each: address (and your name for the row), harness, state, folder. Its own is marked. |
| `send(to, text)` | leaves a message for one of them, by address or by the name you gave the row (`ctrl+r` in the list). A wrong address is an error that suggests close matches. |
| `inbox` | reads its waiting messages, oldest first, and clears them. |

A message is a file under `~/.relay/mail/`. It waits there until the recipient calls `inbox`.
How the recipient learns it has mail depends on its harness:

- **Codex** is pinged. `send` also runs `codex queue … --resume` on the recipient's thread, which
  queues a turn telling it to read its inbox. An idle thread wakes up and reads it.
- **Claude** depends on how the session stands:

  | the Claude session | how it learns | `send` says |
  | --- | --- | --- |
  | one of Claude's background sessions, idle | woken: Relay types `relay mail from <you>: call the relay inbox tool to read it` into it (through `claude attach` on a hidden terminal) | `· woke claude` |
  | one of Claude's background sessions that has stopped or ended | carried on in the background with that line as its prompt (`claude --resume … --bg`) | `· resumed claude` |
  | working, or waiting on you | Relay's hooks: when it finishes its turn, the hook keeps it going one more turn to read the mail | `· claude is busy; it will see it at the end of its turn` |
  | open in your terminal, or held by Relay | Relay's hooks: each of your prompts carries a line saying mail waits, and the end of a turn keeps it going to read it | nothing more |

  Relay types into an idle session only when its screen shows an empty prompt and nothing
  running, and never while you are attached to it from the list. Where it could not (you are
  attached, its screen was in doubt), `send` says why and, from another Claude, how to wake it
  with Claude's own `SendMessage`.
- **Gemini, Antigravity and the rest** find it when they call `inbox`; ask them to.

Replies are messages too: an agent that wants an answer asks for one, and the other agent `send`s
it back. Nothing is typed into a terminal you are using: the one line Relay types into an idle
Claude goes through an attach of its own, which it lets go of at once.

The sender is known automatically in a session Relay holds and in Claude. A Codex agent passes its
own address (`from`, `as`) itself: tell it which row it is if it asks.

## Use cases

The prompts below are what you type to an agent. Address the other session by what the list shows
for it: its name, the name you gave it, or its folder ("the codex session in shop-api"). The agent
calls `who` to find the address.

### Ask for a review and get the findings back

> Ask the codex session in shop-api to review my uncommitted diff in this repo and send its
> findings back to you. Then fix what it found.

The Claude session sends the request; Codex is pinged, reviews, and sends its findings back; Claude
reads them from its inbox at the end of its turn (the hook prompts it), or at once if it had gone
idle in Claude's background (Relay wakes it).

### Hand-off when something is done

> Run the test suite until it passes. When it does, tell the claude session "web" to rebase on
> main and rerun its build.

The message goes out only when the first agent gets there; "web" picks it up at its next turn.

### A planner and workers

> You're the planner. Split the migration plan in docs/plan.md into three independent tasks. Send
> one each to the sessions "worker-1", "worker-2" and "worker-3", and ask each to reply when done
> with a one-paragraph summary. Then wait for all three replies in your inbox and write the
> summary to docs/progress.md.

Start the workers first, from the list's command line (`claude …`, `codex …`) or with
`rl new --harness claude`, and name them with `ctrl+r`.

### Two repos, one API change

> I'm changing the `/orders` response in this repo (shop-api): `total` becomes `total_cents`.
> Send the codex session in web the new shape and ask it to update the client and tell you when
> it's done. Don't merge until it replies.

Each agent stays in its own repo and context; the message carries only what the other needs.

### Collect status from everyone

> Use who to list the other agents. Ask each one for a two-line status (what it's doing, what
> it's blocked on), then read the replies from your inbox and give me a table.

Codex sessions answer right away (they're pinged), and so do Claude's idle background sessions
(they're woken); busy Claude ones at the end of their turn; the others when they check their
inbox.

## Good to know

- Messages are plain text, and only the recipient reads them. `inbox` deletes them as it reads.
- `who` lists the sessions the list shows: the ones Relay holds, Claude's and Codex's own
  background sessions, and agents in other terminals that Relay's hooks told it of.
- An agent that isn't set up (`rl setup --check`) has no `relay` tools; run `rl setup` or `/setup`
  in the list.
- Codex asks you to trust Relay's hook once (`/hooks` in Codex); Claude may ask you to review it in
  `/hooks`.
