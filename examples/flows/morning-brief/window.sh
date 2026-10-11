# The days a brief covers, as dates: from $since to $until, both whole days. Yesterday; on a
# Monday, Friday through Sunday. Sourced by the other scripts.
day() { date -d "$1 days ago" +%F 2>/dev/null || date -v-"$1"d +%F; }
if [ "$(date +%u)" = 1 ]; then since=$(day 3); else since=$(day 1); fi
until=$(day 1)
today=$(date +%F)
