#!/usr/bin/env bash
# compare.sh — thin wrapper for the Python cross-language comparison harness.
#
# For each (prompt × language × model × run), sends the prompt to
# Claude with the language-specific system context, extracts the
# program from the response, hands it to comp/scripts/eval-<lang>.sh,
# and records the result. On first-shot failure, an edit-loop turn
# feeds the failure back to the model.
#
# See comp/scripts/compare.py --help for full options.
#
# Required env: ANTHROPIC_API_KEY
# Required build: `cargo build --release` (sigil eval driver invokes target/release/sigil)
# Required tools: python3, go (1.21+) for the eval drivers

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec python3 "$SCRIPT_DIR/compare.py" "$@"
