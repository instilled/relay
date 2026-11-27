# Relay roadmap

Goal: **`claude agents` for every harness, plus shells, plus messaging between agents.** One list
of every agent session in the terminal you already use; attach full-screen, `ctrl+\` back. No
daemon: `rl`, `rl-session` holders and plain files. What is done is in git.

## Next

* Antigravity hooks — its `hooks.json` events differ from Gemini's and have no permission event,
  so its rows can't say "needs input" (today its state is a guess from the screen: `working?`).
* Reply from the peek pane without attaching (peek itself shipped in #237).

## Later

* `option+enter` as a new line under tmux (pending a capture of the bytes it sends).
* Antigravity in its own background (`agy remote-control`, untested for local sessions).
* `rl mcp` inside Codex knowing its caller (Codex runs MCP servers without the session's env).
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

## Done lately

The intent-level `Harness` trait (stable contracts only, no screen reading to act); `/project`;
peek (`space`); `ctrl+\` detaching under CSI u terminals; the first frame inside the settle;
socket paths kept under macOS's 104 bytes (`rl-core`, hashed when long). Git has the rest.
