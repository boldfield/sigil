#!/usr/bin/env bash
# pod-verify.sh — plan A2 task 1.5.4.
#
# Memory-safe local verification for memory-constrained hosts (the
# headless Talos pod is the motivating case; any ~8-12 GiB box qualifies).
# Runs only the subset of cargo commands that do not blow the pod's peak
# memory ceiling. CI is authoritative for the full test suite.
#
# See the plan's "Local verification strategy" section for the split.
# CI also invokes this script (as an additional step, not a replacement)
# so the pod-safe path is itself continuously exercised on both hosts.
#
# Exits non-zero on the first failure. Each step is announced before it
# runs so a partial log is still debuggable.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Defensive PATH extension. rustup installs cargo under $HOME/.cargo/bin;
# non-login shells and some CI contexts do not prepend it automatically.
if [ -d "${HOME:-}/.cargo/bin" ]; then
    case ":$PATH:" in
        *":$HOME/.cargo/bin:"*) ;;
        *) export PATH="$HOME/.cargo/bin:$PATH" ;;
    esac
fi

say() {
    printf '\n>>> pod-verify: %s\n' "$*"
}

say "cargo fmt --all -- --check"
cargo fmt --all -- --check

# `cargo check --workspace` typechecks the whole workspace without
# running codegen. It is the cheapest signal for "does the source still
# compile end-to-end" and fits in memory even on constrained pods.
say "cargo check --workspace"
cargo check --workspace

# Clippy one crate at a time — never --workspace on a constrained pod;
# parallel analysis of both crates can push peak memory past the ceiling.
say "cargo clippy -p sigil-runtime --all-targets -- -D warnings"
cargo clippy -p sigil-runtime --all-targets -- -D warnings

say "cargo clippy -p sigil-compiler --all-targets -- -D warnings"
cargo clippy -p sigil-compiler --all-targets -- -D warnings

# Runtime is small; its lib tests fit in memory.
say "cargo test -p sigil-runtime --lib"
cargo test -p sigil-runtime --lib

# Interior-pointer discipline.
say "scripts/check-no-interior-pointers.sh"
./scripts/check-no-interior-pointers.sh

# Grep discipline: no HashMap/HashSet in compiler/src/ outside errors/.
# This mirrors the CI inline check; having it here too means the
# discipline runs on every pod-verify invocation without waiting for CI.
say "discipline grep: no HashMap/HashSet in compiler/src/"
if grep -REn 'HashMap|HashSet' compiler/src/ \
    --include='*.rs' \
    --exclude-dir='errors' \
  | grep -v -E '//.*HashMap|//.*HashSet'; then
    echo "compiler/src/ contains HashMap/HashSet references — see clippy.toml rules." >&2
    exit 1
fi

# Grep discipline: no unwrap/expect in compiler/src/ outside tests or
# explicit #[allow] sites. The clippy disallowed-methods rule catches
# this, but the grep is a belt-and-braces check that works even if
# clippy is skipped in a future refactor.
say "discipline grep: no unwrap/expect in compiler/src/ outside tests"
if grep -REn '\.unwrap\(\)|\.expect\(' compiler/src/ \
    --include='*.rs' \
  | grep -v -E '//.*unwrap|//.*expect|#\[allow|#\[cfg\(test' \
  | grep -v -E '/tests?\.rs:|mod tests'; then
    # Not every match here is a real offender (we can't tell inside-test
    # from outside-test by grep alone). Clippy is the authority; this
    # grep only warns.
    echo "warning: grep saw unwrap/expect sites in compiler/src/. clippy -D warnings above is the authority; if that passed, these are in allowed locations." >&2
fi

# Grep discipline: no panic!() in non-test compiler/src/.
say "discipline grep: no panic!() in non-test compiler/src/"
if grep -REn '\bpanic!\(' compiler/src/ \
    --include='*.rs' \
  | grep -v -E '//.*panic|#\[cfg\(test|#\[allow' \
  | grep -v -E '/tests?\.rs:|mod tests'; then
    echo "warning: grep saw panic!() sites in compiler/src/. clippy -D warnings above is the authority." >&2
fi

# Counter-name drift check: every SIGIL_COUNTER_* literal that
# scripts/measure-throughput.sh extracts must exist in
# runtime/src/counters.rs::NAMES. A runtime-side rename would
# otherwise silently degrade the throughput-report's per-counter
# columns to `null` for the renamed counter, masking the regression.
say "discipline: measure-throughput.sh counter names exist in runtime NAMES"
shell_counter_names=$(grep -oE 'SIGIL_COUNTER_[A-Z0-9_]+' scripts/measure-throughput.sh \
  | sort -u)
missing_counters=""
for name in $shell_counter_names; do
    if ! grep -qF "\"${name}\"" runtime/src/counters.rs; then
        missing_counters="${missing_counters} ${name}"
    fi
done
if [ -n "$missing_counters" ]; then
    echo "ERROR: measure-throughput.sh references SIGIL_COUNTER_* not in runtime/src/counters.rs::NAMES:" >&2
    for name in $missing_counters; do
        echo "  - $name" >&2
    done
    exit 1
fi

say "OK — pod-verify passed"
