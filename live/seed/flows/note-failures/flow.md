---
in: "@LIVE@/ws"
on: run.failed
steps:
  note:
    run: 'echo "$(date +%T) $RELAY_EVENT_SUMMARY" >> failures.log'
---

Every failure leaves a line you can tail. A command never gets a placeholder:
it reads the event from its environment.
