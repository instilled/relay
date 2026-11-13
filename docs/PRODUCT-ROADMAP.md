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
hooks; ctrl+x hides it and enter says to switch to it there (#228). Hidden rows stay off the
list even when one needs you, counted on the toggle (`◌ hidden 2`), marked `◌ hidden` when
shown, and rows move with them shown (#229); a typed line starts in the row's project root, not
a session's worktree (#230).

## Next

Toward announcing (2026-11-13): it is ready for a quiet beta with people who run more than one
harness once CI is green and it installs without Rust; a public announcement waits for peek and
reply and a short demo.

* CI green again: the runs for #227 and #228 never started (GitHub billing); re-run them on main.
* Peek and reply from the list without attaching (as `claude agents`' space): the core loop of
  watching a pile of agents, today a full attach and `ctrl+\` each time.
* An install without Rust: release binaries for macOS and Linux (`rl`, `rl-session`) built on a
  tag, a one-line install in the README; the repo public. Unsigned for the beta: `curl` and a
  Homebrew tap set no quarantine, and Rust's linker ad-hoc signs arm64. A browser download gets
  the "cannot be verified" dialog; the README gives `xattr -d com.apple.quarantine rl rl-session`.
* Verify live before strangers hit it: Relay starting the Codex daemon, `/stop` on a Codex turn
  (`turn/interrupt`), `thread/delete`, the Gemini MCP entry.
* A 60-second demo: a Claude and a Codex on the list, one asks the other to review, the reply
  comes back.
* The list ignored SIGTERM once (2026-11-13, a #226 build, no child): find which state doesn't
  listen for the stop, and make every state leave cleanly on it.
* Verify live that Claude's hooks fire on a session woken with `--resume --bg`.

## Later

* `option+enter` as a new line under tmux (pending a capture of the bytes it sends).
* Antigravity hooks — its `hooks.json` events differ from Gemini's and have no permission event,
  so its rows can't say "needs input".
* Antigravity in its own background (`agy remote-control`, untested for local sessions).
* `rl mcp` inside Codex knowing its caller (Codex runs MCP servers without the session's env).
* Socket paths that can't overflow macOS's ~104 bytes.
* Lister timeouts that leave no threads lingering.
* `rl setup --harness X` through the wizard, not only plain output.
* Compatibility CI: recorded fixtures of the real CLIs, and a nightly run against them.
* Notarized macOS binaries, by the public announcement or the first user hitting the dialog:
  the Apple Developer Program (~$99/yr) for a Developer ID; the release workflow runs `codesign`
  and `xcrun notarytool submit --wait`, the cert and an app-specific password as GitHub secrets.
* Verify live what's left: an `agy` session calling `who`, `claude rm`.

## Dropped

* Flows, schedules and reactions; the report arc, briefer, catch-up and summaries; the co-worker
  layer, notifications, the pane redesign and the project overview (2026-11-07 slim pivot).
* The daemon, ledger and protocol crates (`rld`, `rl-daemon`, `rl-proto`, `rl-ledger`, `rl-flow`,
  `rl-cli`).
* Archiving Codex threads (`ctrl+x` removes instead).
