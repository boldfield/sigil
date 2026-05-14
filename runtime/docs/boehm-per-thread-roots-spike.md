# Boehm per-thread roots — spike findings

## Status: spike complete, Plan E2 Phase 3 Task 10. Updated with Task 11 findings (PR #170).

This document pins the Boehm API surface Phase 3 will use to:
- Mark Sigil program threads as "precise stack roots" (root
  locations supplied by the stackmap-driven walker built in Plan
  E2 Phase 1).
- Keep runtime-internal threads (Plan E1's profile drainer; any
  future runtime-spawned thread) on conservative stack scan.

**Task 11's PR #170 integration work surfaced three Boehm 8.x
interaction constraints the spike did not anticipate. See
[`# Task 11 follow-up findings (PR #170)`](#task-11-follow-up-findings-pr-170)
at the bottom of this doc for the breadcrumbs; Task 12's
implementer should read that section first.**

The plan body posed it as a single question:

> `GC_register_my_thread` — does it support a "precise root
> callback" parameter, or is the precise/conservative distinction
> global?

**Answer: the distinction is global.** Boehm has no per-thread
"precise vs conservative" switch on `GC_register_my_thread`; the
function takes only a `const struct GC_stack_base *`
(`gc.h` line 1575):

```c
GC_API int GC_CALL GC_register_my_thread(const struct GC_stack_base *)
                                                  GC_ATTR_NONNULL(1);
```

The workaround the plan body anticipated — *"custom marker +
thread-local 'precise enabled' flag, manually invoking the precise
walker for Sigil threads"* — is structurally what Boehm itself
documents as "make stack scanning more precise" (`gc.h` line
1620). The full mechanism uses three APIs that have nothing to do
with `GC_register_my_thread`:

| API | Role |
|---|---|
| `GC_do_blocking(fn, cd)` | Run `fn(cd)` with the calling thread in the "inactive" state — Boehm excludes the thread's frames belonging to functions in the inactive state from conservative stack scan. |
| `GC_call_with_gc_active(fn, cd)` | Inverse: switch back to "active" state for `fn`'s lifetime. Reachability inside `fn` is scanned conservatively again. |
| `GC_set_push_other_roots(proc)` | Register a callback invoked during the mark phase. The callback pushes additional root ranges via `GC_push_all_eager(start, end)` or `GC_push_conditional(start, end, all)`. Sigil's stackmap-driven walker plugs in here to supply precise root locations from inactive Sigil-thread stacks. |

## How Phase 3 will use these

The plan body's Tasks 11 + 12 will:

1. **Wrap the Sigil program's run loop in `GC_do_blocking`.**
   `sigil_run_loop` (the trampoline that dispatches NextStep
   values back into Cranelift-generated user code) becomes the
   "inactive" region. The Sigil call stack inside that region is
   *not* scanned by Boehm's automatic stack scan.

2. **Register a `GC_push_other_roots` callback** that walks the
   inactive Sigil-thread stack via `stackmap::walk_for_gc()` and
   pushes each `RootLocation.addr` as a precise root via
   `GC_push_all_eager(addr, addr + 8)`. This is the
   stackmap-driven precise root supply for the marker.

3. **The `sigil_alloc`-from-blocked-thread shape is OPEN — Task
   11 picks empirically.** Two design choices Task 11 must
   choose between:

   - **Option A: wrap `sigil_alloc` in
     `GC_call_with_gc_active`.** Documented-safe per `gc.h`
     line 1626-1636: *"the user function is allowed to call any
     GC function and/or manipulate pointers to the garbage
     collected heap."* Cost: every alloc pays the
     active-state-switch overhead.
   - **Option B: allow `sigil_alloc` to call from blocked
     state.** Has no documented backing in `gc.h` — `GC_do_blocking`
     says the thread "is not suspended" (which is about STW
     signal handling, NOT about which GC entry points are safe
     to call). Whether `GC_malloc_explicitly_typed` is safe
     from a blocked thread is undocumented, so empirical
     verification is the only path.

   Phase 3's correctness hinges on this choice — Task 11 must
   verify before committing. The most likely outcome is Option
   A (cost is small + documented-stable), but a measured
   comparison via the runtime test suite + the Phase 2 throughput
   workloads is the discriminator.

4. **Runtime-internal threads stay conservative.** The Plan E1
   profile drainer thread (`runtime/src/profile/cpu.rs`) is NOT
   wrapped in `GC_do_blocking`. Its stack is scanned by Boehm's
   default conservative scan. Same for any future runtime-
   spawned thread that doesn't run Sigil bytecode.

The "per-thread" distinction is therefore not a Boehm API choice
— it's a runtime-side choice of *which threads run inside
`GC_do_blocking`*. The same `GC_register_my_thread` call enrolls
every thread; only the do_blocking wrapping differs.

## API reference (the surface Phase 3 will touch)

The Sigil FFI uses `*const c_void` for `GC_stack_base *` rather
than defining a Rust mirror of the struct, because every Sigil
caller passes `NULL` and lets Boehm auto-detect the stack base
via `GC_get_stack_base`. The `*const c_void` typing is how the
existing `runtime/src/test_support.rs::GcThreadEnrolment` and
the Phase 2 spike already declare the symbol; the spec below
preserves that convention.

```rust
// gc.h
extern "C" {
    // Register the calling thread. Stack base auto-detected via
    // GC_get_stack_base(); we already use `GC_register_my_thread(NULL)`
    // in the runtime's test_support.
    pub fn GC_register_my_thread(sb: *const c_void) -> i32;

    // Run fn(cd) with the calling thread in "inactive" state.
    // The thread's frames belonging to fn are not scanned.
    pub fn GC_do_blocking(
        fn_: extern "C" fn(*mut c_void) -> *mut c_void,
        cd: *mut c_void,
    ) -> *mut c_void;

    // Inverse: run fn(cd) with the calling thread in "active"
    // state, even if the surrounding context is inactive.
    pub fn GC_call_with_gc_active(
        fn_: extern "C" fn(*mut c_void) -> *mut c_void,
        cd: *mut c_void,
    ) -> *mut c_void;
}

// gc_mark.h
extern "C" {
    pub type GC_push_other_roots_proc = extern "C" fn();
    pub fn GC_set_push_other_roots(p: GC_push_other_roots_proc);

    // Inside a push_other_roots_proc, push a root range:
    pub fn GC_push_all_eager(bottom: *mut c_void, top: *mut c_void);
}
```

## macOS aarch64 quirks

The plan body called out macOS aarch64 historically having
Boehm quirks with thread suspension and stack-bottom detection.
Surveying libgc 8.x:

- **Thread suspension on Darwin** uses Mach `task_threads_for_pid`
  / `thread_suspend` rather than POSIX signals (`GC_DARWIN_THREADS`
  is defined per-target, not a runtime flag). This works for the
  thread-stop-the-world phase of marking. Sigil's existing
  Plan B Task 56 + Plan E2 Phase 1 tests run on the macos-14
  CI lane today without divergence, so the suspension path is
  validated.

- **Stack-bottom detection** on Darwin uses
  `pthread_get_stackaddr_np()` (which returns the *highest*
  stack address — the bottom on a downward-growing stack).
  `runtime/src/stackmap_xcheck.rs::thread_stack_base()` already
  uses this for the cross-check harness; it's the same call
  Boehm uses internally on `GC_register_my_thread(NULL)`.

- **`GC_do_blocking` on Darwin.** The "inactive" state mechanism
  is portable across libgc 8.x targets; the Darwin port has no
  special-cased divergence. Verified in libgc source
  (`pthread_support.c` ≈ line 2150 in 8.2.x: `GC_do_blocking_inner`
  works the same on POSIX-thread Darwin as on Linux).

- **`GC_set_push_other_roots`** is platform-independent — the
  callback runs during the marker on whichever thread holds the
  GC lock. No Darwin-specific gotchas.

**No 8.x escalation needed:** every API Phase 3 Tasks 11 + 12
need is present on both `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin`, with consistent semantics across the two
targets.

## What this spike does NOT decide

- Whether `sigil_alloc` needs explicit `GC_call_with_gc_active`
  wrapping (open question above). Task 11's first iteration
  proves this empirically by enabling do_blocking on the test
  threads and running the existing test suite — if the runtime
  test suite passes without an `active` wrapper, we don't need
  one.

- The shape of the runtime-side `GC_push_other_roots_proc`. v1
  candidates:
  1. Walk every registered Sigil thread's stack via
     `stackmap::walk_for_gc()`. Requires the runtime to maintain
     a list of "Sigil program threads" — straightforward since
     thread enrolment is a runtime entry point.
  2. Only walk the current thread's stack (caveat: Boehm calls
     the callback once per mark phase, not per thread). Not
     viable for multi-threaded Sigil programs (Plan E2 doesn't
     ship multi-threading, but Phase 3's design shouldn't paint
     itself into a corner).

  Option 1 is the right shape. Task 11's deliverable will
  introduce the thread registry.

