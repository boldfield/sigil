#!/usr/bin/env bash
#
# scripts/check-docs-sync.sh — CI gate. Runs `sync-docs.sh`, then
# fails if it produced any uncommitted changes. The pair:
#
#   scripts/sync-docs.sh         — re-copy canonical → docs/ pages
#   scripts/check-docs-sync.sh   — gate that the result is committed
#
# CI runs this; contributors who edit `spec/language.md` (or the
# other canonical sources) get a one-line failure pointing them at
# the fix.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Snapshot the current state of the published files. If sync-docs
# rewrites them, the snapshots will diverge.
before_lang="$(sha256sum docs/language.md | awk '{print $1}')"
before_cap="$(sha256sum docs/capabilities.md | awk '{print $1}')"
before_llm="$(sha256sum docs/for-llms.md | awk '{print $1}')"

./scripts/sync-docs.sh > /dev/null

after_lang="$(sha256sum docs/language.md | awk '{print $1}')"
after_cap="$(sha256sum docs/capabilities.md | awk '{print $1}')"
after_llm="$(sha256sum docs/for-llms.md | awk '{print $1}')"

drift=0
[ "$before_lang" != "$after_lang" ] && { echo "check-docs-sync: docs/language.md drifted from spec/language.md"; drift=1; }
[ "$before_cap" != "$after_cap" ] && { echo "check-docs-sync: docs/capabilities.md drifted from CAPABILITIES.md"; drift=1; }
[ "$before_llm" != "$after_llm" ] && { echo "check-docs-sync: docs/for-llms.md drifted from SIGIL_FOR_LLMS.md"; drift=1; }

if [ "$drift" -ne 0 ]; then
    echo "check-docs-sync: run \`./scripts/sync-docs.sh\` and commit the updated docs/*.md files" >&2
    exit 1
fi

echo "check-docs-sync: OK"
