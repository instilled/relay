#!/bin/sh
# The days the brief covers, as a person reads them: "Wed 7 Oct", or "Fri 2 Oct – Sun 4 Oct".
set -eu
. "${RELAY_FLOW_DIR:-$(dirname "$0")}/window.sh"
show() { date -d "$1" '+%a %-d %b' 2>/dev/null || date -j -f %F "$1" '+%a %-d %b'; }
if [ "$since" = "$until" ]; then show "$since"; else echo "$(show "$since") – $(show "$until")"; fi
