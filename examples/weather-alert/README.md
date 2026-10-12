# Weather alert → a note, if you approve

A small, **runnable** flow — no API keys, no agent, just `curl` and `jq`. It's the same shape as
the roadmap's UC2 (watch something, decide, act on approval); swap the probe and the action for
your real ones (see the bottom) and it becomes "cloud logs → analyse → a Notion task".

```
when 07:00 daily        probe                      review                    you
      ──────────▶  check-weather ──out──▶  "sunny in Zurich… add a note?" ──▶ 1 / 2
                   (one line, or none)      (only when probe said something)    │
                                                                                ▼
                                                                     note: a line appended
```

## How the data moves

The folder is the flow: `flow.md` says when it fires and what its steps are, and
`check-weather` is its own script, first on its steps' `PATH`. At 07:00 `rld` fires it; the
`probe` step runs `check-weather`, which fetches today's forecast from
[Open-Meteo](https://open-meteo.com) (keyless) and prints one line when it's sunny or the chance
of rain is over the threshold. What it printed is the step's out. `review` comes after it, and
its `if: probe.out` skips it on a day the probe said nothing; otherwise Relay raises the
question with `{probe.out}` in its text, on top of the panel's list and in `rl decisions`.
Answering `note` runs that option as the step's continuation; `skip`, or 12 hours with no
answer, does nothing. Every run is in the firing's chain, under `cause: flow:weather-alert/<step>`.

A command never gets a placeholder (Relay refuses `{probe.out}` in a `run:` at load), so the
option asks the forecast again rather than echoing what the probe said.

## Run it

```bash
mkdir -p ~/.relay/flows && cp -R examples/weather-alert ~/.relay/flows/   # needs curl + jq
rl flow reload
rl flow approve weather-alert                  # a flow runs only once you've said yes to its folder

# don't wait for 07:00 — fire it now:
rl flow run weather-alert
```

`rl` then shows the question (when it's sunny or rain is likely) and — once you answer `note` —
the line appended to `~/weather-notes.txt`. Point `WEATHER_LAT` / `WEATHER_LON` /
`WEATHER_PLACE` somewhere else, or lower `RAIN_POP` (default 50), to make it ask. Editing a file
in the folder (by hand, not through `rl flow edit`) asks for the yes again.

## For real work

Keep the shape; change the two ends:

- **Probe** — instead of Open-Meteo, read your source and print what's new, e.g. Cloud Logging
  errors: `gcloud logging read 'severity>=WARNING' …`. A `when` firing hands its steps
  `$RELAY_LAST_FIRING` (when the previous firing started) so the probe need only look at what's
  new since.
- **Analyse** — put an agent step between the probe and the question:
  `triage: { after: probe, if: probe.out, run: { agent: claude } }`, its prompt under a
  `## triage` heading in `flow.md` with `{probe.out}` in it, and `ask: "{triage.out}"`.
- **Action** — instead of appending a line, create a Notion task, open a PR, page someone: any
  command, or an agent with a `## review.<option>` prompt, that the option runs.
- **Where** — `in: ~/Projects/your-repo` (a folder `rl add` registered) runs the steps there
  and puts the flow's row under that project.
