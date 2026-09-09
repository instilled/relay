# Relay

Relay keeps your agents and long-running work alive when you're gone, tells you what they need and what they did, runs work on a schedule, and is extended by small plugins that run in their own process — plugins that agents can write.

Command: `rl`. Daemon: `rld`.

- [docs/PRODUCT.md](docs/PRODUCT.md) — the product brief: jobs to be done, features, plugin system, architecture, phasing.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the tree is wired, as built: processes, crates, run lifecycle, detection.

Relay is a fresh rebuild of [instilled/ax](https://github.com/instilled/ax), which stays archived as the legacy reference.
