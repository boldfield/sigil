#!/usr/bin/env bash
#
# scripts/sync-docs.sh — copy the canonical files into `docs/` for
# GitHub Pages publication.
#
# `docs/` is published to https://sigillang.ai/ via GitHub Pages.
# Most published pages have a Jekyll front matter header that
# `sync_pair` preserves before appending the rewritten canonical
# content; the LLM-ready raw spec at `docs/language.raw.md` has NO
# front matter so Jekyll passes it through verbatim (Jekyll's
# "files without front matter are static files" rule).
#
# Pairs:
#   spec/language.md      <-->  docs/language.md       (front-matter wrap)
#   spec/language.md      <-->  docs/language.raw.md   (no front matter; LLM-ready)
#   CAPABILITIES.md       <-->  docs/capabilities.md
#   SIGIL_FOR_LLMS.md     <-->  docs/for-llms.md
#
# Usage: ./scripts/sync-docs.sh
# Exit 0 if everything is already in sync OR after a successful copy.
# Exit non-zero on I/O failure.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Pipe the canonical content through link-rewrites so the published
# copy uses the site's URL hierarchy. Canonical files live in the
# repo root / `spec/` and use a mix of `./X.md` and `../X` relative
# links. On the published site:
# - `./CAPABILITIES.md`, `./SIGIL_FOR_LLMS.md`, `./spec/language.md`
#   are published as `/capabilities/`, `/for-llms/`, `/language/`.
# - `../std/`, `../examples/`, `../compiler/`, etc. (used by
#   `spec/language.md` to reference repo-root paths) are NOT
#   published; rewrite to absolute GitHub blob URLs.
# - `validation-prompts.md` (sibling of language.md in spec/) is
#   also not published; rewrite to GitHub blob URL.
#
# The `../` rewrite is blanket-safe because every `../` in a
# canonical doc lives one level deep (in `spec/`) and means
# "from repo root". A future `spec/language.md` add of `../`
# links to anywhere in the tree will Just Work.
rewrite_links() {
    local canonical="$1"
    sed \
        -e 's|\](\./CAPABILITIES\.md)|](/capabilities/)|g' \
        -e 's|\](\./SIGIL_FOR_LLMS\.md)|](/for-llms/)|g' \
        -e 's|\](\./spec/language\.md)|](/language/)|g' \
        -e 's|\](validation-prompts\.md)|](https://github.com/boldfield/sigil/blob/main/spec/validation-prompts.md)|g' \
        -e 's|\](\.\./|](https://github.com/boldfield/sigil/blob/main/|g' \
        "$canonical"
}

# Front-matter-preserving copy. Prepends two preserved-from-target
# regions to the rewritten canonical content:
#
#   1. The front-matter block (`---...---` at top of file).
#   2. An optional Pages-specific preamble between the front matter
#      and the sentinel `<!-- BEGIN SYNCED CONTENT -->`. The
#      preamble survives re-syncs — use it for callouts that are
#      site-specific (e.g., "here's the raw markdown") and shouldn't
#      pollute the canonical source.
#
# If the sentinel is absent, only the front matter is preserved
# (backward-compatible with pre-sentinel `docs/*.md` files).
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

    # Extract preserved prefix: front matter + (optional) preamble
    # up to and including the `<!-- BEGIN SYNCED CONTENT -->` sentinel.
    # If the sentinel doesn't exist, we extract just the front matter.
    local prefix
    prefix="$(awk '
        BEGIN { in_fm = 0; fm_done = 0; saw_sentinel = 0 }
        NR == 1 && $0 == "---" { in_fm = 1; print; next }
        in_fm == 1 && $0 == "---" { in_fm = 0; fm_done = 1; print; next }
        in_fm == 1 { print; next }
        fm_done == 1 && /<!-- BEGIN SYNCED CONTENT -->/ { print; saw_sentinel = 1; exit }
        fm_done == 1 { print; next }
        END { if (!saw_sentinel) exit 0 }
    ' "$published")"

    if [ -z "$prefix" ]; then
        echo "sync-docs: $published has no front matter; aborting" >&2
        return 1
    fi

    # If no sentinel was found, awk emitted only the front matter
    # plus whatever followed (probably the previous canonical content,
    # which we want to drop). Re-extract just the front matter for
    # the safe-default behaviour.
    if ! grep -q '<!-- BEGIN SYNCED CONTENT -->' "$published"; then
        prefix="$(awk '
            BEGIN { in_fm = 0 }
            NR == 1 && $0 == "---" { in_fm = 1; print; next }
            in_fm == 1 && $0 == "---" { print; exit }
            in_fm == 1 { print; next }
        ' "$published")"
    fi

    local tmp
    tmp="$(mktemp)"
    {
        printf '%s\n\n' "$prefix"
        rewrite_links "$canonical"
    } > "$tmp"
    mv "$tmp" "$published"
    echo "sync-docs: $canonical -> $published"
}

# Verbatim copy: NO front matter prepended. Used for the LLM-ready
# raw spec endpoint at `docs/language.raw.md`. Jekyll treats files
# without front matter as static and passes them through verbatim,
# so the URL `https://sigillang.ai/language.raw.md` serves the raw
# markdown body with the same link rewrites the front-matter-wrapped
# `/language/` endpoint applies (so LLM consumers see actionable
# GitHub-blob URLs for `../std/` references instead of broken
# relative paths).
sync_raw() {
    local canonical="$1"
    local published="$2"

    if [ ! -f "$canonical" ]; then
        echo "sync-docs: missing canonical source: $canonical" >&2
        return 1
    fi

    local tmp
    tmp="$(mktemp)"
    rewrite_links "$canonical" > "$tmp"
    mv "$tmp" "$published"
    echo "sync-docs: $canonical -> $published (raw)"
}

sync_pair "spec/language.md"     "docs/language.md"
sync_pair "CAPABILITIES.md"      "docs/capabilities.md"
sync_pair "SIGIL_FOR_LLMS.md"    "docs/for-llms.md"
sync_raw  "spec/language.md"     "docs/language.raw.md"

echo "sync-docs: OK"
