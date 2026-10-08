---
in: "@LIVE@/ws"
when: every 5m
steps:
  standup:
    run: { agent: claude }
    tags: [live, agent]
---

## standup
Say what changed in this folder since yesterday, in three bullets or fewer.
If nothing changed, say so in one line.
