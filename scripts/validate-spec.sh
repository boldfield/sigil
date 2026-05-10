#!/usr/bin/env bash
# validate-spec.sh — thin wrapper for the Python validation harness.
#
# Reads spec/validation-prompts.md, runs each prompt against a fresh
# Claude API session given only spec/language.md as context, compiles
# + runs the produced program, and compares stdout + exit to the
# oracle. See scripts/validate_spec.py --help for full options.
#
# Required env: ANTHROPIC_API_KEY
# Required build: `cargo build --release`

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec python3 "$SCRIPT_DIR/validate_spec.py" "$@"
