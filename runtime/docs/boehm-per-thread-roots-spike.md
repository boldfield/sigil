# Boehm per-thread roots — spike findings

## Status: spike complete, Plan E2 Phase 3 Task 10

This document pins the Boehm API surface Phase 3 will use to:
- Mark Sigil program threads as "precise stack roots" (root
  locations supplied by the stackmap-driven walker built in Plan
  E2 Phase 1).
- Keep runtime-internal threads (Plan E1's profile drainer; any
  future runtime-spawned thread) on conservative stack scan.

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

3. **`sigil_alloc` does NOT need to flip back to active.** The
   plan body's "thread registration discriminator" model is
   simpler: while inside `GC_do_blocking`, Boehm marks the
   thread's frames as inactive; allocations from
   `GC_call_with_gc_active(sigil_alloc, ...)` work transparently
   because `GC_malloc_explicitly_typed` doesn't require the
   thread to be active — it just allocates. The precise walker
   provides the roots; the alloc path does its work.

   (Open question that Task 11's spike will resolve: whether
   `sigil_alloc` needs to be wrapped in `GC_call_with_gc_active`
   at all, or whether allocations from a "blocked" thread are
   already permitted by libgc 8.x. The docs imply the latter via
   `GC_do_blocking`'s caveat "the thread is not suspended", which
   reads as "the thread can still call into the runtime".)

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

```rust
// gc.h
extern "C" {
    // Register the calling thread. Stack base auto-detected via
    // GC_get_stack_base(); we already use `GC_register_my_thread(NULL)`
    // in the runtime's test_support.
    pub fn GC_register_my_thread(sb: *const GC_stack_base) -> i32;

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

- How the precise / conservative split interacts with
  `SIGIL_GC_CROSS_CHECK` (the Plan E2 Phase 1 cross-check
  harness). The cross-check walks the same stackmap data but
  asserts every precise root address is heap-pointer-shaped
  per Boehm's conservative recogniser. Post-Phase-3, when
  Boehm no longer scans the Sigil-thread stack, the cross-check
  is the only structure verifying the precise walker matches
  the conservative-equivalent answer. Tasks 11 + 12 should
  ensure the cross-check stays meaningful in this regime —
  likely by running it explicitly via a debug-mode
  `GC_call_with_gc_active` wrapper around the cross-check
  itself, so Boehm sees the same stack frames the precise
  walker reads.

## Stability

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
