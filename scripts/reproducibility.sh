#!/usr/bin/env bash
# reproducibility.sh — plan A1 Stage 1 task 17, extended across later plans.
#
# Compiles every shipped example twice on the same host and asserts the
# two output binaries are byte-identical (sha256sum). Reproducibility
# is per-host: we make no claim about cross-host binary identity. Plan
# A3 adds option_demo.sigil and tree.sigil to the coverage so the
# layout-table tag allocation and match decision-tree codegen stay
# byte-stable.
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

if [[ ! -x "${sigil_bin}" ]]; then
    echo "reproducibility: sigil binary not found at ${sigil_bin}" >&2
    exit 1
fi

tmpdir="$(mktemp -d -t sigil-repro.XXXXXX)"
trap 'rm -rf "${tmpdir}"' EXIT

# sha256sum is GNU coreutils (Linux, or macOS w/ Homebrew). shasum -a 256
# is portable Perl and ships on macOS by default. Prefer sha256sum when
# present so Linux CI behaviour is unchanged.
if command -v sha256sum >/dev/null 2>&1; then
    hash_cmd=(sha256sum)
else
    hash_cmd=(shasum -a 256)
fi

# On macOS the ad-hoc code signature's Identifier is derived from the
# output filename, so two compiles to different filenames produce
# byte-different binaries even though the compilation is reproducible.
# Compile twice to the *same* filename (in per-example per-run
# subdirectories) so filename-derived metadata is identical across
# the two binaries.
check_example() {
    local source_path="$1"
    local name
    name="$(basename "${source_path}" .sigil)"

    if [[ ! -f "${source_path}" ]]; then
        echo "reproducibility: ${source_path} missing" >&2
        exit 1
    fi

    local dir_a="${tmpdir}/${name}/a"
    local dir_b="${tmpdir}/${name}/b"
    mkdir -p "${dir_a}" "${dir_b}"
    local out_a="${dir_a}/${name}"
    local out_b="${dir_b}/${name}"

    "${sigil_bin}" "${source_path}" -o "${out_a}"
    "${sigil_bin}" "${source_path}" -o "${out_b}"

    local hash_a hash_b
    hash_a="$("${hash_cmd[@]}" "${out_a}" | awk '{print $1}')"
    hash_b="$("${hash_cmd[@]}" "${out_b}" | awk '{print $1}')"

    if [[ "${hash_a}" != "${hash_b}" ]]; then
        echo "reproducibility: FAIL — ${name}: same-host builds produced different binaries" >&2
        echo "  ${out_a}: ${hash_a}" >&2
        echo "  ${out_b}: ${hash_b}" >&2
        exit 1
    fi
    echo "reproducibility: OK (${name} sha256=${hash_a})"
}

check_example "${repo_root}/examples/hello.sigil"
check_example "${repo_root}/examples/option_demo.sigil"
check_example "${repo_root}/examples/tree.sigil"
check_example "${repo_root}/examples/generic_map.sigil"

echo "reproducibility: OK (all examples reproducible)"
