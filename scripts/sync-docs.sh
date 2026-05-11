#!/usr/bin/env bash
#
# scripts/sync-docs.sh — copy the canonical files into `docs/` for
# GitHub Pages publication, preserving each page's Jekyll front matter.
#
# `docs/` is published to https://sigillang.ai/ via GitHub Pages.
# The Jekyll front matter on each docs/*.md page is a few lines at the
# top; the rest of the file mirrors the canonical source in the repo.
# Re-run this whenever the canonical content changes.
#
# Pairs:
#   spec/language.md       <-->  docs/language.md
#   CAPABILITIES.md        <-->  docs/capabilities.md
#   SIGIL_FOR_LLMS.md      <-->  docs/for-llms.md
#
# Usage: ./scripts/sync-docs.sh
# Exit 0 if everything is already in sync OR after a successful copy.
# Exit non-zero on I/O failure.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# A single front-matter-aware copy: prepends the front-matter block
# captured from the existing docs/ page (if any) to the canonical
# content, then writes to the destination atomically.
sync_pair() {
    local canonical="$1"
    local published="$2"

    if [ ! -f "$canonical" ]; then
        echo "sync-docs: missing canonical source: $canonical" >&2
        return 1
    fi
    if [ ! -f "$published" ]; then
        echo "sync-docs: missing published target (need to be created by hand once): $published" >&2
        return 1
    fi

    # Extract the front-matter block: the first contiguous run from a
    # leading `---` through the closing `---`. The published file is
    # expected to start with `---`.
    local front
    front="$(awk '
        BEGIN { in_fm = 0 }
        NR == 1 && $0 == "---" { in_fm = 1; print; next }
        in_fm == 1 && $0 == "---" { print; exit }
        in_fm == 1 { print; next }
    ' "$published")"

    if [ -z "$front" ]; then
        echo "sync-docs: $published has no front matter; aborting" >&2
        return 1
    fi

    local tmp
    tmp="$(mktemp)"
    {
        printf '%s\n\n' "$front"
        cat "$canonical"
    } > "$tmp"
    mv "$tmp" "$published"
    echo "sync-docs: $canonical -> $published"
}

sync_pair "spec/language.md"     "docs/language.md"
sync_pair "CAPABILITIES.md"      "docs/capabilities.md"
sync_pair "SIGIL_FOR_LLMS.md"    "docs/for-llms.md"

echo "sync-docs: OK"
