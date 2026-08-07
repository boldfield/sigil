# Feature spec — macOS static Boehm GC build fails silently, dropping the darwin release artifact

**Status:** board milestone (sigil project). **Date:** 2026-08-07.

## Problem

The v1.4.2 release shipped **no darwin artifact**. The
`build aarch64-apple-darwin` job failed, and the release carries only
the Linux tarball and its checksum. This is a regression: v1.4.0 and
v1.4.1 both published a darwin build.

Two defects, one masking the other.

**1. `ar -D` does not exist on macOS.** `use_homebrew_on_macos()` in
`scripts/build-static-boehm.sh` locates Homebrew's `bdw-gc` static
archive, then repacks it — extracting the objects and rebuilding the
archive with `ar -D` for byte-for-byte determinism. GNU binutils `ar`
implements `-D`; the BSD/cctools `ar` that ships with macOS does not.
The repack therefore never produces `libgc.a`, and the subsequent copy
into the destination directory fails.

**2. The failure is swallowed.** The function does not check whether
the archive was produced, and returns success regardless. So the
`build static Boehm GC` step reports green while having produced
nothing. The error only surfaces three steps later, when
`stage release tarball` tries to copy the missing file:

```
Using Homebrew Boehm GC from /opt/homebrew/opt/bdw-gc/lib/libgc.a
cp: libgc.a: No such file or directory
...
cp: target/release/boehm/libgc.a: No such file or directory
```

The reported failing step is `stage release tarball`, which points
diagnosis at the packaging logic rather than at the GC build. That
misdirection is worth fixing independently of the `ar` flag: a step
that produces nothing must be the step that goes red.

Evidence: release run
<https://github.com/boldfield/sigil/actions/runs/31198836981>, job
`build aarch64-apple-darwin`, failing step `stage release tarball`.

The Linux path is unaffected and is confirmed working — the v1.4.2
Linux tarball was verified on a Debian 12 host (GLIBC 2.36, no
`libgc-dev` installed): the compiler executes, and programs compile,
link, and run against the bundled `lib/libgc.a`.

## Goal

A `v*` tag produces a complete release — both the
`x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` tarballs, each
with its checksum and each carrying a working static `libgc.a` — and
any failure to produce the GC archive fails the step that was
supposed to produce it.

## Approach

Do not depend on `ar -D` on macOS. The repack exists solely to make the
archive byte-reproducible; it is not needed for correctness, since
Homebrew's archive is already a valid static library. Either copy that
archive through unchanged on darwin, or probe whether the available
`ar` accepts `-D` and skip the repack when it does not. If
reproducibility on macOS is judged worth keeping, `zero_ar_date` is the
platform-native lever rather than `-D` — but treating determinism as
best-effort on darwin, and saying so in the script's header comment, is
the simpler and more honest option.

Separately, make the helper verify its own output before reporting
success: confirm the destination archive exists and is a valid archive,
and fail the script otherwise. Every path through the script should be
covered by that check, not just the Homebrew one, so this class of
silent no-op cannot recur in the from-source path either.

Worth evaluating while in here: whether the Homebrew shortcut should
exist at all. The from-source path already builds a known-good Boehm
(>= 8.2.4) and is what Linux uses. Using it on both platforms would
delete a platform-specific branch, remove the dependency on whatever
version Homebrew currently ships, and make the two artifacts more
alike. The tradeoff is macOS build time. Decide deliberately rather
than keeping the shortcut by default.

## Acceptance

- Pushing a `v*` tag produces a release with all four assets: both
  platform tarballs and both `.sha256` files.
- The darwin tarball unpacks to the same layout as the Linux one —
  `bin/sigil` alongside `lib/libgc.a` — so `locate_gc_lib()` resolves
  the archive through the release-archive path without `SIGIL_GC_LIB`.
- The macOS `smoke (compile + run hello)` step passes.
- If the GC archive cannot be produced, `build static Boehm GC` is the
  step that fails, and its log states why. A later step never fails on
  a missing artifact that an earlier step silently skipped.
- The Linux path is unchanged and still yields a binary that runs on a
  GLIBC 2.36 host with no system Boehm GC installed.

## Testing (required)

- Run the script directly on macOS and confirm it produces a valid
  static archive at the destination; confirm a compiled Sigil program
  runs on a machine with no Homebrew `bdw-gc` on the library path.
- Force the failure case — make the archive unproducible — and confirm
  the script exits non-zero at that point rather than returning
  success.
- Re-run the release workflow to completion and verify all four assets.
  The workflow exposes a manual dispatch trigger for retroactively
  releasing an existing tag, so this can be exercised without minting a
  throwaway version.
- Confirm the Linux job still passes and its artifact still runs on
  GLIBC 2.36.

## Out of scope

- The Linux GLIBC baseline and static-GC work, which landed and is
  verified. See `2026-06-23-static-link-boehm-gc.md`.
- Bumping `SIGIL_VERSION` in sigil-programs, tracked on that board.
- Re-cutting v1.4.2. Once this lands, the next tag produces a complete
  release; v1.4.2's Linux artifact remains valid and in use meanwhile.
