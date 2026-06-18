#!/usr/bin/env bash
# smoke.sh — plan A1 Stage 1 task 18, extended across later plans.
#
# Builds the sigil compiler and runs every shipped example, asserting
# stdout matches the documented invariant and exit status is zero.
# Plan A3 adds option_demo.sigil and tree.sigil to the coverage.
#
# Cargo is expected on PATH. Works from any cwd.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

cargo build --release --workspace --quiet

sigil_bin="${repo_root}/target/release/sigil"

tmpdir="$(mktemp -d -t sigil-smoke.XXXXXX)"
trap 'rm -rf "${tmpdir}"' EXIT

# Compile + run an example, assert stdout matches `expected` byte-for-byte.
check_example() {
    local source_path="$1"
    local expected="$2"
    local name
    name="$(basename "${source_path}" .sigil)"
    local out_path="${tmpdir}/${name}"

    "${sigil_bin}" "${source_path}" -o "${out_path}"
    local actual
    actual="$("${out_path}")"

    if [[ "${actual}" != "${expected}" ]]; then
        echo "smoke: FAIL — ${name} stdout mismatch" >&2
        echo "  expected: ${expected}" >&2
        echo "  actual:   ${actual}" >&2
        exit 1
    fi
    echo "smoke: OK (${name})"
}

check_example "${repo_root}/examples/hello.sigil" "hello, world"
check_example "${repo_root}/examples/option_demo.sigil" "$(printf '42\n-1')"
check_example "${repo_root}/examples/tree.sigil" "32767"
check_example "${repo_root}/examples/generic_map.sigil" "$(printf '3\n2')"
check_example "${repo_root}/examples/path_demo.sigil" "$(printf 'usr/local/bin\nhosts\n/etc\narchive.tar | .gz\na/c/d')"
check_example "${repo_root}/examples/multi_hello/main.sigil" "hello from helper"

echo "smoke: OK (all examples passed)"