- **TODO for Task 11/12:** the precise / conservative split
  interacts with `SIGIL_GC_CROSS_CHECK` (Plan E2 Phase 1's
  cross-check harness). The harness walks the stackmap and
  asserts every precise root address is heap-pointer-shaped
  per Boehm's conservative recogniser; post-Phase-3, when
  Boehm no longer scans the Sigil-thread stack, the harness is
  the only structure verifying the precise walker matches the
  conservative-equivalent answer. Tasks 11/12 must explicitly
  decide:
  1. Whether the cross-check stays in production (it's already
     `SIGIL_GC_CROSS_CHECK=1`-gated, so leaving it as-is is
     option (a) — opt-in for debug runs).
  2. Whether running it requires Boehm to see the Sigil-thread
     frames (probably yes, otherwise the stackmap-derived
     addresses can't be cross-validated against Boehm's view of
     the stack). If yes, the harness needs to wrap its call site
     in `GC_call_with_gc_active` so the active-state stack scan
     re-engages briefly.

  No design commitment here — Task 11's first iteration must
  pick one of (1)/(2) and document the rationale.

## Stability

- **`GC_set_push_other_roots` / `GC_get_push_other_roots` are
  not internally synchronised.** Per `gc_mark.h:309`: *"Note that
  both the setter and getter require some external synchronization
  to avoid data race."* Sigil's mitigation: install the callback
  exactly once, at runtime init, BEFORE any worker thread spawns.
  Task 11 must document the discipline at the install site;
  re-installation from a runtime worker thread (e.g., a future
  profile-data hook) would race against the marker reading the
  proc pointer during STW.
