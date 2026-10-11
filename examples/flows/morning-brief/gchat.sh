#!/bin/sh
# Google Chat messages in the brief's days (window.sh), in every space and DM active since then:
# space, sender, text. The space the brief is posted to ($BRIEF_SPACE) is left out. Read as the
# account gws is signed in to; its own messages are marked "(me)".
set -eu
me=$(gws auth status 2>/dev/null | jq -r '.user // empty')
[ -n "$me" ] || { echo "gws is not signed in: gws auth login" >&2; exit 1; }
. "${RELAY_FLOW_DIR:-$(dirname "$0")}/window.sh"
from="${since}T00:00:00Z"
gws chat spaces list --params '{"pageSize": 1000}' --page-all 2>/dev/null |
  jq -r --arg since "$from" --arg brief "${BRIEF_SPACE:-}" '.spaces[]? | select((.lastActiveTime // "") >= $since and .name != $brief)
    | [.name, (.displayName // "DM")] | @tsv' |
  while IFS="$(printf '\t')" read -r space title; do
    filter="createTime > \"$from\" AND createTime < \"${today}T00:00:00Z\""
    gws chat spaces messages list --page-all \
      --params "$(jq -nc --arg p "$space" --arg f "$filter" '{parent: $p, filter: $f, pageSize: 1000}')" 2>/dev/null |
      jq -r --arg title "$title" --arg me "$me" '.messages[]? | select(.sender.type == "HUMAN")
        | [$title, (if .sender.email == $me then "(me)" else (.sender.displayName // .sender.email // "?") end), ((.text // "") | gsub("\\s+"; " ") | .[0:400])]
        | @tsv'
  done
