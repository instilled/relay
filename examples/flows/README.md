# Example flows to copy

Two working flows, to copy into `~/.relay/flows/`, edit and approve. Relay does not install
them: each is a folder, a `flow.md` and its own scripts, with placeholders (`my-gcp-project`,
`spaces/AAAA`, `my-org`, `~/code/my-service`) where your values go. The lines to change are
marked `# change`.

## prod-logs

Every 30 minutes, the errors a GCP project logged that were never reported before. When there
is one, an agent proposes one action; you say yes (an agent does it in your checkout) or ignore
it. Unanswered in 4h counts as ignore.

Needs `gcloud` (signed in, with read access to the project's logs), `jq` and `claude`. Its `in:`
is a folder Relay knows: `rl add` it first.

## morning-brief

Weekday mornings at 7:30, what each teammate worked on yesterday (Friday through Sunday on a
Monday), from Google Chat, Notion and GitHub. An agent writes the brief, `post.sh` sends it to a
Google Chat space, and it heads your catch-up.

Needs `gws` (Google Chat), `ntn` (Notion), `gh` (GitHub), `jq`, `python3` and `claude`, each
signed in as you: the scripts read what those accounts can see.

## Use one

```sh
cp -r examples/flows/prod-logs ~/.relay/flows/logs
$EDITOR ~/.relay/flows/logs/flow.md      # set the project and folder
rl add ~/code/my-service                 # the folder its `in:` names, if Relay does not know it
rl flow test logs                        # reads the folder again; what a firing would do, starting nothing
rl flow approve logs                     # it fires from now on
```

`rl flow run logs` fires it now instead of waiting. Editing a file in the folder by hand asks
for the yes again.
