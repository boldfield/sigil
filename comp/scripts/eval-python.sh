#!/usr/bin/env bash
# eval-python.sh — run + diff oracle for one Python program.
#
# Usage: eval-python.sh <program.py> <prompt-id>
#
# Same shape as eval-sigil.sh. Python doesn't have a separate compile
# step (syntax errors surface at run time); we run via `python3 -B`
# (no pyc) and capture stdout + exit. A SyntaxError or other
# Python-level error before any user output counts as a "compile"
# failure for category-comparable accounting.
#
# Output:
#   pass                              — stdout + exit match oracle
#   fail: <category> — <details>      — anything else
#
# Categories:
#   compile  — SyntaxError / ImportError / NameError before any user output
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
    echo "usage: $0 <program.py> <prompt-id>" >&2
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
    in_block && /^## [A-Z][0-9]/ && $0 !~ "^"id { exit }
    in_block && /^\*\*Oracle \(stdout\):\*\*/ { in_oracle=1; next }
    in_oracle && /^```$/ {
        if (started) { exit } else { started=1; next }
    }
    in_oracle && started { print }
' "$PROMPTS")"

expected_exit="$(awk -v id="## $PROMPT_ID " '
    $0 ~ "^"id { in_block=1 }
    in_block && /^## [A-Z][0-9]/ && $0 !~ "^"id { exit }
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

PY="${PYTHON:-python3}"
if ! command -v "$PY" >/dev/null 2>&1; then
    echo "fail: harness — $PY not on PATH" >&2
    exit 1
fi

# Run. Capture stdout + stderr separately. SyntaxError / NameError
# show up on stderr with exit != 0 before any program output.
stdout_file="$(mktemp)"
stderr_file="$(mktemp)"
trap 'rm -f "$stdout_file" "$stderr_file" 2>/dev/null || true' EXIT

actual_exit=0
$TIMEOUT_CMD "$PY" -B "$PROGRAM" >"$stdout_file" 2>"$stderr_file" || actual_exit=$?

if [ "$actual_exit" -eq 124 ] || [ "$actual_exit" -eq 137 ]; then
    echo "fail: timeout — program exceeded ${TIMEOUT}s"
    exit 1
fi

# Categorize: SyntaxError + nothing on stdout = compile failure.
if [ "$actual_exit" != "0" ] && [ ! -s "$stdout_file" ]; then
    err_first="$(head -c 200 "$stderr_file")"
    if echo "$err_first" | grep -qE 'SyntaxError|IndentationError|ImportError|ModuleNotFoundError'; then
        echo "fail: compile — $err_first" | head -c 400
        echo
        exit 1
    fi
fi

actual_stdout="$(cat "$stdout_file")"

if [ "$actual_exit" != "$expected_exit" ]; then
    echo "fail: runtime — exit $actual_exit (expected $expected_exit); stderr: $(head -c 200 "$stderr_file")"
    exit 1
fi

if [ "$actual_stdout" != "$expected_stdout" ]; then
    echo "fail: stdout — output differs from oracle"
    diff <(echo "$expected_stdout") <(echo "$actual_stdout") | head -20 >&2
    exit 1
fi

echo "pass"
exit 0
