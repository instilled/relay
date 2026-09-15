# Cloud logs → analyse → a Notion task, if you approve

UC2 from the roadmap, wired end to end on Relay's own primitives — a schedule, a
custom event, a reaction, an agent, and a decision. Nothing here is special to
Relay: the two scripts are yours to adapt, and the two YAML files are ordinary
`schedules.yaml` / `reactions.yaml` entries.

```
schedule (every 1h)          reaction                     you
  scan-logs  ──emit──▶  log.issue ──▶ claude analyses ──▶ rl ask ──▶ approve
                        (ERROR/WARN)                                   │
                                                                       ▼
                                                                  notion-task
```

## The pieces

- **`scan-logs`** — the probe. A schedule runs it on a cadence; it reads Cloud
  Logging for `ERROR`/`WARNING` entries since the last scan and `rl emit`s one
  `log.issue` event per entry. Relay hands it `$RELAY_LAST_RUN` (the previous
  firing, empty the first time) so it need not keep its own clock, and inside
  the run `rl emit` inherits `$RELAY_RUN`, so each issue is filed under this
  folder and joins the flow. You provide `gcloud` (authenticated) and `jq`.
- **`reactions.yaml`** — on each `log.issue` of severity `ERROR`/`WARNING`, run
  `claude` to decide whether it is real. The event's payload is filled into the
  prompt as `{data.severity}` / `{data.message}`.
- The agent, if the issue is real, runs **`rl ask`** with an approve option
  whose action is **`notion-task`** — so the task is created only when you say
  so. Noise it drops silently.
- **`notion-task`** — the action. Creates a Notion task from a title and a
  one-line cause. You provide the Notion token and database id.

## Install

```bash
# 1. put the scripts on your PATH (and make them executable)
cp scan-logs notion-task ~/bin/ && chmod +x ~/bin/scan-logs ~/bin/notion-task

# 2. give them what they need
export GOOGLE_CLOUD_PROJECT=my-project        # scan-logs
export NOTION_TOKEN=secret_…                   # notion-task
export NOTION_DATABASE_ID=…                    # notion-task

# 3. add the rules (globally, or into a folder's .relay/)
cat schedules.yaml >> ~/.relay/schedules.yaml
cat reactions.yaml >> ~/.relay/reactions.yaml
rl daemon restart          # or: rl schedule reload && rl react reload
rl rules                   # approve `scan-logs` and `analyse-log-issue` with `y`
```

Then watch it: `rl` shows the scan firing, the questions it raises, and the
tasks made on approval. To try the wiring without waiting an hour or touching
Cloud Logging, fire it by hand: `rl schedule run scan-logs`.
