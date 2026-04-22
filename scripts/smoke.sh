#!/usr/bin/env bash
# smoke.sh — plan A1 Stage 1 task 18.
#
# Builds the sigil compiler, compiles examples/hello.sigil, runs the
# produced binary, and asserts stdout is exactly "hello, world" and
# exit status is zero. Later plans extend this with additional
# examples.
#
# Cargo is expected on PATH. Works from any cwd.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

cargo build --release --workspace --quiet

sigil_bin="${repo_root}/target/release/sigil"
source_path="${repo_root}/examples/hello.sigil"

tmpdir="$(mktemp -d -t sigil-smoke.XXXXXX)"
trap 'rm -rf "${tmpdir}"' EXIT
out_path="${tmpdir}/hello"

"${sigil_bin}" "${source_path}" -o "${out_path}"

actual="$("${out_path}")"
expected="hello, world"

if [[ "${actual}" != "${expected}" ]]; then
    echo "smoke: FAIL — stdout mismatch" >&2
    echo "  expected: ${expected}" >&2
    echo "  actual:   ${actual}" >&2
    exit 1
fi

echo "smoke: OK (hello-world printed as expected)"
