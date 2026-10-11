---
in: ~/code/my-service  # change: the checkout the agents read and change (a folder `rl add` registered)
when: every 30m
steps:
  probe:
    run: new-log-errors.sh
    env: { GCP_PROJECT: my-gcp-project }  # change: your GCP project, and in the prompts below
  triage:
    after: probe
    if: probe.out
    run: { agent: claude, args: [--allowedTools, "Bash(gcloud logging read:*)"] }
  review:
    after: triage
    ask: "{triage.out}"
    options:
      apply:  { run: { agent: claude, args: [--permission-mode, acceptEdits] } }
      ignore: {}
    default: ignore
    expires: 4h
---

Every 30 minutes, the errors the GCP project my-gcp-project logged since the last look that were
never reported before. When there is one, an agent proposes one action; you say yes (it is done
in this checkout) or ignore it. Unanswered in 4h counts as ignore. new-log-errors.sh keeps the
kinds it has reported in runs/seen: delete a line there to hear about that kind again.

## triage
These are errors the GCP project my-gcp-project logged since the last check that were never reported
before, one line per kind: count, where, message with numbers blanked.

{probe.out}

Look into what matters, in this repository and with `gcloud logging read --project my-gcp-project`
if you need more of a log. Do not change anything. Then propose exactly one action, in two lines
at most: what is wrong and what to do about it. If nothing needs doing, say so in one line.

## review.apply
Do this, in this repository: {triage.out}

What was said with the yes: {review.note}

Keep the change small, run the relevant tests, and do not push or deploy.
