---
in: "@LIVE@/ws"
when: every 30s
steps:
  beat:
    run: "date +%T | tee -a pulse.log"
    tags: [live]
---
