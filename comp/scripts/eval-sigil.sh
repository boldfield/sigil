#!/usr/bin/env bash
# eval-sigil.sh — compile + run + diff oracle for one Sigil program.
#
# Usage: eval-sigil.sh <program.sigil> <prompt-id>
#
# Looks up the oracle for <prompt-id> in comp/prompts.md, compiles
# the Sigil source, runs the resulting binary, captures stdout +
# exit code, and compares against the oracle.
#
# Output:
#   pass                              — stdout + exit match oracle
#   fail: <category> — <details>      — anything else
#
# Failure categories:
#   compile  — compiler or linker error
#   runtime  — non-zero exit when oracle expected zero (or vice versa)
#   stdout   — exit matched but stdout differed
#   timeout  — program exceeded TIMEOUT seconds
#
# Exits 0 on pass, 1 on fail. Stderr carries diagnostics.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROMPTS="$REPO_ROOT/comp/prompts.md"
TIMEOUT="${TIMEOUT:-30}"

# Portable timeout: GNU coreutils' `timeout` (linux), `gtimeout`
# (macOS via `brew install coreutils`), or fall back to no enforcement
# (programs that hang will block the harness — acceptable for the
# rough sketch).
if command -v timeout >/dev/null 2>&1; then
    TIMEOUT_CMD="timeout $TIMEOUT"
elif command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_CMD="gtimeout $TIMEOUT"
else
    TIMEOUT_CMD=""
    echo "eval-sigil.sh: warning — neither 'timeout' nor 'gtimeout' on PATH; running without timeout enforcement" >&2
fi

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <program.sigil> <prompt-id>" >&2
    exit 2
fi

PROGRAM="$1"
PROMPT_ID="$2"

if [ ! -f "$PROGRAM" ]; then
    echo "fail: input — program file not found: $PROGRAM" >&2
    exit 1
fi

# Extract oracle stdout + exit from prompts.md for this prompt id.
# The fenced code block under "**Oracle (stdout):**" is the byte-exact
# expected output (trailing newline preserved). The line beginning
# with "**Oracle (exit):**" carries the expected exit code in
# backticks.
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

# Compile.
SIGIL_BIN="$REPO_ROOT/target/release/sigil"
if [ ! -x "$SIGIL_BIN" ]; then
    SIGIL_BIN="$REPO_ROOT/target/debug/sigil"
fi
if [ ! -x "$SIGIL_BIN" ]; then
    echo "fail: harness — no sigil binary at $REPO_ROOT/target/{release,debug}/sigil; run cargo build first" >&2
    exit 1
fi

OUTBIN="$(mktemp)"
trap 'rm -f "$OUTBIN" "$OUTBIN.o" 2>/dev/null || true' EXIT

if ! compile_err="$("$SIGIL_BIN" "$PROGRAM" -o "$OUTBIN" --human-errors 2>&1)"; then
    echo "fail: compile — $compile_err" | head -c 400
    echo
    exit 1
fi

# Run with timeout.
actual_stdout="$($TIMEOUT_CMD "$OUTBIN" 2>/dev/null)" && actual_exit=$? || actual_exit=$?
if [ "$actual_exit" -eq 124 ] || [ "$actual_exit" -eq 137 ]; then
    echo "fail: timeout — program exceeded ${TIMEOUT}s"
    exit 1
fi

# Compare.
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
