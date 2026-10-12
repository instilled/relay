#!/bin/sh
# Posts what brief wrote to the Google Chat space $BRIEF_SPACE, as the account gws is signed in
# to. It prints nothing, so what the firing came to is the brief itself.
set -eu
: "${BRIEF_SPACE:?set BRIEF_SPACE in the env of the post step}"
text=$(cat "$RELAY_IN/brief.out")
[ -n "$text" ] || { echo "brief wrote nothing to post" >&2; exit 1; }
gws chat +send --space "$BRIEF_SPACE" --text "$text" > /dev/null
