#!/usr/bin/env bash
# build-static-boehm.sh — build a pinned, deterministic Boehm GC static archive
#
# Builds Boehm GC (>= 8.2.4) from source as a self-contained static libgc.a,
# configured with:
#   --enable-static --disable-shared --enable-threads=posix
#   --enable-parallel-mark --with-libatomic-ops=none
#
# The archive is repacked with ar -D (deterministic: no timestamps/uids/order)
# so reproducibility checks stay green.
#
# PINNED VERSION: 8.2.12
# URL: https://github.com/bdwgc/bdwgc/releases/download/v8.2.12/gc-8.2.12.tar.gz
# SHA256: 42e5194ad06ab6ffb806c83eb99c03462b495d979cda782f3c72c08af833cd4e
#
# Usage: $0 [DEST_DIR]
#   DEST_DIR: destination directory for libgc.a (default: ./build/boehm)
#
# On macOS: uses $(brew --prefix bdw-gc)/lib/libgc.a if present and static,
# otherwise builds from source. On Linux: always builds from source.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"

# Destination directory for libgc.a (default: ./build/boehm)
DEST_DIR="${1:-./build/boehm}"
# Make path absolute
if [[ "${DEST_DIR}" != /* ]]; then
    DEST_DIR="${repo_root}/${DEST_DIR}"
fi

# Boehm GC pinned version and download location
BOEHM_VERSION="8.2.12"
BOEHM_URL="https://github.com/bdwgc/bdwgc/releases/download/v${BOEHM_VERSION}/gc-${BOEHM_VERSION}.tar.gz"
BOEHM_SHA256="42e5194ad06ab6ffb806c83eb99c03462b495d979cda782f3c72c08af833cd4e"

# Utility to compute hash portably
compute_hash() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "${file}" | awk '{print $1}'
    else
        shasum -a 256 "${file}" | awk '{print $1}'
    fi
}

# Try to use Homebrew's static archive on macOS
use_homebrew_on_macos() {
    if [[ "${OSTYPE}" != "darwin"* ]]; then
        return 1
    fi

    if ! command -v brew >/dev/null 2>&1; then
        return 1
    fi

    local brew_libgc
    brew_libgc="$(brew --prefix bdw-gc 2>/dev/null)/lib/libgc.a" || return 1

    if [[ ! -f "${brew_libgc}" ]]; then
        return 1
    fi

    # Verify it's actually static (ELF archive or Mach-O archive signature)
    if ! file "${brew_libgc}" | grep -q "ar archive"; then
        return 1
    fi

    echo "Using Homebrew Boehm GC from ${brew_libgc}" >&2

    # Repack with ar -D for determinism (no timestamps, uids, or file order nondeterminism)
    local repack_dir="/tmp/sigil-boehm-repack-work"
    rm -rf "$repack_dir"
    mkdir -p "$repack_dir"
    cd "$repack_dir"
    ar -x "${brew_libgc}"

    # Delete any existing archive and recreate deterministically
    rm -f libgc.a
    # Create the archive with deterministic mode (-D) using an explicit ordered list
    ar -D crs libgc.a $(ls -1 *.o | sort)

    # Install to destination
    mkdir -p "${DEST_DIR}"
    cp libgc.a "${DEST_DIR}/libgc.a"
    rm -rf "$repack_dir"
    return 0
}

# Build Boehm GC from source
build_from_source() {
    # Use fixed paths for reproducibility (no random mktemp directories)
    local tmpdir="/tmp/sigil-boehm-build-work"
    local repack_dir="/tmp/sigil-boehm-repack-work"

    # Clean up any previous build
    rm -rf "$tmpdir" "$repack_dir"
    mkdir -p "$tmpdir" "$repack_dir"

    # Set up cleanup trap
    # shellcheck disable=SC2064
    trap "rm -rf '$tmpdir' '$repack_dir' 2>/dev/null || true" RETURN

    local tarball="${tmpdir}/gc-${BOEHM_VERSION}.tar.gz"
    local extracted="${tmpdir}/gc-${BOEHM_VERSION}"

    echo "Downloading Boehm GC ${BOEHM_VERSION}..." >&2
    curl -sSL -L "${BOEHM_URL}" -o "${tarball}"

    # Verify sha256
    local actual_hash
    actual_hash="$(compute_hash "${tarball}")"
    if [[ "${actual_hash}" != "${BOEHM_SHA256}" ]]; then
        echo "SHA256 mismatch for ${BOEHM_URL}" >&2
        echo "  Expected: ${BOEHM_SHA256}" >&2
        echo "  Actual:   ${actual_hash}" >&2
        exit 1
    fi

    echo "Extracting Boehm GC..." >&2
    tar -xzf "${tarball}" -C "${tmpdir}"

    cd "${extracted}"

    echo "Configuring Boehm GC..." >&2
    ./configure \
        --enable-static \
        --disable-shared \
        --enable-threads=posix \
        --enable-parallel-mark \
        --with-libatomic-ops=none

    echo "Building Boehm GC..." >&2
    make -j "$(nproc 2>/dev/null || sysctl -n hw.logicalcpu 2>/dev/null || echo 4)"

    # Repack with ar -D for determinism (no timestamps, uids, or file order nondeterminism)
    cd "${repack_dir}"
    ar -x "${extracted}/.libs/libgc.a"

    # Delete any existing archive and recreate deterministically
    rm -f libgc.a
    # Create the archive with deterministic mode (-D) using an explicit ordered list
    ar -D crs libgc.a $(ls -1 *.o | sort)

    # Install to destination
    mkdir -p "${DEST_DIR}"
    cp libgc.a "${DEST_DIR}/libgc.a"

    echo "Boehm GC ${BOEHM_VERSION} built and installed to ${DEST_DIR}/libgc.a" >&2
}

# Main entry point
main() {
    # Try Homebrew on macOS first
    if use_homebrew_on_macos; then
        return 0
    fi

    # Fall back to building from source
    build_from_source
}

main "$@"
