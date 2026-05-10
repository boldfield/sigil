#!/usr/bin/env bash
# eval-go.sh — compile + run + diff oracle for one Go program.
#
# Usage: eval-go.sh <program.go> <prompt-id>
#
# Same shape as eval-sigil.sh. Go has a real compile step via
# `go build`; failures there are categorized as "compile". Runtime
# errors (panics, nil derefs) are categorized as "runtime".
#
# Output:
#   pass                              — stdout + exit match oracle
#   fail: <category> — <details>      — anything else
#
# Categories:
#   compile  — go build failed
#   runtime  — non-zero exit during execution
#   stdout   — exit matched but stdout differed
#   timeout  — exceeded TIMEOUT seconds

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROMPTS="$REPO_ROOT/comp/prompts.md"
TIMEOUT="${TIMEOUT:-30}"

# Portable timeout. See eval-sigil.sh for the rationale.
if command -v timeout >/dev/null 2>&1; then
    TIMEOUT_CMD="timeout $TIMEOUT"
elif command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_CMD="gtimeout $TIMEOUT"
else
    TIMEOUT_CMD=""
fi

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <program.go> <prompt-id>" >&2
    exit 2
fi

PROGRAM="$1"
PROMPT_ID="$2"

if [ ! -f "$PROGRAM" ]; then
    echo "fail: input — program file not found: $PROGRAM" >&2
    exit 1
fi

# Parse oracle from prompts.md (same shape as eval-sigil.sh).
expected_stdout="$(awk -v id="## $PROMPT_ID " '
    $0 ~ "^"id { in_block=1 }
    in_block && /^## C[0-9]/ && $0 !~ "^"id { exit }
    in_block && /^\*\*Oracle \(stdout\):\*\*/ { in_oracle=1; next }
    in_oracle && /^```$/ {
        if (started) { exit } else { started=1; next }
    }
    in_oracle && started { print }
' "$PROMPTS")"

expected_exit="$(awk -v id="## $PROMPT_ID " '
    $0 ~ "^"id { in_block=1 }
    in_block && /^## C[0-9]/ && $0 !~ "^"id { exit }
    in_block && /^\*\*Oracle \(exit\):\*\*/ {
        match($0, /`[0-9]+`/)
        if (RSTART > 0) {
            print substr($0, RSTART+1, RLENGTH-2)
            exit
        }
    }
' "$PROMPTS")"

if [ -z "$expected_exit" ]; then
    echo "fail: harness — could not parse oracle for $PROMPT_ID from $PROMPTS" >&2
    exit 1
fi

GO="${GO:-go}"
if ! command -v "$GO" >/dev/null 2>&1; then
    echo "fail: harness — $GO not on PATH" >&2
    exit 1
fi

# Build. Go requires the source file to be in a package; for
# package main with a main() it can be built directly via
# `go build -o <out> <file.go>` without a module file as long as
# imports are stdlib-only.
OUTBIN="$(mktemp)"
build_err_file="$(mktemp)"
trap 'rm -f "$OUTBIN" "$build_err_file" 2>/dev/null || true' EXIT

if ! "$GO" build -o "$OUTBIN" "$PROGRAM" 2>"$build_err_file"; then
    echo "fail: compile — $(head -c 400 "$build_err_file")"
    exit 1
fi

# Run with timeout.
actual_stdout="$($TIMEOUT_CMD "$OUTBIN" 2>/dev/null)" && actual_exit=$? || actual_exit=$?
if [ "$actual_exit" -eq 124 ] || [ "$actual_exit" -eq 137 ]; then
    echo "fail: timeout — program exceeded ${TIMEOUT}s"
    exit 1
fi

if [ "$actual_exit" != "$expected_exit" ]; then
    echo "fail: runtime — exit $actual_exit (expected $expected_exit)"
    exit 1
fi

if [ "$actual_stdout" != "$expected_stdout" ]; then
    echo "fail: stdout — output differs from oracle"
    diff <(echo "$expected_stdout") <(echo "$actual_stdout") | head -20 >&2
    exit 1
fi

echo "pass"
exit 0
