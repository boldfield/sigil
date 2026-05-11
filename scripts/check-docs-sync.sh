#!/usr/bin/env bash
#
# scripts/check-docs-sync.sh — local drift check. Runs `sync-docs.sh`,
# then reports if it produced any uncommitted changes. The pair:
#
#   scripts/sync-docs.sh         — re-copy canonical → docs/ pages
#   scripts/check-docs-sync.sh   — report whether docs/ is up to date
#
# Docs sync is no longer a per-commit CI gate; the release workflow
# runs sync-docs on every `v*` tag push and commits the result back
# to main. This script remains for contributors who want to check
# drift locally before cutting a tag.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# `shasum -a 256` works on both Linux (perl-backed) and macOS (native);
# `sha256sum` is Linux-only and trips the script for contributors
# running locally on darwin.
hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }

# Snapshot the current state of the published files. If sync-docs
# rewrites them, the snapshots will diverge.
before_lang="$(hash_file docs/language.md)"
before_cap="$(hash_file docs/capabilities.md)"
before_llm="$(hash_file docs/for-llms.md)"
before_raw="$(hash_file docs/language.raw.md)"

./scripts/sync-docs.sh > /dev/null

after_lang="$(hash_file docs/language.md)"
after_cap="$(hash_file docs/capabilities.md)"
after_llm="$(hash_file docs/for-llms.md)"
after_raw="$(hash_file docs/language.raw.md)"

drift=0
[ "$before_lang" != "$after_lang" ] && { echo "check-docs-sync: docs/language.md drifted from spec/language.md"; drift=1; }
[ "$before_cap" != "$after_cap" ] && { echo "check-docs-sync: docs/capabilities.md drifted from CAPABILITIES.md"; drift=1; }
[ "$before_llm" != "$after_llm" ] && { echo "check-docs-sync: docs/for-llms.md drifted from SIGIL_FOR_LLMS.md"; drift=1; }
[ "$before_raw" != "$after_raw" ] && { echo "check-docs-sync: docs/language.raw.md drifted from spec/language.md"; drift=1; }

if [ "$drift" -ne 0 ]; then
    echo "check-docs-sync: run \`./scripts/sync-docs.sh\` and commit the updated docs/*.md files" >&2
    exit 1
fi

echo "check-docs-sync: OK"
