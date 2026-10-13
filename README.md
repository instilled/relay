# Relay

Relay keeps your agents and long-running work alive while you're gone, tells you what they need
and what they did, and runs recurring work as flows — with your yes where it matters.

Command: `rl`. Daemon: `rld`.

```text
  ▄     ▄██▄     ▄
 █  █    ██    █  █   relay 0.1.0 · ~/code                             all · agents · flows · hidden
  ▀    ▄█▀▀█▄    ▀    1 to decide · 1 working · 1 finished
    ▄▄██▄▄▄▄██▄▄
    ▀▀  ▀▀▀▀  ▀▀
──────────────────────────────────────────────────────────────────────────────────────────────────────
 ? api › Deploy api to staging?           to decide  tests pass on main, 3 commits since the last deploy

~/code/api
 ▸ cargo test                             done       exit 0 · 1s

~/code/web
 ▸ npm run dev                            working~   ready on :3000
```

Early; macOS and Linux. Not built yet: notifications, plugins, and per-run facts (commits, diffs,
cost) — see [the roadmap](docs/PRODUCT-ROADMAP.md).

## Install

Needs a recent stable Rust (CI uses 1.96) and `git`.

```sh
git clone https://github.com/instilled/relay && cd relay
cargo build --release
export PATH="$PWD/target/release:$PATH"   # rl, rld and rl-session must stay side by side
```

Or link the dev shims, which rebuild when the source changes: `ln -sf "$PWD/scripts/rl" "$PWD/scripts/rld" ~/.local/bin/`.

Everything lives under `~/.relay` (`RELAY_HOME` moves it). Any `rl` command starts `rld`, and runs
survive `rl daemon restart`.

## Use

```sh
rl setup                                  # which agents to offer, who writes summaries
rl up ~/code/api                          # a project Relay knows
rl                                        # the panel: type a task or a shell line; / for commands, ^h for keys
rl run --agent claude "make the flaky parser test deterministic"
rl run -- cargo test                      # any command; it outlives the terminal
```

Close the laptop. When you're back:

```sh
rl since                                  # what happened, and what needs you (the panel opens on it too)
rl join <run>                             # back in a run's terminal; ctrl-\ leaves it running
rl decisions                              # questions waiting on you
rl decide <id> yes
```

## Flows

A flow is a folder `~/.relay/flows/<name>/` with a `flow.md`: when it runs (`when: every 30m` or
`on:` an event), its steps (scripts, agents, questions to you) and how they connect. Nothing runs
until you approve the folder's exact contents, and any change needs your yes again.

```sh
rl flow new brief "every weekday at 7:30, sum up yesterday's PRs for me"
rl flow test brief                        # what would fire and run; starts nothing
rl flow list
```

Two to copy and adapt are in [examples/flows](examples/flows): `prod-logs` and `morning-brief`.

## For agents

`rl context` explains Relay to an agent; `rl --json …` makes every command machine-readable.

## Docs

- [PRODUCT.md](docs/PRODUCT.md) — what Relay is for, and where it's going.
- [ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the tree is wired, as built.
- [PRODUCT-ROADMAP.md](docs/PRODUCT-ROADMAP.md) — done and next.

Relay is a rebuild of [instilled/ax](https://github.com/instilled/ax), kept archived as the legacy reference.
