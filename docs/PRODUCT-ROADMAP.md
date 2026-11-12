# Relay roadmap

Goal: **`claude agents` for every harness, plus shells, plus messaging between agents.** One list
of every agent session in the terminal you already use; attach full-screen, `ctrl+\` back. No
daemon: `rl`, `rl-session` holders and plain files. What is done is in git.

## Now

* Slim the docs: README, ARCHITECTURE, this roadmap; the old plans and specs go.

## Next

* One intent-level `Harness` interface — info, sessions, start, attach, actions, stop, close,
  rename, notify, setup, caller, check — each harness binding its own steps. Stable contracts only
  (documented CLI, JSON, Codex's schema): no screen reading or typing. An idle Claude is woken by
  `claude stop` + `claude --resume <id> --bg`; the ← screen watch goes.
* `/project <folder>`.

## Later

* `option+enter` as a new line under tmux (pending a capture of the bytes it sends).
* Antigravity hooks — its `hooks.json` events differ from Gemini's and have no permission event,
  so its rows can't say "needs input".
* Antigravity in its own background (`agy remote-control`, untested for local sessions).
* `rl mcp` inside Codex knowing its caller (Codex runs MCP servers without the session's env).
* Peek and reply from the list without attaching (as `claude agents`' space).
* Socket paths that can't overflow macOS's ~104 bytes.
* Lister timeouts that leave no threads lingering.
* `rl setup --harness X` through the wizard, not only plain output.
* Compatibility CI: recorded fixtures of the real CLIs, and a nightly run against them.
* Verify live what's left: an `agy` session calling `who`, `/stop` on a Codex turn open elsewhere,
  Relay starting the Codex daemon, `claude rm`.

## Dropped

* Flows, schedules and reactions; the report arc, briefer, catch-up and summaries; the co-worker
  layer, notifications, the pane redesign and the project overview (2026-11-07 slim pivot).
* The daemon, ledger and protocol crates (`rld`, `rl-daemon`, `rl-proto`, `rl-ledger`, `rl-flow`,
  `rl-cli`).
* Archiving Codex threads (`ctrl+x` removes instead).
