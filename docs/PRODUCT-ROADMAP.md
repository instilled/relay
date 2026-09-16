# Relay Product Roadmap

Goal for v1:

* UC1: i want to check notion via api (cli client) periodically for new feedback log items, e.g. 1h. if it has output claude should work on it and trigger a skill then surface recommendations to me i can accept or reject.   
* UC2: i want to check google cloud logs. on error / warn i want to trigger claude to analyse and create a notion task if i approve

## Open — in the order they'd be built

* [ ] the `/rules` screen — designed properly: it does not show where you are (which folder, global or all), and it should read like the rest of the panel
* [ ] hide & a "hidden" lens — hide any item of a project by hand (runs included) and find it again under its own tab; the counterpart to `^p` pin
* [ ] git backed workspaces should not hide the "name" the workspace should carry an icon after the path denoting git backing. and the current branch in brackets?!?! (or right aligned)
* [ ] agent-attach reliability — join a just-spawned holder without bouncing back to the list
* [ ] CI on GitHub — Actions runners have not started since 09-15 (private repo, free plan: jobs fail with no steps); `scripts/ci` in Docker is the gate until billing is sorted
* [ ] ts plugin subsystem
* [ ] pipelines & easy wiring & visualization of "graph" — `workflows/<name>.yaml` engine (steps with `after`/`when` and decision steps) plus a graph view of how runs, schedules and reactions wire together
* [ ] facts collector (J6) — git commits + `diff --stat` per run, transcript ingestion (Claude/Codex `*.jsonl`), tokens/cost; facts-first run detail (facts on top, narrative below, transcript behind enter)
* [ ] retention purge of archived runs — today archive is out-of-list only and `rl prune` lets the journal go; actually delete runs past the retention window
* [ ] worktrees & ref rules — `--worktree`, branch naming, `.relay/config.yaml` ref derivation
* [ ] notifications — reference notify plugins (Slack / mail on `decision.raised`, `run.blocked`)

## Done — newest first

* [x] pin — `^p` / `/pin` keeps a run above every project, whatever its age or the lens; `/archive seen` leaves it alone (`run.pin`)
* [x] CLI names match the panel — `rl archive` and `rl prune` (`rl done` and `rl gc` still answer)
* [x] the cursor follows the mouse; a joined shell or agent shows its caret; the global row is `global · ~/.relay` (was "yours")
* [x] command & shortcut cleanup — one name per act (`/archive [seen]`, `/pane [full]`, `/reload [rules|shell]`, `/edit [file|schedule|reaction]`), keys `^x` `^r` `^o` `^t` `^c` shown in the command list
* [x] every project is on the list under every lens — registered folders even before anything ran, and the global row always (`/up` registers from the panel)
* [x] `/edit schedule|reaction` opens the rule file for the scope under the cursor
* [x] rename anything — `^r` on a folder, a run or a shell (`run.rename`), a job through its rule
* [x] shift-enter works in kitty-protocol terminals
* [x] esc closes the pane first
* [x] CI checks on Linux in Docker — `scripts/ci`
* [x] persistent project shell — several shells per folder reached from the list, per-command facts via prompt marks (OSC 133: what ran, exit code, into the ledger), a shell reads as a shell and leads its folder, prompt can show it is a Relay shell
* [x] mouse reaches the list; the selected row wears a band
* [x] `ctrl-x` and `ctrl-c` ask first; the command list scrolls and shows key shortcuts
* [x] UC2 wiring — logs check → analyse → a Notion task on approval (`examples/weather-alert`), `rl emit` for custom events
* [x] rename shared-* crates to rl-core, rl-detect, rl-ledger, rl-proto
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
