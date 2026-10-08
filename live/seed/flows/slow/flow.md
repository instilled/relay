---
in: "@LIVE@/ws"
when: every 3m
overlap: skip
steps:
  six:
    run: 'for i in 1 2 3 4 5 6; do echo "step $i of 6"; sleep 5; done'
    tags: [live, long]
---
