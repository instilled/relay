---
in: "@LIVE@/ws"
when: every 45s
steps:
  look:
    run: 'echo "tick in $(pwd)"; ls'
    tags: [live]
---
