# Relay

Relay keeps your agents and long-running work alive when you're gone, tells you what they need and what they did, runs recurring work as flows, with your yes where it matters, and is extended by small plugins that run in their own process — plugins that agents can write.

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

Relay is early and runs on macOS and Linux. Not built yet: notifications (you find out by opening the panel or `rl since`), and the facts collector (commits, diffs and cost per run). See [the roadmap](docs/PRODUCT-ROADMAP.md).

## Install

You need a recent stable Rust toolchain (CI uses 1.96) and `git`. From a checkout:

```sh
git clone https://github.com/instilled/relay && cd relay
cargo build --release
export PATH="$PWD/target/release:$PATH"   # rl, rld and rl-session must stay side by side
```

To follow the repo instead, link the dev shims once. They rebuild when the source changes:

```sh
ln -sf "$PWD/scripts/rl" "$PWD/scripts/rld" ~/.local/bin/
```

Relay keeps everything under `~/.relay` (`RELAY_HOME` moves it). Every `rl` command starts `rld` if it isn't running. Runs survive `rl daemon restart`.

## Five minutes with it

```sh
rl setup                                  # which agent CLIs the panel offers, who writes summaries
rl up ~/code/api                          # a project Relay knows, listed even before anything runs
rl                                        # the panel
```

In the panel, type what you want done and press enter, or start a shell line. `/` lists the commands and `?` the keys. Or from any shell:

```sh
rl run --agent claude "make the flaky parser test deterministic"
rl run -- cargo test                      # any command; it keeps running when you close the terminal
```

Now leave: close the terminal, close the laptop. When you're back:

```sh
rl since                                  # what happened while you were away, and what needs you
rl join <run>                             # be in a run's terminal again; ctrl-\ leaves it running
rl decisions                              # questions waiting on you
rl decide <id> yes
```

Opening `rl` shows the same catch-up at the top of the panel.

## Flows: work that starts without you

A flow is a folder in `~/.relay/flows/<name>` with a `flow.md`: when it runs (`when: every 30m`, or `on:` an event), its steps (scripts, agents and questions to you), and how they wire together. Nothing runs until you approve the folder's exact contents, and a change needs your yes again.

```sh
rl flow new logs --from prod-logs --set project=my-gcp-project --in ~/code/api
rl flow new brief "every weekday at 7:30, sum up yesterday's PRs and chat for me"
rl flow test logs                         # what would fire it and which steps would run; starts nothing
rl flow list
```

## For agents

```sh
rl context    # what Relay is, its events and flows, and the answer it wants — read this first
rl --json …   # every command, machine-readable
```

## More

- [docs/PRODUCT.md](docs/PRODUCT.md) — the product brief: jobs to be done, features, plugin system, architecture, phasing.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the tree is wired, as built: processes, crates, run lifecycle, detection, flows.
- [docs/PRODUCT-ROADMAP.md](docs/PRODUCT-ROADMAP.md) — what's done and what's next.

Relay is a fresh rebuild of [instilled/ax](https://github.com/instilled/ax), which stays archived as the legacy reference.
