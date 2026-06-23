# Feature spec — static-link Boehm GC into the toolchain

**Status:** board milestone (sigil project). **Date:** 2026-06-23.

## Problem

Sigil-compiled programs link Boehm GC **dynamically** (`cc … -lgc …` in
`compiler/src/link.rs`). Two consequences bite portability:

1. **The runtime requires a newer Boehm than older distros ship.** The
   runtime calls `GC_set_markers_count` to pin Boehm to single-marker
   mode before `GC_init` (load-bearing for the precise GC — parallel
   markers break alloc-heavy workloads, PR #170). That symbol exists in
   ubuntu-24.04's libgc but **not** ubuntu-22.04's older Boehm. So the
   v1.4.1 release build, which builds the Linux binary on ubuntu-22.04
   for an older glibc baseline, fails at link with
   `undefined reference to GC_set_markers_count`.

2. **Even if the build passed, the fleet would still break.** Worker
   hosts run the same older-vintage libgc, so a dynamically-linked
   program would hit the same missing symbol at the user's own link
   step. The host's libgc version — not just glibc — is the real
   portability constraint.

## Goal

Sigil-compiled programs link a **recent Boehm GC statically** from a
`libgc.a` bundled in the toolchain, instead of dynamically via `-lgc`.

After this lands, the released Linux binary needs only glibc ≥ 2.35 and
**no host libgc at all**. This both fixes the fleet portability bug and
**subsumes** the `GC_set_markers_count` failure: a from-source recent
Boehm defines the symbol, so it simply resolves at the (now static)
final link. **No `runtime/src/gc.rs` change is required** — the call
stays; only where the symbol is resolved from changes.

## Approach

- Build a **pinned, recent** Boehm GC (≥ 8.2.4) from source as a
  self-contained static archive (`libgc.a`), with threads + parallel
  mark enabled and atomic-ops provided by compiler intrinsics so no
  separate `libatomic_ops.a` is needed. The archive must be
  **deterministic** so the reproducibility gate (every example compiles
  to a byte-identical binary) stays green.
- Teach the linker (`compiler/src/link.rs`) to locate a static
  `libgc.a` the same way it locates `libsigil_runtime.a` (an env
  override, the release-archive `../lib/` layout, a flat layout, the
  cargo `target/` tree) and, when found, link it by absolute path
  instead of `-lgc`. When **no** static `libgc.a` is present, fall back
  to the current dynamic `-lgc` path (with the macOS pkg-config search
  paths) so building from a plain dev checkout is unchanged.
- Ship `lib/libgc.a` in the release tarball and link the release smoke
  step against it, so the published toolchain is self-contained.

## Scope / boundaries

- **In scope:** the build script for the static archive; the linker
  change for compiled user programs; the release packaging; the
  install-docs nuance.
- **Out of scope:** the runtime crate's own test linking
  (`cargo test -p sigil-runtime`, `#[link(name = "gc")]`) — a
  build-from-source developer concern, not the released-binary
  portability story. `libgc-dev` remains a **build-from-source**
  dependency; only **runtime** dependence on a host libgc is removed.
- **No code in this spec.** Each task carries its own exact,
  mechanically-checkable acceptance criteria.

## Task DAG

1. `boehm-build-script` (deps: none) — the deterministic static-`libgc.a`
   build script.
2. `gc-static-link` (deps: 1) — `link.rs` locates and statically links
   `libgc.a`, with `-lgc` fallback.
3. `gc-static-release` (deps: 1, 2) — `release.yml` builds + stages
   `lib/libgc.a`, links the smoke step statically, drops the libgc
   runtime prereq from the tarball README.
4. `gc-static-docs` (deps: 3) — clarify build-time vs runtime libgc in
   the canonical docs (`README.md`, `runtime/README.md`,
   `spec/language.md`).

## Done definition

All four tasks merged; the release build passes on ubuntu-22.04 with a
self-contained binary; `ldd` of a program compiled from the tarball
shows no libgc; the glibc baseline is ≤ 2.35. Then v1.4.1 is re-cut and
sigil-programs is bumped to v1.4.1.
