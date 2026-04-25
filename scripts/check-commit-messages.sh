#!/usr/bin/env bash
# check-commit-messages.sh — plan A1 Task 0.5, extended in Plan B.
#
# Every commit's message must begin with one of:
#   [Task <N>[.M[.P...]]], [DEVIATION Task <N>[.M[.P...]]], [CHORE]
# where each numeric segment is one or more digits. Sub-task depths of
# 2 ("Task 1.5"), 3 ("Task 3.5.1"), or deeper are all permitted —
# every plan since A2 has used at least two levels for its scaffolding
# (Task N.5.1 / 5.2 / 5.3 / etc.); the pattern explicitly admits them
# rather than relying on CI-level leniency.
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
# Numeric component `[0-9]+(\.[0-9]+)*` allows any depth (1.5, 3.5.1,
# 4.5.1, …) so every Plan's sub-task numbering passes.
PATTERN='^(\[Task [0-9]+(\.[0-9]+)*\]|\[DEVIATION Task [0-9]+(\.[0-9]+)*\]|\[CHORE\])'

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

Every commit's subject line must start with one of:
  [Task <N>[.M[.P...]]] short description
  [DEVIATION Task <N>[.M[.P...]]] short description
  [CHORE] short description

Examples:
  [Task 0.3] CI workflow matrix with Boehm install steps
  [Task 3.5.1] Plan A3 scaffolding
  [Task 4.5.5] Extract cross-boundary ABI constants into sigil-abi

Fix: rebase and edit the offending commit messages.
EOF
    exit 1
fi

echo "commit-message check: OK"
