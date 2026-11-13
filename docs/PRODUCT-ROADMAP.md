# Relay roadmap

Goal: **`claude agents` for every harness, plus shells, plus messaging between agents.** One list
of every agent session in the terminal you already use; attach full-screen, `ctrl+\` back. No
daemon: `rl`, `rl-session` holders and plain files. What is done is in git.

## Now

* Nothing in flight.

Last landed (2026-11-12): `ctrl+j` adds a line (#221); `ctrl+x` asks stop / remove the same for
every harness, Codex removing by `thread/delete` (#222); empty Codex threads not listed (#223); a
close always ends a held run (#224); the docs slimmed (#225); `/project <folder>` (#226); one
intent-level `Harness` interface on stable contracts only — no typing, no screen watch; an idle
Claude woken by `claude stop` + `--resume --bg`, its row and mail carried to the forked session
(#227). 2026-11-13: a Claude open in another terminal goes by its own name, one row with its
hooks; ctrl+x hides it and enter says to switch to it there (#228).

## Next

* The list ignored SIGTERM once (2026-11-13, a #226 build, no child): find which state doesn't
  listen for the stop, and make every state leave cleanly on it.
* Verify live that Claude's hooks fire on a session woken with `--resume --bg`.

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
