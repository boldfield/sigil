#!/usr/bin/env bash
# validate-spec.sh — Plan C task 6.5.4 stub.
#
# Spec validation harness. Reads `spec/validation-prompts.md`,
# iterates each prompt-bank entry, and (when populated in Stage 9
# Task 85) runs each prompt against a fresh Claude API session given
# only `spec/language.md` as context, then compiles + runs the
# produced program and compares stdout + exit to the oracle.
#
# **Stub status:** Stage 6.5 lands this script as a stub that reads
# the prompt bank, iterates entries, and prints "not yet implemented"
# per entry. Stage 9 Task 85 replaces the stub with the real
# Claude-API-driven validation loop. Until then, invoking this
# script lists the prompt bank and exits non-zero to prevent
# accidental "looks green" interpretation in any caller pipeline.
#
# Future surface (Stage 9):
# - --model opus|sonnet|haiku (default: iterate over opus + sonnet).
# - Per-prompt: first-compile result, first-run result, after-one-edit result.
# - Aggregate pass rates per model.
# - Per-failure: which spec section is implicated, what fix was applied.
# - Output written to `spec/validation-log.md`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROMPTS="$REPO_ROOT/spec/validation-prompts.md"

if [ ! -f "$PROMPTS" ]; then
    echo "validate-spec.sh: prompt bank not found at $PROMPTS" >&2
    exit 2
fi

echo "validate-spec.sh: stub mode (Plan C Stage 6.5 task 6.5.4)"
echo "validate-spec.sh: prompt bank at $PROMPTS"
echo

# Iterate entries. Headings follow the convention `## P\d\d — <topic>`.
count=0
while IFS= read -r line; do
    if [[ "$line" =~ ^##[[:space:]]+P[0-9]+[[:space:]]+—[[:space:]] ]]; then
        # Strip the leading "## " for display.
        printf '  %s — not yet implemented\n' "${line#\#\# }"
        count=$((count + 1))
    fi
done < "$PROMPTS"

echo
echo "validate-spec.sh: $count prompts in bank; full validation lands in Stage 9 (Task 85)"
echo "validate-spec.sh: stub exiting non-zero so callers don't mistake stub for green"
exit 1
