#!/usr/bin/env bash
# plan-b-invariants.sh — Plan B Task 4.5.4 scaffolding.
#
# Runs in CI as a dedicated step that exercises the three correctness
# invariants Plan B is chartered to defend:
#
#   1. Deep-recursion trampoline (Stage 6 / Task 55-56-60).
#      Compile a Sigil program that recurses 1,000,000 times; the
#      trampoline for CPS-color code must prevent C-stack overflow.
#
#   2. Multi-shot continuation (Stage 6 / Task 58).
#      A handler captures a continuation and resumes it ≥10 times
#      with different inputs; the results must be independent.
#
#   3. Selective CPS (Stage 5-6 / Task 50 + Task 60 performance floor).
#      A program in which native-color fib(30) runs fast and a CPS-
#      color fib(30) (forced to CPS via a gratuitous effect) also
#      completes; verifies both emission paths work end-to-end.
#
# Stage 4.5 wires this script as a CI step before any of the
# invariants can actually pass — the features each invariant depends
# on land in Stage 5/6. Each invariant drives itself off a specific
# Sigil example under examples/ and a specific e2e test name in
# `compiler/tests/e2e.rs`. Absent either, the invariant reports SKIP
# and the script exits 0. Once the example and test land, the check
# flips from SKIP to requiring the test to pass.
#
# CI treats this script as an early signal that Plan B's charter
# invariants are exercised, not only as a gate. The `cargo test
# --workspace` step later in the CI run is the authoritative pass/
# fail for everything; this script's job is to surface the three
# invariants by name so their CI presence is impossible to drop by
# mistake.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Defensive PATH extension. rustup installs cargo under $HOME/.cargo/bin.
if [ -d "${HOME:-}/.cargo/bin" ]; then
    case ":$PATH:" in
        *":$HOME/.cargo/bin:"*) ;;
        *) export PATH="$HOME/.cargo/bin:$PATH" ;;
    esac
fi

say() { printf '\n>>> plan-b-invariants: %s\n' "$*"; }
skip() { printf '    [SKIP] %s\n' "$*"; }
pass() { printf '    [PASS] %s\n' "$*"; }

# Run a named e2e test for a given invariant. The example file must
# exist under examples/ for the invariant to be considered "ready";
# this prevents a stale test name from masking a missing Sigil program.
run_invariant() {
    local label="$1"
    local example_path="$2"
    local test_name="$3"

    if [ ! -f "$example_path" ]; then
        skip "$label — $example_path absent (Stage 5/6 work)"
        return 0
    fi

    say "$label — running cargo test $test_name"
    cargo test -p sigil-compiler --test e2e "$test_name" -- --exact --nocapture
    pass "$label"
}

say "Plan B invariant 1: deep recursion trampoline"
run_invariant \
    "deep recursion trampoline (1M-depth recursion must not overflow)" \
    "examples/deep_recursion.sigil" \
    "plan_b_invariant_deep_recursion"

say "Plan B invariant 2: multi-shot continuation"
run_invariant \
    "multi-shot continuation (resume >=10 times with independent results)" \
    "examples/multishot_stress.sigil" \
    "plan_b_invariant_multishot"

say "Plan B invariant 3: selective CPS correctness"
run_invariant \
    "selective CPS (native fib(30) <100ms; forced-CPS fib(30) completes)" \
    "examples/fib_cps_forced.sigil" \
    "plan_b_invariant_selective_cps"

say "GC cross-check: deep recursion path (10k-element JSON array parsing)"
cargo test -p sigil-compiler --test e2e "cross_check_json_parse_large_array_no_abort" -- --exact --nocapture
pass "GC cross-check: deep recursion path (JSON parser under SIGIL_GC_CROSS_CHECK=1)"

say "OK — plan-b-invariants completed (SKIPs will convert to PASS as Stage 5/6 lands)"