- libgc 8.x exposes `GC_do_blocking`, `GC_call_with_gc_active`,
  and `GC_set_push_other_roots` as documented stable surface.
  No deprecation path through 8.x; these APIs trace back to
  libgc 6.x.
- The Ubuntu `libgc-dev` package and Homebrew `bdw-gc` both
  ship these symbols on every release we target.
- Risk of API rename across the plan window: low.

## Verification

A minimal end-to-end test of the push-other-roots mechanism
lives at `runtime/tests/boehm_per_thread_roots_spike.rs`:

1. Register a process-wide
   `GC_push_other_roots_proc` that pushes a thread-local
   "shadow root" word (containing a heap pointer alias) via
   `GC_push_all_eager`.
2. Allocate a target string; capture its address into the
   shadow root; bury the typed pointer (matches the Phase 2
   false-retention reproducer's `#[inline(never)]` discipline).
3. Force `GC_gcollect()`.
4. Assert: the target survives. If the push_other_roots
   callback isn't called, or if the pushed range isn't honoured
   as a root, the finalizer registered on the target fires and
   the assertion trips.

Both Sigil-side acceptances mirror the Phase 2 Task 6 spike
shape:
- The API works on the pod + on both CI lanes.
- The doc names the mechanism Tasks 11 + 12 will use.

## Decision summary

**Adopted approach for Phase 3:**

1. **Sigil program threads** call `GC_do_blocking(sigil_run_loop_inner,
   ...)` to enter the inactive state. While inside, the precise
   stackmap walker (registered via `GC_set_push_other_roots`)
   supplies root locations to the marker.

2. **Runtime-internal threads** (profile drainer, future
   runtime workers) are NOT wrapped in `GC_do_blocking` — their
   stacks continue to be scanned conservatively by Boehm's
   default mechanism.

3. **The discriminator is the runtime's call shape**, not a
   Boehm API parameter. Task 11 implements this as a one-line
   change to `sigil_run_loop`'s entry + a thread-registry hook;
   Task 12 verifies the full suite still passes + the precise
   walker is exercised under `GC_gcollect` pressure.

**No escalation needed.** Every API is documented stable on
libgc 8.x and exposed on both target hosts.

## Task 11 follow-up findings (PR #170)

Task 11's PR #170 attempted to wire the discriminator into
production (sigil_gc_init calls
`register_sigil_thread_for_precise_roots`; drainer calls
`register_runtime_thread_for_conservative_roots`). The wiring
went through five CI rounds before landing — each round
diagnosed a Boehm 8.x interaction constraint the Task 10 spike
didn't catch. Task 12's implementer needs these findings up
front:

### Finding 1: `GC_allow_register_threads` switches Boehm to parallel-marker mode

Documented in `gc.h:1551`: *"Includes a `GC_start_mark_threads()`
call."* Easily missed — the spike treated allow_register_threads
as a benign "enable thread enrolment" call, but it has a
load-bearing side effect: it spawns parallel marker threads
that change Boehm's marker semantics process-wide.

**Empirical failure:** PR #170 commit `e30d6ef` called
`GC_allow_register_threads` from `register_sigil_thread_for_precise_roots`
on the main thread (per the previous review's B1 prescription).
On the next CI run, 7 e2e tests failed:

- `tree_example_prints_32767_under_500ms` returned the wrong
  sum (`6749` instead of `32767`) — live tree nodes collected
  as garbage.
- `cpu_profile_writes_folded_for_txt_extension` +
  `cpu_profile_writes_pprof_when_env_set` had their compiled
  binaries crash with empty stdout/stderr.
- `std_list_sort_int_ten_thousand_reversed`,
  `std_map_ten_thousand_inserts_then_lookups`,
  `task_12_validation_profile_json_sigil_end_to_end`
  similarly failed.

The switch to parallel markers broke the marker for a
previously-single-threaded user program. The Task 12 work
needs to characterise this empirically before reinstating
`GC_allow_register_threads`. Options: keep single-marker
mode (call `GC_set_markers_count(1)` early?), figure out why
parallel markers misbehave on Sigil's alloc pattern, or
defer enrolment entirely until multi-Sigil-threading lands.

### Finding 2: `walk_for_gc` SIGSEGVs from inside libgc's mark phase

The Phase 1 stackmap walker reads `current_caller_fp()` via
inline asm + walks the chain via `*fp` reads (`walk_frame`).
That works fine when the walker is called from Rust code with
conventional FP-saving prologues (e.g., cross_check_xchk's
`sigil_alloc`-driven invocation).

**But:** when the walker is invoked from inside Boehm's mark
phase (via a `GC_set_push_other_roots` callback), the call
chain passes through libgc internal frames. libgc 8.x on
both target hosts is built with optimisations that may omit
frame pointers from internal frames — so reading `saved_fp`
from a libgc frame yields garbage, and the next `walk_frame`
deref blows up with SIGSEGV.

**Empirical failure:** PR #170 commit `9a9d7d5` removed
`GC_allow_register_threads` from the production path
(addressing Finding 1) but kept the walker active in the
push_other_roots callback. `tree.sigil` SIGSEGVed (exit `-1`,
empty stdout) — the walker died inside libgc.

**Task 12 fix shape:** the captured-FP mechanism the spike's
"Section 3" describes. `sigil_alloc` wraps its body in
`GC_call_with_gc_active` (or equivalent) and captures the
user-level FP — the top of the Sigil call chain, OUTSIDE
libgc's internal frames — into a thread-local. The
push_other_roots callback reads that captured FP and calls
`walk_for_gc_with_callback_from(captured_fp, …)` (a new
variant Task 12 introduces) — walking from a known-clean
starting point.

### Finding 3: `GC_set_push_other_roots` must chain to the prior proc

Documented in `gc_mark.h`: *"A client supplied procedure
should also call the original procedure."* Easily missed —
the spike's Section 1 doc cited the setter/getter but didn't
spell out the chaining contract.

**Empirical failure:** PR #170 commit `7bd11b3` made the
callback body a no-op (addressing Finding 2 by skipping the
walker entirely). 7 e2e tests still SIGSEGVed:
`tree.sigil`, `sudoku.sigil` (both variants),
`std_list_sort_int_ten_thousand_reversed`,
`std_map_ten_thousand_inserts_then_lookups`,
`multishot_perf_example_under_5s`,
`cross_check_tree_stress_drop_repeat_runs_cleanly`. All
alloc-heavy workloads; all empty stderr.

Diagnosis: setting our own proc REPLACED Boehm's internal
push_other_roots proc, which Boehm uses for its own TLS root
+ dynamic-library root supply hooks. Without chaining, those
roots vanish on every mark phase → live objects collected →
later user-code derefs SIGSEGV.

**Fix shape:** at install time, capture the prior proc via
`GC_get_push_other_roots()` and invoke it from the wrapper
before any custom body. PR #170 commit `a9875a3` lands this
fix; the chaining structure stays through Task 12.

### Implication for Task 12's spike-vs-production split

Each of the three findings cost a CI round on PR #170 to
discover. Task 12's `GC_do_blocking` + captured-FP boundary
is an even more empirical problem (3-dimensional: blocking
correctness, active-state re-entry, walker safety from the
captured FP). A test-only spike PR that exercises each
dimension in isolation BEFORE the production-wiring PR
would amortise the discovery cost. The Task 10 spike
checked `GC_do_blocking`'s link surface + the
`push_other_roots` install path; Task 12 should similarly
exercise the captured-FP walk in test scaffolding before
flipping production behavior.

## Task 12 implementation notes (this PR)

Task 12 ships the production wiring that drops Boehm's
conservative stack scan on Sigil program threads. Three
load-bearing pieces compose the boundary:

1. **`GC_do_blocking` wrapping `sigil_run_loop`.** The public
   entrypoint stages an `RunLoopBlockingCtx` on the stack and
   routes the trampoline body through
   `GC_do_blocking(trampoline, &ctx)`. Boehm transitions the
   thread to "GC-inactive" state for the duration of the loop:
   the conservative stack scan covers only the C-frame range
   ABOVE `sigil_run_loop`'s captured stack base (Rust main
   shim, libc init) — not the Sigil call chain. Nested
   `sigil_run_loop` calls (nested handle expressions) are safe
   because `GC_do_blocking` is stack-disciplined and
   re-entrable.

2. **`GC_call_with_gc_active` wrapping `sigil_alloc`'s
   dispatch.** From inside the blocked region, calling
   `GC_malloc` directly would be undefined behavior per gc.h.
   `sigil_alloc` instead routes the allocator selection
   (`GC_malloc_atomic` / `GC_malloc` / `GC_malloc_explicitly_-
   typed`) through `GC_call_with_gc_active(trampoline, &ctx)`
   so Boehm re-activates GC state for the allocation. The
   counter increments, cross-check hook, allocation-profile
   sample, FP capture, header write, and null check all run
   OUTSIDE the active wrapper — only the actual `GC_malloc_*`
   call needs to be inside.

3. **Captured-FP walker entry-point semantics.** The
   `push_other_roots` callback reads
   `CAPTURED_SIGIL_CALLER_FP` (TLS) and feeds it to
   `walk_for_gc_with_callback_from`. The captured FP must be
   `sigil_alloc`'s own frame pointer — NOT the Sigil caller's
   FP. This matters because the walker iterates UP the chain
   and for each frame `fp` looks up the saved return-PC at
   `*(fp+8)`. With `starting_fp = sigil_alloc_FP`, the first
   iteration's return-PC points at the call site INSIDE the
   Sigil caller — which is exactly where the stackmap has
   entries. Starting one frame higher (Sigil caller's FP)
   would yield the *caller's caller's* records and skip the
   Sigil function's own roots at the alloc site. To get
   sigil_alloc's own FP from inside sigil_alloc, the runtime
   calls `stackmap::capture_caller_fp_for_walk()`: an
   `#[inline(never)]` helper whose own prologue gives it a
   frame; reading rbp inside that frame and dereferencing
   yields the caller's saved FP = sigil_alloc's FP.

4. **`GC_set_markers_count(1)` before `GC_init`.** Pinned to
   single-marker mode. Finding 1 above describes how
   `GC_allow_register_threads` would otherwise auto-spawn
   parallel marker threads via its implicit
   `GC_start_mark_threads` call; the count-1 pin neutralises
   that side effect. Production paths in this PR don't call
   `GC_allow_register_threads` themselves (see "Runtime
   thread enrolment" note below), but the pin stays as
   defense-in-depth: test_support's GC stress harness still
   exercises that call path, and future production work
   (multi-Sigil-thread Plan E3) will want the constraint
   in place before any worker enrols.

**Runtime thread enrolment — closed without action.**
Task 11's deferral list named "runtime-thread Boehm
enrolment in production paths" as a Task 12 item. The
review against actual runtime threads (CPU-profile
drainer, alloc-profile drainer) concluded the item is
moot: neither thread allocates from Boehm or holds
Boehm pointers on its stack, and `std::thread::spawn`
doesn't route through `GC_pthread_create`'s auto-
cleanup hook so an enrolled drainer would leak its
registration on exit — which CI surfaced as a CPU-
profile e2e crash before any Sigil output appeared.
`register_runtime_thread_for_conservative_roots` stays
a no-op on Boehm state.

The four pieces compose to a thread model where:
- Boehm's STW conservatively scans ABOVE the
  `GC_do_blocking` stack base + the `GC_call_with_gc_active`
  re-active window (small C-frame ranges only).
- The precise walker supplies roots for the Sigil call
  chain via stackmap entries discovered along the FP chain
  starting at `sigil_alloc`'s captured FP.
- Chained `push_other_roots` preserves Boehm's internal
  roots (TLS, dl_iterate_phdr) alongside our precise roots.

Compile-time gating: all four pieces are
`#[cfg(not(test))]`-gated. `cargo test` exercises the
runtime in active GC state with the cross-check harness,
which does not depend on the captured FP or the blocking
boundary. End-to-end coverage comes from the
`precise_walker_deep_*` e2e tests in `compiler/tests/e2e.rs`
(deep recursion + GC stress) and the existing
`cross_check_tree_stress_*` suite (alloc volume + GC
pressure).
