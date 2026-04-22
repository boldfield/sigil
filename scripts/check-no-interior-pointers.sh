#!/usr/bin/env bash
# check-no-interior-pointers.sh — plan A1 Task 0.12
#
# Heuristic grep for patterns that commonly produce interior pointers into
# GC-managed heap objects. Any match without a same-or-previous-line
# "SAFETY: not an interior pointer" escape-hatch comment fails the script.
#
# The check covers runtime/src/ only; compiler/src/ does not directly
# manipulate heap-object pointers. Codegen-emitted Cranelift IR is reviewed
# structurally (no pointer-into-struct computations) per the plan, not by
# this script.
#
# This is intentionally conservative: false positives land on the safety
# escape hatch; the value is preventing new offenders from slipping in.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$REPO_ROOT/runtime/src"

if [[ ! -d "$TARGET" ]]; then
    echo "check-no-interior-pointers: $TARGET does not exist" >&2
    exit 1
fi

# Patterns that typically produce a pointer into a heap object.
PATTERNS=(
    '\.as_mut_ptr\('
    '\.as_ptr\('
    '\.offset\('
    'ptr\.add\('
    'ptr\.sub\('
)

failed=0

while IFS= read -r -d '' file; do
    line_no=0
    prev_line=""
    while IFS= read -r line; do
        line_no=$((line_no + 1))
        for pat in "${PATTERNS[@]}"; do
            if echo "$line" | grep -Eq "$pat"; then
                # Is there an acknowledging SAFETY comment on this or the previous line?
                if echo "$line" | grep -q "SAFETY: not an interior pointer"; then
                    continue
                fi
                if echo "$prev_line" | grep -q "SAFETY: not an interior pointer"; then
                    continue
                fi
                echo "INTERIOR-POINTER RISK: $file:$line_no matches '$pat'" >&2
                echo "  $line" >&2
                echo "  (add '// SAFETY: not an interior pointer (<reason>)' on the same or preceding line to acknowledge.)" >&2
                failed=1
            fi
        done
        prev_line="$line"
    done < "$file"
done < <(find "$TARGET" -name '*.rs' -print0)

if [[ $failed -ne 0 ]]; then
    echo "check-no-interior-pointers: FAIL" >&2
    exit 1
fi

echo "check-no-interior-pointers: OK ($TARGET)"
