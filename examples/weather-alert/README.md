# Weather alert → a note, if you approve

A small, **runnable** example of Relay's source→reaction→decision wiring — no API
keys, no agent, just `curl` and `jq`. It's the same shape as the roadmap's UC2
(watch something, decide, act on approval); swap the probe and the action for
your real ones (see the bottom) and it becomes "cloud logs → analyse → a Notion
task".

```
schedule (07:00 daily)          reaction                    you
  check-weather ──emit──▶  weather.alert ──────────────▶  rl ask ──▶ approve
                           (sunny | rain-likely)                       │
                                                                       ▼
                                                              append a note
```

## How the data moves

`check-weather` fetches today's forecast from [Open-Meteo](https://open-meteo.com)
(keyless) and, when it's sunny or the chance of rain is over the threshold, runs
`rl emit weather.alert --data condition=… --data place=… --data rain_pct=…`.
That call travels as JSON-RPC over `~/.relay/run/rld.sock`, lands as an event row
in the ledger, and the reactor offers it to `reactions.yaml`. The reaction's
`where.data.condition` narrows it, `{data.*}` fills the question and the note it
carries, and approving `note` runs the action. Inside the scheduled run `rl emit`
inherits `$RELAY_RUN`, so the alert is filed under this folder and joins the chain.

## Run it

```bash
cp check-weather ~/bin/ && chmod +x ~/bin/check-weather   # needs curl + jq
cat schedules.yaml >> ~/.relay/schedules.yaml
cat reactions.yaml >> ~/.relay/reactions.yaml
rl daemon restart          # or: rl schedule reload && rl react reload
rl rules                   # approve check-weather and weather-advisory with `y`

# don't wait for 07:00 — fire it now:
rl schedule run check-weather
```

`rl` then shows the alert (when it's sunny or rain is likely), the question it
raises, and — once you approve — the line appended to `weather-notes.txt`.
Point `WEATHER_LAT` / `WEATHER_LON` / `WEATHER_PLACE` somewhere else, or lower
`RAIN_POP` (default 50), to make it fire.

## For real work

Keep the wiring; change the two ends:

- **Probe** — instead of Open-Meteo, read your source and emit per finding, e.g.
  Cloud Logging errors: `gcloud logging read 'severity>=WARNING' … | … rl emit
  log.issue --data severity=… --data message=…`. Relay hands the probe
  `$RELAY_LAST_RUN` (the previous firing) so it need only look at what's new.
- **Reaction** — instead of asking directly, have an agent analyse first:
  `run: { agent: claude, prompt: "… {data.message} …" }`, and let the agent
  `rl ask` with an approve option.
- **Action** — instead of appending a line, create a Notion task, open a PR,
  page someone: any command the approve option runs.
