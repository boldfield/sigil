#!/usr/bin/env bash
# check-commit-messages.sh — plan A1 Task 0.5
#
# Every commit's message must begin with one of:
#   [Task <N>], [Task 0.<M>], [DEVIATION Task <N>], [CHORE]
# where <N>/<M> are integers.
#
# Invoked by CI on pull_request events with two arguments: base SHA and
# head SHA. Walks the commit range (exclusive base, inclusive head) and
# fails if any subject line lacks the prefix.

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <base-sha> <head-sha>" >&2
    exit 2
fi

BASE="$1"
HEAD="$2"

# Regex: match the required prefixes. ^ anchors to start of subject.
PATTERN='^(\[Task [0-9]+(\.[0-9]+)?\]|\[Task 0\.[0-9]+\]|\[DEVIATION Task [0-9]+\]|\[CHORE\])'

failed=0
while read -r sha; do
    subject="$(git log -1 --format=%s "$sha")"
    if ! echo "$subject" | grep -Eq "$PATTERN"; then
        echo "BAD commit message ($sha): $subject" >&2
        failed=1
    fi
done < <(git rev-list --reverse "$BASE".."$HEAD")

if [[ $failed -ne 0 ]]; then
    cat >&2 <<'EOF'

Every commit on a plan-A1 branch must start with one of:
  [Task <N>] short description
  [Task 0.<M>] short description
  [DEVIATION Task <N>] short description
  [CHORE] short description

Example: [Task 0.3] CI workflow matrix with Boehm install steps

Fix: rebase and edit the offending commit messages.
EOF
    exit 1
fi

echo "commit-message check: OK"
