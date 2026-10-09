#!/bin/sh
# Errors the GCP project $GCP_PROJECT logged since the last check (runs/last; the first time, the
# last 24 hours) that were never reported before, one line per kind: count, where, message with
# numbers and ids blanked so repeats group. Prints nothing when there is nothing new.
#
# Kinds already reported are kept in runs/seen: runs/ is outside the flow's hash, so remembering
# them asks no new yes. Delete a line there to hear about that kind again. Cluster noise
# (kube-system, gke-*) is left out; LOG_FILTER in the probe's env replaces the whole filter.
set -eu
: "${GCP_PROJECT:?set GCP_PROJECT in the probe step's env}"
runs="${RELAY_FLOW_DIR:-$(dirname "$0")}/runs"
mkdir -p "$runs"
seen="$runs/seen"
last="$runs/last"
touch "$seen"
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
since=$(cat "$last" 2>/dev/null ||
  date -u -d "24 hours ago" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-24H +%Y-%m-%dT%H:%M:%SZ)
filter=${LOG_FILTER:-'severity>=ERROR AND NOT resource.labels.namespace_name=("kube-system" OR "gke-managed-system" OR "gmp-system")'}
# Read whole first: a read that fails fails the step, and runs/last stays where it was.
logs=$(gcloud logging read "($filter) AND timestamp>=\"$since\"" \
  --project "$GCP_PROJECT" --limit 2000 --format json)
printf '%s\n' "$logs" |
  jq -r '.[] | [
      ((.resource.labels.namespace_name // .resource.type // "") + "/" + (.resource.labels.container_name // .resource.labels.service_name // (.resource.labels.pod_name // "" | sub("-[a-z0-9]+-[a-z0-9]+$"; "")))),
      ((.textPayload // .jsonPayload.message // .jsonPayload.msg // (.jsonPayload | tostring) // "") | tostring | gsub("\\s+"; " ") | .[0:300])
    ] | @tsv' |
  sed -E 's/[0-9a-f]{8}-[0-9a-f-]{27}/<id>/g; s/[0-9]+/N/g' |
  sort | uniq -c | sort -rn |
  {
    # Forty kinds at most; the rest stay unseen, and come next time.
    n=0
    while IFS= read -r line; do
      [ "$n" -lt 40 ] || break
      kind=$(printf '%s\n' "$line" | sed -E 's/^ *[0-9]+ //')
      grep -qxF -- "$kind" "$seen" && continue
      printf '%s\n' "$kind" >> "$seen"
      printf '%s\n' "$line"
      n=$((n + 1))
    done
  }
printf '%s\n' "$now" > "$last"
