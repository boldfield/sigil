# Plan A2 Deviations

Each deviation from the plan is logged here *before* the implementing commit.
Retroactive entries are forbidden.

## [Task 32] `call_indirect` path is `unreachable!` in Plan A2; direct-call covers every well-typed program

**Commit:** cb6967c

**Plan text:** "Extend codegen: closure calling convention (`arg0 = closure
ptr, arg1..argN = args`); indirect call via the closure's code pointer;
closure allocation on the GC heap."

**What was done instead:** Every `Expr::Call` whose callee is syntactically
`Expr::Ident(name)` (where `name` is a registered user fn) or
`Expr::ClosureRecord { code_fn_name, .. }` is lowered as a **direct**
Cranelift `call` to the relevant `FuncRef`. Direct calls to top-level fns
pass a null pointer as the closure_ptr argument; calls whose callee is a
`ClosureRecord` allocate the record first (via `sigil_alloc` + `store`
sequence) and pass the record's header pointer as closure_ptr. Callees
with any other syntactic shape hit an `unreachable!("codegen: indirect
call deferred to Plan A3 (TypeExpr::Fn not in A2)")` arm in `lower_call`.

Closure allocation and the `code_ptr` field of `ClosureRecord` are
*fully* implemented — every closure record stores a valid code_ptr
suitable for an indirect call, even though no call site in a well-typed
Plan A2 program actually loads it.

**Why:** Indirect calls require the callee's Cranelift signature at the
call site (param types + return type) to build a Cranelift `SigRef` via
`builder.import_signature`. For a callee like `Expr::Ident(local_name)`
where `local_name` was bound to a closure value, the signature lives on
that closure's `Ty::Fn(FnSig)` — which in turn requires the
`TypeExpr::Fn` surface syntax so that let bindings, function parameters,
and return types can carry function types through the type system.
Plan A2 deliberately defers `TypeExpr::Fn` (see Task 30's PROGRESS
note: "`TypeExpr::Fn` surface syntax deliberately deferred"). Without
it, no well-typed Plan A2 program can produce a call site whose callee
is not already an `Ident(top_level_fn)` or a `ClosureRecord`. Every such
callee is direct-callable, so implementing `call_indirect` in Plan A2
would add a code path that cannot be exercised by any program — a
testing void that's worse than leaving the arm unreachable with a
helpful message.

The closure record's `code_ptr` field is still populated correctly (via
`func_addr` over the synthetic fn's `FuncRef`), so when Plan A3 lands
`TypeExpr::Fn` the indirect-call path only needs to (a) reconstruct the
callee signature from the callee's typechecked `Ty::Fn(FnSig)` and (b)
emit `builder.ins().call_indirect(sig_ref, code_ptr_loaded, &all_args)`
— the runtime layout it loads against is already the shipped shape.

**Forward implications:** Plan A3 (`TypeExpr::Fn` surface syntax +
typechecker support for fn-typed lets and params) must:

1. Replace the `unreachable!` arm in `Lowerer::lower_call` with a real
   indirect-call lowering. The path loads `code_ptr` from the callee
   pointer at offset 8 (past the 8-byte header), builds a Cranelift
   `SigRef` matching the callee's `Ty::Fn` signature, and emits
   `call_indirect`.
2. Re-apply `push_placeholder` after the indirect call (stackmap
   discipline unchanged).
3. Remove the defensive `types::I64` fall-back in `Lowerer::type_of_expr`
   for the `Expr::Call` arm whose callee is neither an `Ident` of a
   top-level fn nor a `ClosureRecord` — that fall-back exists only to
   make `lower_match` well-typed in the presence of a `Call` scrutinee
   that Plan A2 cannot construct.
4. Extend the e2e test suite with indirect-call tests once fn-typed let
   bindings parse.

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
