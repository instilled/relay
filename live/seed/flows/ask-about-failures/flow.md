---
in: "@LIVE@/ws"
on: run.failed
where: { tag: live }
steps:
  review:
    ask: "{event.title} failed: {event.summary}"
    options:
      again: { run: "echo again >> retried.log" }
      ignore: {}
    default: ignore
    expires: 2m
---
