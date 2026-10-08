---
when: "0 7 * * *"
steps:
  probe:
    run: check-weather
  review:
    after: probe
    if: probe.out
    ask: "{probe.out} — add a note?"
    options:
      note: { run: 'check-weather >> "$HOME/weather-notes.txt"' }
      skip: {}
    default: skip
    expires: 12h
---

Every morning at 07:00 the probe looks at today's forecast. It prints one line when it is
sunny or rain is likely, and nothing otherwise, so `review` asks only on a day worth a note.
`note` writes the forecast to ~/weather-notes.txt; `skip`, or no answer in 12 hours, does
nothing.
