---
in: "@LIVE@/ws"
on: decision.raised
steps:
  tell:
    run: 'echo "$(date +%T) ASK $RELAY_EVENT_SUMMARY" >> asked.log'
---

A question raised anywhere leaves a line. This is how decisions reach you:
Relay never talks to a service itself, a flow's step does.
