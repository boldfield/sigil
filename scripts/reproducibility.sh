#!/usr/bin/env bash
# reproducibility.sh — plan A1 Stage 1 task 17.
#
# Compiles examples/hello.sigil twice on the same host and asserts the
# two output binaries are byte-identical (sha256sum). Reproducibility
# is per-host: we make no claim about cross-host binary identity.
#
# Cargo is expected on PATH. The script works from any cwd because it
# resolves the repo root from its own location.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

# Build release binaries (compiler + runtime staticlib).
cargo build --release --workspace --quiet

sigil_bin="${repo_root}/target/release/sigil"
source_path="${repo_root}/examples/hello.sigil"

if [[ ! -x "${sigil_bin}" ]]; then
    echo "reproducibility: sigil binary not found at ${sigil_bin}" >&2
    exit 1
fi
if [[ ! -f "${source_path}" ]]; then
    echo "reproducibility: ${source_path} missing" >&2
    exit 1
fi

tmpdir="$(mktemp -d -t sigil-repro.XXXXXX)"
trap 'rm -rf "${tmpdir}"' EXIT

# On macOS the ad-hoc code signature's Identifier is derived from the
# output filename, so two compiles to different filenames produce
# byte-different binaries even though the compilation is reproducible.
# Compile twice to the *same* filename (in per-run subdirectories) so
# filename-derived metadata is identical across the two binaries.
dir_a="${tmpdir}/a"
dir_b="${tmpdir}/b"
mkdir -p "${dir_a}" "${dir_b}"
out_a="${dir_a}/hello"
out_b="${dir_b}/hello"

"${sigil_bin}" "${source_path}" -o "${out_a}"
"${sigil_bin}" "${source_path}" -o "${out_b}"

hash_a="$(sha256sum "${out_a}" | awk '{print $1}')"
hash_b="$(sha256sum "${out_b}" | awk '{print $1}')"

if [[ "${hash_a}" != "${hash_b}" ]]; then
    echo "reproducibility: FAIL — same-host builds produced different binaries" >&2
    echo "  ${out_a}: ${hash_a}" >&2
    echo "  ${out_b}: ${hash_b}" >&2
    exit 1
fi

echo "reproducibility: OK (sha256=${hash_a})"
