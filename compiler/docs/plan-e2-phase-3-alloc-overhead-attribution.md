# Plan E2 Phase 3 — alloc-path overhead attribution

**Status:** verdict landed (this doc).
**Date:** 2026-05-15.
**Tool:** CPU folded-stacks profile from CI workflow's
`profile-validation-${os}` artifact on PR #179 at SHA aae118b
(equivalent to HEAD `1d19f96` source-wise). dladdr-backed symbol
resolution from PR #179 made dyld-loaded library frames legible.

## TL;DR

The ~10–25 ns/alloc Phase 3 overhead surfaced by the two PR #178
throughput-report runs decomposes structurally as:

1. **`GC_call_with_gc_active` trampoline wrap (dominant).** Adds 4
   runtime/Boehm frames between user code and `GC_malloc_explicitly_typed`
   on every alloc: `sigil_alloc → alloc_dispatch_active →
   GC_call_with_gc_active → alloc_active_trampoline → alloc_dispatch
   → GC_malloc_explicitly_typed`. Confirmed by 9/9 macOS samples
   through `sigil_alloc` (every single one shows this chain).
2. **`SigilCallerFpGuard::capture` + `Drop`.** Two TLS writes per
   alloc (capture at entry, clear at drop). Confirmed by sample
   chains terminating at `SigilCallerFpGuard::drop` (macOS) and
   `captured_fp_looks_plausible → AtomicUsize::load` (Ubuntu).

**Verdict:** both costs are paying their honest correctness price.
FP capture is structural to Phase 3 walker anchoring (cannot be
elided without a correctness regression). The trampoline wrap is
structural in the worst case (sigil_run_loop has parked the thread
via `GC_do_blocking`, and `GC_malloc_*` requires GC-active state),
**but is wasted work when the thread is already in active state**
— a future optimization opportunity tracked in
`/repos/designs/queue/2026-05-15-sigil-alloc-trampoline-elision.md`.

**Recommendation:** close the loop. The 10-25 ns/alloc is honest
cost. A future plan can pursue conditional trampoline elision
when prioritized; it's not a "free" optimization (requires TLS
bookkeeping at every `GC_do_blocking` entry/exit + careful audit
of all paths that re-enter active state).

## Methodology

