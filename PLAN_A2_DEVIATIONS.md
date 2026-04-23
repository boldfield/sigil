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

## Format

```
## [Task <N>] short description

**Commit:** (pending) or <hash>

**Plan text:** (verbatim or precisely paraphrased)

**What was done instead:** ...

**Why:** ...

**Forward implications:** ...
```
