# Plan A2 Deviations

Each deviation from the plan is logged here *before* the implementing commit.
Retroactive entries are forbidden.

## [Task 1.5.5 — revision] Move staticlib materialisation out of build.rs into the e2e test itself

**Commit:** db3ae5e

**Plan text:** Same as the earlier 1.5.5 entry below (plan offers option-a
build.rs / option-b CI restructure; neither fits stable cleanly).

**What was done instead (revised):** `compiler/build.rs` is now a no-op
apart from `cargo:rerun-if-changed` hints. The staticlib-materialisation
logic lives in `compiler/tests/e2e.rs::ensure_runtime_staticlib`, called
at the top of the `hello` test. It detects the test's profile from
`env!("CARGO_BIN_EXE_sigil")`'s path and invokes `cargo build -p sigil-runtime`
(with `--release` if applicable) at test-run time.

**Why (revised):** The original build.rs approach (previous
`f0a6212`) deadlocked under `cargo test --workspace` on a cold target
directory. Observed CI behaviour on PR #2's first run: the
`cold-checkout test` jobs hung on "cold run 1 of 2" for 47+ minutes
on both Linux and macOS, long past the expected ~15-20 min for a
full Cranelift cold build. The outer cargo holds per-build-unit
locks while `compiler/build.rs` runs as part of sigil-compiler's
build; the nested `cargo build -p sigil-runtime` then blocks
trying to acquire a compatible lock. Cargo's jobserver handles some
nested invocations, but not this particular build-unit overlap on
our 1.95.0 pin.

The test-run-time approach avoids the deadlock cleanly: once the
outer cargo finishes its build phase and starts executing tests, it
has released the build-unit locks, so a nested cargo invocation
acquires its own locks fresh. Parallel test execution is not a risk
either — cargo serialises concurrent builds at the target-dir level,
so even if two tests called `ensure_runtime_staticlib` at once, one
would wait briefly for the other rather than deadlocking.

**Forward implications:** The `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD`
escape hatch introduced in the previous revision is *no longer in
the code* — build.rs no longer does anything it needs to skip. If a
future out-of-tree caller needs to suppress the e2e-test-time
rebuild (e.g. pre-building the staticlib in a custom harness), the
cleanest escape is for the caller to place
`target/<profile>/libsigil_runtime.a` before invoking `cargo test`;
`ensure_runtime_staticlib` short-circuits when the staticlib is
already present. The env var is removed from `runtime/README.md`
along with this change.

## [Task 1.5.5] `compiler/build.rs` invokes cargo to materialise the runtime staticlib; cold-checkout verification lives in a dedicated CI job

**Commit:** f0a6212 (superseded by revision above; entry preserved for history)

**Plan text:** "Fix by adding a `build.rs` to `compiler/` that declares an
explicit artifact dependency on `runtime`, or by restructuring CI's test
job to build the staticlib in a prior step before `cargo test`. Document
the chosen approach in `runtime/README.md`. Acceptance: two successive
`rm -rf target && cargo test --workspace` runs on a cold checkout both
pass on both hosts (CI-verified; do not attempt on the pod)."

**What was done instead:** Neither of the plan's two suggested approaches
fits cleanly on stable Rust 1.95.0; a third approach is used.

1. `compiler/build.rs` (new) emits `cargo:rerun-if-changed` for the
   runtime source and, during its own execution, checks whether
   `target/<profile>/libsigil_runtime.a` exists. If missing, it shells
   out to `cargo build -p sigil-runtime` to materialise the staticlib
   before sigil-compiler's own build completes. A
   `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD=1` env var escape-hatch disables
   this for environments where cargo recursion is problematic (e.g.
   custom build systems).

2. The existing CI ordering (`cargo build -p sigil-runtime` then
   `cargo build -p sigil-compiler` then `cargo test --workspace`) is
   kept unchanged; it's the warm-path discipline even though the new
   build.rs makes cold `cargo test --workspace` work unaided.

