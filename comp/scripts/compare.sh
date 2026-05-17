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
# Required: `claude` binary on PATH and authenticated (either via
#           `claude /login` or `claude setup-token` + export
#           CLAUDE_CODE_OAUTH_TOKEN). Subscription-billed; no API key.
# Required build: `cargo build --release` (sigil eval driver invokes target/release/sigil)
# Required tools: python3, go (1.21+), rustc for the eval drivers

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec python3 "$SCRIPT_DIR/compare.py" "$@"
