#!/bin/sh
# Pull requests in the GitHub organisation $GITHUB_ORG created or updated in the brief's days, by
# author: author, repo, state, title, url. The login gh is signed in as is "(me)".
set -eu
: "${GITHUB_ORG:?set GITHUB_ORG in the github step's env}"
. "${RELAY_FLOW_DIR:-$(dirname "$0")}/window.sh"
me=$(gh api user --jq .login)
gh search prs --owner "$GITHUB_ORG" --updated "$since..$until" --limit 200 \
  --json title,author,repository,state,url \
  --jq 'sort_by(.author.login)[] | "\(if .author.login == "'"$me"'" then "(me)" else .author.login end)\t\(.repository.name)\t\(.state)\t\(.title)\t\(.url)"'
