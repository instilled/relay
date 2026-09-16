# Relay Product Roadmap

Goal for v1:

* UC1: i want to check notion via api (cli client) periodically for new feedback log items, e.g. 1h. if it has output claude should work on it and trigger a skill then surface recommendations to me i can accept or reject.   
* UC2: i want to check google cloud logs. on error / warn i want to trigger claude to analyse and create a notion task if i approve

## Open — in the order they'd be built

* [ ] ctrl-x should ask to delete hitting it a second time delete -> same as claude agent view. and ctrl-c dito
* [ ] persistent project shell — multiple shells per project + switching between them ("panes", not a multiplexer); per-command facts via shell prompt integration (OSC 133 / precmd) so exit codes and history survive; a distinct "shell" presentation in the list
* [ ] agent-attach reliability — join a just-spawned holder without bouncing back to the list
* [ ] ts plugin subsystem
* [ ] investigate a lightweight JS/TS SDK for plugins — a thin wrapper around the `rl emit` / `rl ask` / `rl run` contract so a plugin is a few lines of TS that reads its input and emits events, decisions or runs (a plugin is just a command; the SDK makes the common case trivial)
* [ ] inter-run data passing — events carry small facts well, but bulk (a log dump, a file, a transcript) shouldn't ride through argv/env/the ledger; give each run a scratch dir (`$RELAY_RUN_DIR`) so runs pass bulk by path, not bytes (single-host); promote to a network-addressable run-artifact store — the daemon serving a blob by id/key — when runs span hosts
* [ ] pipelines & easy wiring & visualization of "graph" — `workflows/<name>.yaml` engine (steps with `after`/`when` and decision steps) plus a graph view of how runs, schedules and reactions wire together
* [ ] facts collector (J6) — git commits + `diff --stat` per run, transcript ingestion (Claude/Codex `*.jsonl`), tokens/cost; facts-first run detail (facts on top, narrative below, transcript behind enter)
* [ ] retention purge of archived runs — today archive is out-of-list only; actually delete past the retention window
* [ ] worktrees & ref rules — `--worktree`, branch naming, `.relay/config.yaml` ref derivation
* [ ] notifications — reference notify plugins (Slack / mail on `decision.raised`, `run.blocked`)

## Done — newest first

* [x] command list shows key shortcuts — `^x` / `^c` beside the cursor-navigable `/` commands (which already highlight the row and put it in the line on enter)
* [x] add a folder from the panel — `/add-dir <path>` registers a workspace, landing on its new row (`workspace.up`)
* [x] the global "yours" rules are a persistent, selectable project row — leads on a waiting yes, else trails; its pane lists the global schedules & reactions
* [x] every registered workspace shows as a project row, even with no runs
* [x] rename shared-* crates to rl-core / rl-detect / rl-ledger / rl-proto (dirs, package names, imports)
* [x] emit custom events — `rl emit` puts a custom event in the ledger, matched by `where.data`; a scheduled probe → reaction → decision (UC2), with a keyless weather example
* [x] warm per-project shell (slice 1) — a shell line runs in a persistent `$SHELL -i` per folder, not a fresh run each time (`run.input`)
* [x] trustworthy stop — reaps the whole process group (PTY + pipe children); a run's last output reaches viewers before the exit
* [x] manual commands run in your real shell — aliases and functions resolve, recognised via `$SHELL` (`/reindex`)
* [x] selectable project rows — stats pane, `/rules` to edit its schedules & reactions, `^r` rename (`workspace.rename`)
* [x] arrow-key navigation of command candidates
* [x] multi-line prompt input — shift-enter drops a newline, enter still sends
* [x] real terminal handover — raw attach/join passthrough (`rl join` standalone), watch tile, `/full`, `ctrl-\` detach
* [x] archive & done — `ctrl-x`, `/done`, `/done-all` (archive now, purge on retention later)
* [x] lens toggle — all / agents / jobs / reactions
* [x] rich TUI — colour carries run state; header, lens, gutter and section rules; detail pane beside the list
