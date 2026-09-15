# Relay Product Roadmap

## Open — in the order they'd be built

* [ ] every registered workspace shows as a project row even with no runs — a folder you `rl up`'d is always on the list, so you can select it, read its stats, edit its rules, or open its shell before anything has happened there
* [ ] persistent project shell — multiple shells per project + switching between them ("panes", not a multiplexer); per-command facts via shell prompt integration (OSC 133 / precmd) so exit codes and history survive; a distinct "shell" presentation in the list
* [ ] facts collector (J6) — git commits + `diff --stat` per run, transcript ingestion (Claude/Codex `*.jsonl`), tokens/cost; facts-first run detail (facts on top, narrative below, transcript behind enter)
* [ ] agent-attach reliability — join a just-spawned holder without bouncing back to the list
* [ ] retention purge of archived runs — today archive is out-of-list only; actually delete past the retention window
* [ ] rename shared-* modules to rl-core, rl-detect, rl-ledger, rl-proto (as well as the cargo modules)
* [ ] ts plugin subsystem
* [ ] pipelines & easy wiring & visualization of "graph" — `workflows/<name>.yaml` engine (steps with `after`/`when` and decision steps) plus a graph view of how runs, schedules and reactions wire together
* [ ] worktrees & ref rules — `--worktree`, branch naming, `.relay/config.yaml` ref derivation
* [ ] notifications — reference notify plugins (Slack / mail on `decision.raised`, `run.blocked`)

## Done — newest first

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