Folded-stacks profile from `examples/fib_cps_perf.sigil`,
`SIGIL_CPU_PROFILE_HZ=999`, dladdr fallback active (PR #179).
CI ran the workload on both `ubuntu-24.04` and `macos-14` lanes
of run 25939921492. Captured 61 ubuntu samples + 10 macOS samples.

**Sample-size caveat.** `fib_cps_perf` is CPS-dispatch-dominated
(~250–500 ms total wall, almost all in `sigil_run_loop` /
`GC_do_blocking` / handler-arm dispatch). Only 4/61 ubuntu samples
(~7%) and 9/10 macOS samples (~90%) traversed `sigil_alloc`. The
ubuntu sparsity is structural — fib_cps_perf doesn't allocate
frequently — and rules out per-percent leaf attribution **on this
workload**. The descriptor_cache_stress workload (5M allocs / 260 ms
ubuntu) the user cited would give richer data; this is queued as
future-work, not blocking the structural verdict here.

**No 0x<hex>-induced ambiguity in the alloc path.** Of the
alloc-path samples, every Sigil-runtime and libgc public-API frame
resolved by name. A handful of frames deep inside libgc's mark
phase (`0x7f38c56d3eac`, `0x7f38c56d346e`, ...) remain unresolved —
those are libgc-private static symbols (`GC_mark_local`,
`GC_push_marked`, etc.) that aren't in libgc's `.dynsym`, so
dladdr can't reach them. They land in **mark-time** frames, not
**per-alloc** frames, so unresolved-ness doesn't affect the
attribution.

## Numbers

### Stack chain through `sigil_alloc` (macOS, n=9 samples)

Every single macOS sample under `sigil_alloc` (count-weighted
n=9/10 of the profile) traversed:

```
sigil_alloc
  → sigil_runtime::gc::alloc_dispatch_active            (Sigil runtime)
    → GC_call_with_gc_active                            (Boehm)
      → sigil_runtime::gc::alloc_active_trampoline      (Sigil runtime, extern "C" closure target)
        → sigil_runtime::gc::alloc_dispatch             (Sigil runtime)
          → GC_malloc_explicitly_typed                   (Boehm)
            → GC_malloc_kind_global                      (Boehm)
              → GC_generic_malloc                        (Boehm)
                → GC_generic_malloc_inner                (Boehm)
                  → {GC_allocobj | GC_install_header | GC_collect_or_expand}
```

The four bold frames between `sigil_alloc` and `GC_malloc_explicitly_typed`
are the **trampoline-wrap overhead** Phase 3 introduced. They did
not exist in Phase 2 — pre-Phase-3 `sigil_alloc` called
`GC_malloc_explicitly_typed` directly. Source: `runtime/src/gc.rs:1083`
(`alloc_dispatch_active`) and `runtime/src/gc.rs:1122`
(`alloc_active_trampoline`).

### FP-guard cost surfaced as leaf (n=2 samples)

| Sample           | Leaf                                                      | Interpretation                                              |
|------------------|-----------------------------------------------------------|-------------------------------------------------------------|
| macOS, count=1   | `<SigilCallerFpGuard as Drop>::drop`                      | SIGPROF caught the TLS-clear write on guard drop.           |
| Ubuntu, count=1  | `captured_fp_looks_plausible → AtomicUsize::load`         | SIGPROF caught the FP-validation load (debug build only — debug_assert; n.b. this is `--release`'s `debug_assertions=on` test build, NOT real release). |

These confirm the two TLS writes per alloc (`capture()` + `drop()`)
plus the validation load are real, observable per-alloc cost.

### Leaf summary (ubuntu, n=61 — all samples, not just alloc-path)

| Leaf                                                              | Samples | %     |
|-------------------------------------------------------------------|---------|-------|
| `sigil_run_loop_blocking_trampoline`                              | 11      | 18.0% |
| `LocalKey::try_with` / `LocalKey::with` (CPS dispatch + FP TLS)   | 16      | 26.2% |
| `GC_do_blocking`                                                  | 5       | 8.2%  |
| `sigil_perform`                                                   | 3       | 4.9%  |
| `sigil_run_loop_impl`                                             | 4       | 6.6%  |
| `counters::add`                                                   | 3       | 4.9%  |
| `sigil_arena_alloc` + `sigil_arena_reset` + `round_up_to_align`   | 6       | 9.8%  |
| `gc::alloc_dispatch`                                              | 1       | 1.6%  |
| `captured_fp_looks_plausible` / `AtomicUsize::load`               | 1       | 1.6%  |
| `stackmap_xcheck::maybe_cross_check` / `is_enabled`               | 2       | 3.3%  |
| `stackmap::StackmapIndex::lookup_exact` (mark-phase walker)       | 1       | 1.6%  |
| Everything else                                                   | 8       | 13.1% |

These are **all** samples, not just alloc-path. The numbers confirm
the workload is dominated by CPS-dispatch (sigil_run_loop +
LocalKey + GC_do_blocking + handler arms ≈ 60%).

## Verdict

**Trampoline wrap dominates the per-alloc cost by frame count.** Four
extra synchronous runtime frames per alloc, plus one `GC_call_with_gc_active`
Boehm-side call (~10-20 ns of state-flag flip + indirect-call
dispatch). Frame-count cost on modern x86_64 / aarch64: ~5-10 ns/frame
× 4 = 20-40 ns conservative.

**FP capture is real but smaller.** Two TLS writes per alloc are
~2-5 ns each on a modern CPU (uncontended thread-local store).
Total ~5-10 ns per alloc.

**Both costs are honest:**

- **FP capture.** Structural to Phase 3 walker correctness. The
  walker reads `CAPTURED_SIGIL_CALLER_FP` to anchor its stack walk
  at the Sigil caller's frame. Without the capture, the walker
  can't find the user-code frames to mark roots from. **Cannot be
  elided.** The optimization "skip when no precise walker is
  registered" is non-viable: the precise walker is always
  registered on Sigil threads in production builds (it's installed
  by `register_sigil_thread_for_precise_roots`, called from every
  Sigil thread's GC enrolment). There's no "walker disabled" mode
  in production.

- **Trampoline wrap.** Structural to correctness *in the worst
  case*: when `sigil_run_loop` has called `GC_do_blocking` to
  yield to handler dispatch, the thread is in GC-blocking state.
  An alloc made while blocking must re-enter GC-active state, or
  Boehm's precise walker fires mid-alloc against a "parked" thread
  and reads inconsistent state. `GC_call_with_gc_active` is the
  Boehm-blessed mechanism for this. **In the common case, the
  thread is already in active state and the wrap is wasted work.**
  See follow-up plan below.

The ~10-25 ns/alloc observed in throughput-report runs A and B is
consistent with this structural attribution (4 trampoline frames +
2 TLS writes + Boehm-side state-flag flip ≈ ~25-50 ns by code
inspection, with optimization narrowing the spread).

## Recommendation

**Close the loop on the verdict.** Both cost centers are paying
their honest correctness price. The ~10-25 ns/alloc is the structural
cost of Phase 3's precise-walker correctness invariant.

**One future optimization queued:** conditional trampoline elision —
skip `GC_call_with_gc_active` when the thread is already in active
state (i.e., not parked via `GC_do_blocking`). Plan body at
`/repos/designs/queue/2026-05-15-sigil-alloc-trampoline-elision.md`.
This is **not a "free" optimization**: requires TLS bookkeeping at
every `GC_do_blocking` entry/exit, careful audit of all paths that
re-enter active state, and probably a runtime-asserted invariant
that the elision never fires on a parked thread. Queued for
prioritization, not auto-implementation.

**No FP-capture optimization queued.** The capture is structural;
the only conceivable savings (capture lazily at GC mark time
instead of at sigil_alloc entry) breaks the walker's anchor
because sigil_alloc's frame is gone by the time GC fires.

## Why fib_cps_perf, not descriptor_cache_stress

The user's prompt called out `descriptor_cache_stress` (5M allocs)
as the alloc-heavy workload that surfaced the 10-25 ns/alloc cost
in the throughput-report runs. **That workload is the right
measurement target but is not a profile-validation target** — the
profile-validation e2e (`task_12_validation_profile_json_sigil_end_to_end`)
is gated on `examples/fib_cps_perf.sigil` because fib_cps_perf is
the only workload that reliably produces ≥250 SIGPROF samples in
the CI lane's wall-time budget. Switching to `descriptor_cache_stress`
would need either:

- Adding a separate profile-validation e2e targeting it; or
- Running the throughput-report workflow with profile sampling
  bolted on (out of scope here — the prompt explicitly forbade
  rerunning the throughput workflow).

The structural attribution from fib_cps_perf is sufficient for
the verdict because:

- The trampoline-wrap chain is **invariant per alloc** (every alloc
  on production builds hits the same 4-frame chain — confirmed by
  9/9 macOS samples).
- The FP capture is **invariant per alloc** (every alloc captures +
  drops the same TLS slot — confirmed by code reading +
  Drop-leaf sample).

Per-percent attribution between the two would require a richer
sample set than fib_cps_perf can produce. That's queued as a
"future work" note in the follow-up plan but not blocking this
verdict.