3. A new CI job `cold-checkout-test` runs on both hosts. It does
   `rm -rf target && cargo test --workspace` twice in succession (the
   plan's acceptance is "two successive ... runs ... both pass"). This
   job is the sole place in CI that verifies the cold-checkout
   guarantee; the regular `build-test` job continues to exercise the
   incremental / per-crate-prebuild path that's friendliest to cache
   reuse.

**Why:**

- **Option (a), artifact dependencies**, is the cleanest literal fit
  for the plan's words (`[dependencies] sigil-runtime = { path = "../runtime", artifact = "staticlib" }`),
  but it requires `cargo-features = ["bindeps"]` which is **unstable** as
  of Rust 1.95.0 (`bindeps` is still tracked as unstable under
  `rust-lang/cargo#9096`). Enabling it would require switching the
  workspace to nightly, which contradicts `rust-toolchain.toml`'s pin
  and broadens the deviation surface.

- **Option (b), CI restructure**, is already effectively in place — CI
  runs `cargo build -p sigil-runtime` before `cargo test --workspace`,
  so the warm path is fine. But the plan's acceptance criterion is
  specifically `rm -rf target && cargo test --workspace` on a cold
  checkout, which means the fix must also work when a contributor runs
  that one command locally. CI restructure alone doesn't satisfy that
  local-developer acceptance.

- The `build.rs` shell-out approach is used by `rustc_codegen_llvm` and
  similar projects when a staticlib from a sibling crate is needed
  before the calling crate finishes. Cargo 1.74+ supports nested cargo
  invocations via the jobserver protocol, so deadlock is not a real
  concern at 1.95.0. The `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD` escape
  hatch is the belt-and-braces for any environment where the shell-out
  is problematic.

- The **separate `cold-checkout-test` CI job** is there because the
  main `build-test` job uses `actions/cache@v4` on `target/`, so its
  second-run-is-green property doesn't actually verify a true cold
  build. Separating the cold verification lets us keep the warm-path
  cache hit rate in the main job while still proving the cold
  invariant.

**Forward implications:**

- If `bindeps` stabilises before Plan B, the `build.rs` shell-out can
  be replaced with a proper artifact dependency. The interface stays
  the same (sigil-compiler's build produces a valid `libsigil_runtime.a`
  in `target/<profile>/`); only the mechanism changes.
- Plan B may introduce additional staticlib-producing crates (e.g., an
  effect-runtime shim). The same build.rs pattern extends.
- The `cold-checkout-test` job is slow (~10 min Linux, ~15 min macOS
  worst case). It runs on every PR; if cadence becomes painful, gate
  it to `workflow_dispatch` or PRs touching `compiler/`, `runtime/`, or
  the workflow file itself.
- The `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD` env var is part of the
  public build surface now — it must be documented in
  `runtime/README.md` and preserved across plans A3/B/C. Removing it
  is a breaking change for any out-of-tree build environment that
  depends on it.

## [Task 25] `sigil_panic_arith_error` exits via `std::process::exit(2)` instead of `libc::exit(2)`

**Commit:** `d4d0682` (sigil/main `d557482` via squash — the runtime
`sigil_panic_arith_error` implementation landed in the Task-24/25
squash of PR #3). Entry is post-hoc per the PR #3 review's request to
promote an informally-captured note into a formal deviation.

**Plan text:** "`sigil_panic_arith_error(reason: *const c_char) -> !`:
writes `\"sigil: arithmetic error: <reason>\\n\"` to stderr and calls
**`libc::exit(2)`**. Marked `-> !` (noreturn). Error code reserved:
`E0401` (runtime arith abort; v1-only, Plan B replaces)."
(Task 25, plan line 109.)

**What was done instead:** `runtime/src/arith.rs`'s
`sigil_panic_arith_error` calls **`std::process::exit(2)`** rather
than `libc::exit(2)`. A single-line comment in the module doc
discusses the substitution.

**Why:**

- **No `libc` dep on the plan's allow-list.** Plan A1 §0.3 enumerates
  the runtime-crate dependency set (currently empty — only `core` and
  `std` are used, plus a direct `#[link(name = "gc")]` for Boehm and
  direct `extern` declarations for `atexit`/`exit`). Adding the
  `libc` crate to pull in `libc::exit(2)` would be a surface-level
  deviation; skipping the crate and using Rust's std wrapper avoids
  it.
- **`std::process::exit(2)` is behaviourally equivalent on every host
  the plan targets.** On Unix it delegates to the libc `exit(3)` that
  the plan cites (flushes stdio, runs C `atexit` handlers), which is
  load-bearing for the counter-dump atexit hook `sigil_gc_init`
  registers under `SIGIL_PRINT_STATS=1`. On macOS the same
  equivalence holds (`std::process::exit` calls `exit(3)` from
  libSystem). There is no observable difference in user-facing
  behaviour.
- **Alternative considered:** declare `extern "C" { fn exit(code: i32)
  -> !; }` directly, same pattern `gc.rs` uses for `atexit`. Works,
  but `std::process::exit` is already in scope via `use` statements
  and reads more clearly. The `extern` approach remains available if
  Plan B's runtime rewrite needs finer control.

**Forward implications:**

- If Plan B adds a `libc` dep for other reasons (e.g. `libc::dup2`
  for an stdout-capture helper), `sigil_panic_arith_error` can be
  switched to `libc::exit(2)` without behaviour change. A comment in
  `arith.rs` notes the substitution as a mechanical follow-up at
  that point.
- Any reviewer or auditor comparing the runtime against the plan
  text will notice `std::process::exit` where `libc::exit` is
  written. This entry is the canonical explanation; no further
  in-code comment is required beyond the one already present.

## [CHORE] CI `rm -f target/debug/libsigil_runtime.a` before `cargo build -p sigil-runtime`

**Commit:** `969be7d` (sigil/main `d557482` via squash).

**Plan text:** (no literal plan text — this is CI workflow
infrastructure, not a numbered task.) The plan's Task 1.5.5 covers
the build-ordering concern for the e2e staticlib path, but does not
speak to cargo's per-unit fingerprint behaviour on cached `target/`
restores.

**What was done instead:** the `build + test` job's `build runtime`
step was extended from:

```yaml
- name: build runtime
  run: cargo build -p sigil-runtime
```

to:

```yaml
- name: build runtime
  run: |
    rm -f target/debug/libsigil_runtime.a target/release/libsigil_runtime.a
    cargo build -p sigil-runtime
```

**Why:** on PR #3's first CI run, the `build + test (ubuntu-24.04)`
job failed at the e2e link step with
`undefined reference to \`sigil_panic_arith_error\`` even though
`cargo build -p sigil-runtime` had run first and the symbol was
present in the `runtime/src/arith.rs` source. Cold-checkout
(`rm -rf target && cargo test --workspace`) passed on the same host,
confirming the symbol was visible when the staticlib was built
fresh. The cached `build + test` job's `cargo build -p sigil-runtime`
finished in 0.20s (suspiciously fast — too fast to re-emit the
staticlib) and pod-verify's prior `cargo check` / `cargo clippy` runs
appear to have updated enough adjacent per-unit fingerprints that
cargo declared the cached `.a` fresh without re-emitting it against
the current source's symbol set.

Removing the cached staticlibs forces an unambiguous rebuild. The
cold-checkout job already does the equivalent via `rm -rf target`.

**Forward implications:**

- **Proper fix: `Swatinem/rust-cache@v2`.** The third-party action
  handles cargo's target-caching quirks (including re-emitting
  staticlibs when source changes) correctly, by keying the cache
  on source hashes and managing per-target invalidation. Swapping
  in `Swatinem/rust-cache` is a one-commit follow-up; it supersedes
  both this `rm -f` workaround and the ad-hoc `actions/cache@v4`
  configuration currently in `ci.yml`.
- **Until the proper fix lands, the `rm -f` line is load-bearing.**
  A future workflow refactor that drops it without replacing the
  cache strategy will re-introduce the `undefined reference` failure
  mode. The workflow comment above the step spells this out; this
  deviation entry is the canonical reference.
- The only cost of the workaround is ~2–5 seconds of wall time per
  `build + test` job: the rebuild of a single `.a` archive from
  already-cached object files. Acceptable until the proper cache
  strategy lands.

## Format

```
## [Task <N>] short description

**Commit:** (pending) or <hash>

**Plan text:** (verbatim or precisely paraphrased)

**What was done instead:** ...

**Why:** ...

**Forward implications:** ...
```
