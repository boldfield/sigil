# Plan E2 — v2 precise GC + real Cranelift stackmaps

Tracks Plan E2's execution against
`boldfield/designs/in-progress/2026-05-08-sigil-v2-precise-gc.md`
(moves to `done/` on Phase 3 completion). Plan E1 (runtime profile-
data emission surface) merged as PR #148; Plan E2 builds on it and
on PR #151's Cranelift 0.131 stackmap API spike.

Plan E2 has three sequential phases — each ships independently with
its own acceptance gate. Per-task PRs (the cadence default), bundled
or split per task scope.

## Phase 1 — Cranelift stackmaps (real v1 content)

### Task 1 — Cranelift stackmap API spike

- status: **done** (PR #151, squash-merge `86069a7`)
- deliverable: `compiler/docs/cranelift-stackmap-spike.md` +
  `compiler/tests/cranelift_stackmap_spike.rs` (2 integration tests:
  `value_variant_flag_filters_live_set_at_safepoint`,
  `var_variant_emits_stackmap_for_phi_confluence`).
- escalation: none — Cranelift 0.131.0 has every capability Plan E2
  needs. `=0.131.0` exact pin stays.

### Task 2 — Mark GC refs in Sigil codegen

Plan body lists three categories: (1) alloc returns, (2) heap-pointer
loads, (3) "phi confluences" (block-args in sigil since codegen uses
pure SSA + block-args, not Variables). Shipped in two tranches:

- **Task 2a** — Category 1 (alloc returns). status: **done** (PR #156,
  squash-merge `5755e22`). 62 marked sites, verified against
  source-of-truth grep on `runtime/src/**/*.rs` for `pub extern "C"
  fn sigil_* -> *mut u8`. Helper `lower_alloc_call` landed at
  `compiler/src/codegen.rs` (used at one representative site,
  `float_add`); other 61 sites surgical pending Task 2b helper
  refactor.
- **Task 2b** — Categories 2 (heap-pointer loads) + 3 (block-arg
  confluences) + helper rollout to all 62 alloc sites. status:
  **in PR #159 review**. Coverage:
  - **Cat 1 helper rollout**: complete. All 62 alloc sites funnel
    through `lower_alloc_call`. The only `declare_value_needs_stack_map`
    site outside the helper is inside the helper itself.
  - **Cat 2 (heap-pointer loads)**:
    - `lower_closure_env_load_from` (renamed from `lower_closure_env_load`)
      is now the centralised path for closure-env captures from EITHER
      `self.closure_ptr` OR a `synth_closure_ptr` argument.
    - `load_field_value` + `Pattern::Tuple` arm flag by `field_ty` /
      `elem_ty`.
    - 41 named heap-pointer loads flagged (24 in the first sweep + 17
      surfaced by PR #159 review M1).
    - `sigil_ref_deref` result flagged when T is heap-bearing.
    - `lower_heap_pointer_load` helper added — every new heap-pointer
      load must funnel through it. Exercised at one representative
      site; rolling out to existing sites is mechanical follow-up
      (see "Open follow-ups" below).
  - **Cat 3 (block-arg confluences)**:
    - Sync user-fn entry-block user-args flagged via
      `flag_heap_pointer_user_args`.
    - 14 `let closure_ptr = block_params[0]` / `synth_closure_ptr`
      extractions at Cps/synth-fn entries flagged (PR #159 review M2).
      Closure_ptr at block_params[0] is always a heap pointer.
    - 4 high-confidence merge-block params flagged (NextStep merges +
      Option[Char] merges).
  - **Task 3's closure dependency** (fn-entry block-params for
    tail-callable fns) is now satisfied by the Sync user-fn user-args
    + closure_ptr flagging. Re-audit Task 3 after PR #159 lands.

### Task 3 — Annotate safepoints (audit)

- status: **doc-only closure** (PR #157, this PR)
- deliverable: spike doc updated with the audit findings.
- finding: 2 `return_call*` sites (codegen.rs:19987 direct, 20428
  indirect). Cranelift treats both as non-safepoints; ownership of
  live GC refs transfers to the callee. *No annotation needed at
  either site.* Conclusion contingent on Task 2b's fn-entry
  block-param marking — re-audit after Task 2b lands.
- plan-body test ("stackmap section non-empty after a small program
  compile") covered transitively by PR #151's spike tests + PR #156's
  Task 2a marking integration into real codegen.

### Task 4 — Stackmap section v1 writer

- status: **in PR (Task 4 branch)**
- v1 wire format: per-function blocks. Section header (12B) + per-fn
  header (12B: name_len, record_count, text_offset) + name + per-record
  header (12B: pc_offset, frame_size, entry_count, flags) + 5B entries
  (kind:1 + sp_offset:4). Constants in `sigil-abi::stackmap`.
- v0 writer + `push_placeholder` + `function_code_offset` retired
  entirely (120 dead call sites + `Lowerer.stackmap` field removed).
- Writer integration: post-`define_function` reads
  `ctx.compiled_code().unwrap().buffer.user_stack_maps()` via a single
  helper `define_fn_and_capture_stackmap` used at all 10 codegen sites.
- Per-entry type byte: `STACKMAP_ENTRY_KIND_HEAP_POINTER = 0x01`
  (the only kind v1 emits — Phase 2 may add scalar kinds for the
  cross-check). Cranelift's `(ir::Type, sp_offset)` discarded in favour
  of the heap-pointer-only contract (all Sigil-side flags are heap
  pointers via `lower_alloc_call` / `lower_heap_pointer_load`).
- Runtime parser `runtime/src/stackmap.rs` updated to v1; v0 sections
  rejected as stale build artifacts (`UnknownVersion(0)`).
- **G1 verification test lands**: `compiler/tests/e2e.rs` →
  `stackmap_section_parses_v1_with_real_safepoints` compiles
  `examples/choose_demo.sigil`, parses the `__SIGIL,__stackmaps`
  section, asserts ≥1 fn block + ≥1 safepoint record + every entry
  kind is `STACKMAP_ENTRY_KIND_HEAP_POINTER`. Bring-up measurement:
  7 fn blocks, 8 records, 9 total entries.
- **IR-level unit test lands** (per plan-body's "hand-build a known
  IR" spec): `compiler/tests/stackmap_v1_round_trip.rs` →
  `hand_built_alloc_and_call_round_trips_through_v1_section` builds
  a one-fn IR (alloc → flag → consume call → return live ptr) via
  `FunctionBuilder` + `ObjectModule::define_function`, captures
  `user_stack_maps` into `StackMapV1Builder`, serializes, parses
  back via `sigil_runtime::stackmap::parse_section`, and asserts
  exactly one fn block / ≥1 record / ≥1 heap-pointer entry.
- **C-shim handler-frame allocs flagged**: 5 raw
  `builder.ins().call(frame_new_ref, ...)` sites in `emit_main_shim`
  (ArithError / IO / Env / Fs / Process handler frames) +
  `Expr::Handle` lowering's `self.handler_frame_new_ref` call site
  refactored to use `lower_alloc_call` so their results are
  `declare_value_needs_stack_map`'d (PR #156 / #159 missed these
  because they're not at user-code alloc sites). Closes a Task 2c
  residual surfaced by the G1 bring-up.

### Task 5 — Runtime stackmap reader + cross-check

- status: **in PR** (Task 5 branch)
- `runtime/src/stackmap.rs` v1 reader. Section locator: ELF uses
  `dlsym("__start_sigil_stackmaps")` / `dlsym("__stop_sigil_stackmaps")`
  (no extern statics — avoids undef-symbol link error in unit-test
  binaries that don't link compiler-emitted code); Mach-O uses
  `getsectiondata(_dyld_get_image_header(0), "__SIGIL", "__stackmaps",
  ...)`. Per-fn symbol bases resolved via `dlsym(symbol_name)`.
- ELF section name renamed from `.sigil_stackmaps` to `sigil_stackmaps`
  (no leading dot) so the GNU linker auto-generates the start/stop
  symbols (requires valid C-identifier section name).
- Public API: `init_index() -> Option<&'static StackmapIndex>`,
  `StackmapIndex::lookup(pc) -> Option<&ParsedRecord>`, `walk_for_gc()
  -> Vec<RootLocation>`.
- fp-chain walker (`walk_for_gc`) reads x86_64 `rbp` / aarch64 `x29`
  via inline asm, walks the chain to frames whose return-PC matches a
  known safepoint, yields absolute addresses `(fp - frame_size +
  entry.sp_offset)` for every entry.
- Cross-check harness in `runtime/src/stackmap_xcheck.rs`. Activated
  by `SIGIL_GC_CROSS_CHECK=1`; called from `sigil_alloc`. Asserts
  every precise root address lies in `[sp, stack_base)` (B ⊆ A) AND
  the value at the address is heap-pointer-shaped (aligned, ≥ 0x1000).
  Divergence aborts via `std::process::abort` with a stderr
  diagnostic. Production paths skip via cached env-var atomic; cost
  on the fast path is a single relaxed load + branch.
- E2E cross-check tests in `compiler/tests/e2e.rs` (13 total):
  - Hand-picked shapes: `cross_check_hello_runs_cleanly`,
    `cross_check_option_demo_runs_cleanly`,
    `cross_check_choose_demo_runs_cleanly` (multi-shot),
    `cross_check_tree_stress_runs_cleanly` (65,535-node single
    build).
  - Drop-repeat stress (plan body's literal "10k cons cells, drop,
    repeat"): `cross_check_tree_stress_drop_repeat_runs_cleanly`
    runs a new `examples/tree_stress_repeat.sigil` that builds +
    folds + drops a depth-12 tree 10 times (81,910 total
    allocations across 10 build-fold-drop cycles).
  - Broader coverage: `cross_check_arith_runs_cleanly`,
    `cross_check_catch_runs_cleanly`,
    `cross_check_div_recover_runs_cleanly`,
    `cross_check_fib_cps_perf_runs_cleanly`,
    `cross_check_generic_map_runs_cleanly`,
    `cross_check_higher_order_runs_cleanly`,
    `cross_check_nested_effects_runs_cleanly`,
    `cross_check_state_runs_cleanly`.
- Documented in `compiler/docs/stackmap-v1.md`.

## Phase 2 — Precise heap marking

### Task 6 — Boehm precise-mode API spike

- status: **in PR (Task 6 branch)**
- API surface pinned: `GC_make_descriptor(bitmap, len_bits) -> GC_descr` +
  `GC_malloc_explicitly_typed(size_in_bytes, descr) -> *mut c_void`
  (from `gc/gc_typed.h`, libgc 8.x). Heavier alternatives
  (`GC_REGISTER_MARK_PROC`, raw `GC_DESCR_KIND`, `GC_malloc_kind`)
  considered and rejected — `GC_malloc_explicitly_typed` wraps
  kind selection internally and is the right shape for v1's
  bitmap-driven precise marking.
- Repro: `runtime/tests/boehm_precise_spike.rs` (2 integration
  tests). `make_descriptor_returns_nonzero_handle` confirms the
  descriptor constructor returns a non-trivial handle.
  `malloc_explicitly_typed_round_trip` allocates a 2-word object
  via the typed allocator, stores a pointer into its precise
  slot, forces a full `GC_gcollect`, and asserts the slot
  survives unchanged. CI matrix (ubuntu-24.04 + macos-14) is the
  authoritative verification environment.
- Doc: `runtime/docs/boehm-precise-spike.md` documents the chosen
  API, the constraints on `size_in_bytes` (>=
  `len_bits * sizeof(GC_word)`), and the Sigil-pointer-bitmap →
  Boehm-descriptor-bitmap mapping (Sigil's bit-`k` is per-payload-
  word; Boehm's bit-0 is the header word → shift by 1).
- Per-test infra: each spike test runs in its own subprocess via
  the same pattern the runtime's GC stress tests use
  (`SIGIL_BOEHM_SPIKE_INNER` env var, mirrors
  `SIGIL_GC_STRESS_INNER` in `runtime/src/handlers.rs`). This is
  required because libgc 8.x retains stale per-thread mark state
  across cargo-test thread tear-downs — both with and without
  symmetric `GC_unregister_my_thread`, both on the pod and on
  the CI matrix.

### Task 7 — Descriptor cache

- status: **in PR (Task 7 branch)**
- Module: `runtime/src/gc/descriptor.rs` (new, wired via
  `pub(crate) mod descriptor;` in `runtime/src/gc.rs`).
- API: `get_or_create(sigil_bitmap: u32, payload_word_count: u8)
  -> usize` (the Boehm `GC_descr` handle). Cache is process-wide
  `LazyLock<RwLock<BTreeMap<DescriptorKey, GC_descr>>>`; cache
  hits take an `RwLock` read, misses upgrade to write + re-check
  inside the lock so each `(bitmap, count)` enters Boehm's
  descriptor builder at most once across threads.
- Sigil-bitmap → Boehm-bitmap conversion is centralised here:
  `boehm_bitmap = (sigil_bitmap as usize) << 1`,
  `len_bits = 1 + payload_word_count`. Bit 0 of Boehm's bitmap
  is always 0 (header word is never a pointer).
- Deviation from plan: key is `(bitmap, payload_word_count)`,
  not `bitmap` alone. Two objects with the same bitmap but
  different payload counts produce different descriptors —
  Boehm encodes object size in the descriptor word, so the
  handle differs even though the bitmap bits do not. The unit
  test `same_bitmap_different_count_are_distinct_keys`
  documents and pins this.
- `GC_make_descriptor` extern declaration promoted from the
  spike's local `#[link]` block into the production
  `runtime/src/gc.rs` extern block (still `pub(crate)`).
- Tests (`runtime/src/gc/descriptor.rs::tests`): 6 cases —
  identical bitmaps return identical handles; distinct
  bitmaps return distinct handles; same bitmap + different
  count are distinct keys; 10k identical lookups yield 1 cache
  entry (the plan's stress assertion); `bitmap=0` caches
  separately from `bitmap=0b1`; max payload count doesn't
  overflow the single-word bitmap buffer.
- `get_or_create` is `#![allow(dead_code)]`-suppressed at
  module level — caller (`sigil_alloc`) lands in Task 8, which
  removes the suppression.

### Task 8 — `sigil_alloc` registers precise descriptors

- status: **in PR (Task 8 branch)**
- `runtime/src/gc.rs::sigil_alloc` now routes non-zero-bitmap
  allocations through `descriptor::get_or_create` (Task 7's cache)
  and calls `GC_malloc_explicitly_typed(total, descr)`. Bitmap=0
  objects continue to use `GC_malloc_atomic` (strictly better than
  precise marking when no slot can hold a pointer).
- Plain `GC_malloc` is retired from the runtime's extern block —
  no production caller remains. The spike test (`runtime/tests/
  boehm_precise_spike.rs`) keeps its own local `GC_malloc` declaration
  for its baseline-target allocation; that's unchanged.
- `GC_malloc_explicitly_typed` extern declaration added to
  `runtime/src/gc.rs`'s `#[link(name = "gc")]` block.
- `#![allow(dead_code)]` removed from `runtime/src/gc/descriptor.rs`
  (the cache is now reachable from production via `sigil_alloc`).
- Debug-only `total >= (1 + count) * 8` assertion in `sigil_alloc`
  pins the descriptor's bitmap-coverage requirement against drift
  between codegen's `payload_bytes` and Header's `count`.
- New unit test `sigil_alloc_routes_nonzero_bitmap_through_descriptor_cache`
  asserts (a) `bitmap=0` doesn't populate the cache (atomic path),
  (b) non-zero bitmap populates with one entry, (c) repeat-shape
  alloc reuses the entry, (d) distinct shape adds a fresh entry.
- The plan's "60-second allocation-churn workload" stress is
  covered by the existing runtime/compiler test suite running with
  the new path active — every non-atomic Sigil object now goes
  through `GC_malloc_explicitly_typed`. CI's `cargo test --workspace`
  (which compiles the whole runtime and exercises e2e examples)
  is the load-bearing stress.

### Task 9 — Drop conservative heap scan

- status: **in PR (Task 9 branch)**
- "Drop conservative heap scan" was already structurally complete
  in Task 8: every `bitmap != 0 && count > 0` object now routes
  through `GC_malloc_explicitly_typed`, so Boehm's mark phase
  scans only the payload slots the descriptor names as pointers.
  The plan-body concern — conservative heap scan pinning
  unrelated objects via spurious pointer-shaped values — is no
  longer possible for typed-marked objects.
- The remaining conservative paths are intentional:
  - `bitmap != 0 && count == 0` (arrays / mut-arrays / SB segments
    table) → plain `GC_malloc`. These payloads exceed the 6-bit
    count field's reach; conservative scan is the v1 default
    (v2 typed-walker via `TAG_EXTERNAL_DESCRIPTOR` will reshape).
  - Conservative stack scan — Phase 3 lifts this for Sigil
    program threads (Tasks 10–12).
- **False-retention reproducer** (plan body's "single most
  important Phase 2 verification") landed as two paired tests
  in `runtime/src/gc.rs::tests`:
  - `false_retention_reproducer_precise_marker_drops_aliased_address`
    — the actual ship-gate test. Uses a typed-malloc shape
    (`TAG_CLOSURE, count=2, bitmap=0b10`). Writes the target's
    address into payload word 0 (the bitmap-bit-CLEAR slot) and
    null into word 1 (the bitmap-bit-SET slot). Asserts that
    Boehm's precise marker skips word 0 per the descriptor and
    target is collected. **Verified by temporarily reverting
    the typed-malloc dispatch to plain `GC_malloc` (conservative
    full-payload scan) — under that regression the test fails
    with `FINALIZER_FIRED = 0`, proving the test discriminates
    pre-Task-8 from post-Task-8 dispatch.**
  - `atomic_payload_not_scanned_by_conservative_marker` — the
    sanity check (PR #167 review M1 split). Uses `bitmap=0`
    (atomic alloc). Pre- AND post-Task-8 both pass this test;
    it pins `GC_malloc_atomic`'s payload-non-scanning property
    + the `#[inline(never)] unsafe fn` stack-frame-pop
    discipline + the two-GC-cycle + invoke_finalizers pattern.
  Both tests run in a subprocess (matches the runtime's other
  GC stress tests' `SIGIL_GC_STRESS_INNER` discipline).
- 309 runtime lib tests pass (was 307 + new differential test +
  atomic sanity test).
- Throughput delta — ✅ documented in
  [`compiler/docs/plan-e2-phase-2-throughput.md`](compiler/docs/plan-e2-phase-2-throughput.md).
  TL;DR: descriptor_cache_stress (5M allocs) +21% wall-clock on
  ubuntu / +86% on macos (≈ 6–12ns/alloc from the descriptor
  cache + typed-malloc path). Mark-phase delta unmeasured —
  no workload triggered a full GC. Existing perf-floor
  workloads stay below `/usr/bin/time`'s ~10ms precision floor
  and pass their CI gates with the same headroom as pre-Phase-2.
  Re-run via
  [`.github/workflows/throughput-report.yml`](.github/workflows/throughput-report.yml)
  (manual `workflow_dispatch`).
- **Static-descriptor-table follow-up (2026-05-15) — ✅ Confirmed
  (ubuntu) / directionally confirmed within noise (macos).**
  Replaces the runtime `RwLock<BTreeMap<(u32, u8), GC_descr>>`
  descriptor cache with a compile-time-emitted shape table the
  runtime materialises once at startup via `sigil_init_shapes`.
  Every `sigil_alloc` / `sigil_handler_frame_new` call site passes
  a u32 `descriptor_index`; the runtime indexes into a static
  `Vec<GC_descr>`. ~150 lines of cache code removed
  (`runtime/src/gc/descriptor.rs` deleted).
  **Measurement (apples-to-apples baseline `pre_sha=b1ff665` =
  pre-PR-#178 HEAD):** `descriptor_cache_stress` ubuntu −80 ms /
  −30.8%; macos −20 ms / −11.1% (within IQR);
  `tree_stress_repeat_large` ubuntu −20% / macos −25%; other
  workloads flat. Phase 2's +21% / +86% regression is more than
  fully recovered on ubuntu. Doc:
  [`compiler/docs/plan-e2-phase-2-static-descriptor-table.md`](compiler/docs/plan-e2-phase-2-static-descriptor-table.md).
  Lesson recorded in the doc: `pre_sha` must include every
  per-alloc / per-GC overhead change between the named regression
  and the post commit, not just the phase boundary that motivated
  the work — Run A against `4f7ec86` was confounded by Phase 3's
  per-alloc `SigilCallerFpGuard::capture()`.

## Phase 3 — Precise stack roots

### Task 10 — Per-thread root config spike

- status: **in PR (Task 10 branch)**
- API question answered: `GC_register_my_thread` takes a
  `const struct GC_stack_base *` and nothing else; the
  precise / conservative distinction is **global** in libgc 8.x,
  not per-thread.
- Workaround the plan body anticipated is what Boehm itself
  documents (`gc.h` line 1620):
  `GC_do_blocking` + `GC_call_with_gc_active` + `GC_set_push_other_roots`.
  Sigil program threads will be wrapped in `GC_do_blocking`
  (excludes their frames from conservative scan); runtime-
  internal threads stay un-wrapped (Boehm's default scan).
  The stackmap-driven precise walker hooks
  `GC_set_push_other_roots` to supply roots to the marker.
- macOS aarch64: no new quirks — `GC_DARWIN_THREADS` uses
  Mach `task_threads` for suspension, `pthread_get_stackaddr_np`
  for stack-bottom detection; both already validated by Plan B
  Task 56 + Plan E2 Phase 1 on the macos-14 CI lane.
- Spike doc: `runtime/docs/boehm-per-thread-roots-spike.md`.
- Spike test: `runtime/tests/boehm_per_thread_roots_spike.rs` —
  two integration tests (subprocess-wrapped, matching the Phase
  2 Task 6 discipline):
  - `push_other_roots_callback_is_invoked_during_mark` — asserts
    the registered callback fires from inside `GC_gcollect`.
  - `push_other_roots_getter_round_trips_setter` — asserts the
    getter returns the proc the setter installed (needed by
    Task 11 for chaining into a prior callback).
- No 8.x escalation; every API Phase 3 needs is present and
  exposed on both target hosts.

### Task 11 — Thread registration discriminator

- status: **merged (PR #170, commit c0b835f)**
- New module `runtime/src/gc/threads.rs` exposes the
  discriminator API:
  - `register_sigil_thread_for_precise_roots()` — sets a
    thread-local marker (`IS_SIGIL_THREAD`); installs the
    push_other_roots callback (Once-gated); pre-warms the
    stackmap module's lazy initialisers so they never run
    inside STW. Does NOT call `GC_register_my_thread` (per
    `gc.h:1561` "should never be called from the main
    thread, where it is always done implicitly"; Sigil today
    is single-Sigil-threaded, running on the main thread).
  - `register_runtime_thread_for_conservative_roots()` —
    pre-warms the same process-wide state; sets no flag.
    Does NOT call `GC_register_my_thread` either (the
    drainer doesn't allocate from Boehm; the docs-required
    `GC_allow_register_threads` precondition has a side
    effect of starting parallel marker threads, which PR
    #170 CI surfaced as breaking the marker for single-
    threaded user programs). Task 12 reintroduces the
    enrolment when the empirical work to characterise the
    parallel-marker interaction lands.
- **`push_sigil_thread_precise_roots` callback is installed
  but its body is a no-op.** PR #170 CI surfaced a second
  failure mode: calling `stackmap::walk_for_gc_with_callback`
  from inside the mark phase SIGSEGVs alloc-heavy workloads
  (`tree.sigil` exit -1). Root cause: the walker reads
  `current_caller_fp` and walks the chain via `*fp` reads;
  when invoked from inside Boehm's mark phase, the call
  chain passes through libgc internal frames that may be
  compiled with `-fomit-frame-pointer`, so reading saved_fp
  from those frames yields garbage and `walk_frame`
  dereferences invalid memory. The Phase 3 design's
  `GC_do_blocking` + `GC_call_with_gc_active` boundary
  resolves this by capturing the user-level FP at the
  active-state boundary (where Sigil-emitted frames are
  still on top, with conventional FP layout); the callback
  walks from THAT captured FP rather than `current_caller_fp`
  (somewhere inside libgc when the callback fires). Task 12
  ships the captured-FP mechanism + the callback body that
  uses it.
- `GC_set_push_other_roots(push_sigil_thread_precise_roots)` is
  installed at the first registration call (`Once`-gated;
  satisfies `gc_mark.h:309`'s "external synchronization
  required" precondition by the install-once-before-workers
  discipline the Task 10 spike doc names).
- Callback `push_sigil_thread_precise_roots` (runs once per mark
  phase): if the calling thread's `IS_SIGIL_THREAD` is true,
  walks via `stackmap::walk_for_gc()` (Plan E2 Phase 1 walker)
  and pushes each precise root location as an 8-byte range
  via `GC_push_all_eager`. For non-Sigil threads → no-op.
- Wiring:
  - `runtime/src/gc.rs::sigil_gc_init` calls
    `register_sigil_thread_for_precise_roots()` after the
    existing handler/arena root setup (under `cfg(not(test))`).
  - `runtime/src/profile/cpu.rs::drainer_loop` calls
    `register_runtime_thread_for_conservative_roots()` at
    function entry (the "one line at the spawn site" the plan
    body specified).
- **No behaviour change yet.** Boehm's conservative stack scan
  still runs for Sigil threads; the precise walker pushes
  *additional* roots that Boehm already finds via auto-scan.
  Task 12 disables conservative scan for Sigil threads, at
  which point the walker becomes the load-bearing root supply.
- Open question deferred from Task 10 (whether `sigil_alloc`
  needs `GC_call_with_gc_active` wrapping when called from a
  blocked Sigil thread) is still deferred — Task 12 picks
  empirically when it introduces the `GC_do_blocking` wrapping.
- Tests (`runtime/src/gc/threads.rs::tests`): 3 cases — fresh
  thread starts with `IS_SIGIL_THREAD=false`; runtime
  registration leaves the flag `false`; Sigil registration
  sets it `true`. Plus 312/312 existing runtime lib tests
  still pass (no regression from the new wiring).

### Task 12 — Drop conservative stack scan on Sigil threads

- status: **in PR (Task 12 branch — this commit)** — Phase 3
  ship gate.
- Scope expanded per user instruction "no deferrals": Task 12
  ships its original spec plus the items Task 10/11 deferred to
  it (captured-FP mechanism, `GC_do_blocking` wrap,
  `GC_call_with_gc_active` wrap, parallel-marker mitigation,
  walker safety from captured FP, deep-recursion stress tests).
  The Task-11-deferred "Boehm thread enrolment in production
  paths" item was reviewed against the actual runtime threads
  (CPU / alloc profile drainers) and determined NOT needed —
  see step 5 below for the closure rationale.
- **Production wiring** (all `cfg(not(test))`-gated):
  1. `sigil_gc_init` calls `GC_set_markers_count(1)` BEFORE
     `GC_init`, pinning Boehm to single-marker mode. PR #170
     surfaced that `GC_allow_register_threads`'s implicit
     `GC_start_mark_threads` would otherwise spawn parallel
     markers and break alloc-heavy workloads. The pin keeps
     marker semantics single-threaded.
  2. `sigil_run_loop` wraps its trampoline body in
     `GC_do_blocking(trampoline, &ctx)` so the Sigil call
     chain is "GC-inactive" — Boehm's conservative stack scan
     covers only frames ABOVE `sigil_run_loop`. Stack-disciplined,
     so nested run_loops (nested handle expressions) compose
     correctly.
  3. `sigil_alloc` wraps the allocator dispatch
     (`GC_malloc_atomic` / `GC_malloc` / `GC_malloc_explicitly_-
     typed`) in `GC_call_with_gc_active(trampoline, &ctx)` so
     GC routines can run from inside the blocked region.
     Counter increments, cross-check hook, profile sample,
     header write, and null check remain outside.
  4. `sigil_alloc` captures its OWN frame pointer (via
     `stackmap::capture_caller_fp_for_walk`, an
     `#[inline(never)]` helper) into TLS at entry; a Drop
     guard clears it at every exit path. The captured FP is
     `sigil_alloc`'s frame — not the Sigil caller's — because
     the walker iterates UP and reads each frame's saved
     return-PC; with `starting_fp = sigil_alloc_FP`, the
     first iteration's return-PC points INTO the Sigil
     function at the alloc call site, which is where the
     stackmap entries are. Starting one frame higher
     (Sigil caller's FP) would yield the caller's caller's
     records and miss the Sigil function's own roots.
  5. `register_runtime_thread_for_conservative_roots` stays
     a no-op on Boehm state (install_push_other_roots_once +
     stackmap prewarm). Surveyed runtime threads (CPU /
     alloc profile drainers) neither allocate from Boehm
     nor hold Boehm pointers on their stack — they shuffle
     POD `Sample` structs between a lock-free SPSC ring and
     a `Vec<Sample>` on system malloc. Enrolment would
     additionally leak on thread exit (std::thread::spawn
     doesn't route through `GC_pthread_create`'s auto-
     cleanup hook); CI surfaced this empirically as a
     CPU-profile e2e crash before any Sigil output.
  6. The `push_sigil_thread_precise_roots` callback body
     (no-op in Task 11) now reads `CAPTURED_SIGIL_CALLER_FP`,
     gates on `IS_SIGIL_THREAD`, and invokes
     `walk_for_gc_with_callback_from(captured_fp, ...)`. Per
     yielded root, it pushes an 8-byte range via
     `GC_push_all_eager`. Chains to the prior
     push_other_roots proc (preserving Boehm's TLS / dl roots).
  7. SIGPROF unwinder hardening: `profile::cpu::maybe_init`
     primes `profile::unwind::SAFE_STACK_{LO,HI}` from
     `stackmap_xcheck::thread_stack_bounds()` BEFORE arming
     SIGPROF (pthread queries are not async-signal-safe, so
     the cache must be populated off the signal path). The
     unwinder validates every `fp` against the cached range
     before deref, and bails on hops > 4 MB. Defensive: had
     the empty drainer enrolment not been the root cause of
     the CI crash, libgc's `-fomit-frame-pointer` internals
     could still leak wild rbp values into ucontext when
     SIGPROF interrupts during a GC mark phase post-Task 12.
- **Walker variant** —
  `stackmap::walk_for_gc_with_callback_from(starting_fp, f)`
  takes a starting FP rather than reading
  `current_caller_fp()`. The previous variant (used by the
  cross-check) is now a thin wrapper that calls into the new
  variant with `current_caller_fp()`.
- **Tests**:
  - 3 new e2e tests in `compiler/tests/e2e.rs`. Each uses a
    non-TCO `build_nontco(n) -> C(n, build_nontco(n - 1))` shape
    so all 1000 frames remain on the C stack during allocation
    (the constructor wrapper defeats TCO; sum_list is also
    non-tail by construction).
    - `precise_walker_deep_build_sum_chain` — 1000-deep
      `build_nontco` + 1000-deep `sum_list`, asserts sum =
      500_500.
    - `precise_walker_deep_chain_with_gc_pressure` — 5 rounds ×
      1000-deep `build_nontco`/`sum_list`, asserts sum =
      2_502_500.
    - `precise_walker_deep_chain_under_cross_check` —
      `SIGIL_GC_CROSS_CHECK=1` on a 1000-deep chain (the
      cross-check fires at every alloc, so the walker is
      exercised at every depth from 1000 down to 1).
  - Existing `cross_check_tree_stress_*` suite (alloc
    volume + GC pressure) regresses against any walker bug.
  - All existing runtime lib tests + 312/312 pass.
- Spike doc (`runtime/docs/boehm-per-thread-roots-spike.md`)
  updated with "Task 12 implementation notes" section
  documenting the four-piece composition.
- Throughput delta — ✅ documented in
  [`compiler/docs/plan-e2-phase-3-throughput.md`](compiler/docs/plan-e2-phase-3-throughput.md).
  TL;DR: descriptor_cache_stress (5M allocs) +23.5% wall-clock
  on ubuntu (170→210 ms) / +25.0% on macos (120→150 ms) =
  ~6–8 ns/alloc from Phase 3's FP-capture + GC_call_with_gc_active
  wrap. Combined with Phase 2's +21%/+86% (descriptor cache
  + typed-malloc), Plan E2's full per-alloc cost is ~14 ns
  ubuntu / ~18 ns macos. **Mark-phase delta unmeasured —
  `boehm_gc_time_ms = 0` on every workload × every checkpoint,
  INCLUDING the workload (`deep_sync_call_chain`,
  200×2000-deep) designed to trigger many GCs.** This is a
  stronger statement than Phase 2's "unmeasured at this
  scale" — Phase 3's hypothesis (dropping conservative stack
  scan reduces mark time) is unfalsifiable at any practical
  Sigil workload size today, pending a follow-up plan that
  forces frequent collections via `GC_set_max_heap_size` or
  exposing `GC_gcollect()` to Sigil. Existing perf-floor
  workloads (`fib_perf`, `fib_cps_perf`, `tree`,
  `tree_stress_repeat`) stay below `/usr/bin/time`'s ~10ms
  precision floor and pass their CI gates with the same
  headroom as pre-Phase-3. Workflow run:
  [`25870490129`](https://github.com/boldfield/sigil/actions/runs/25870490129).
  Re-run via
  [`.github/workflows/throughput-report.yml`](.github/workflows/throughput-report.yml).
- Mark-phase hypothesis verdict (closed) — ✅ **Disproven.**
  Phase 3 has no measurable mark-phase savings even under forced
  `SIGIL_FORCE_GC_EVERY_N_ALLOCS=1000` injection. Workflow run
  [`25903666139`](https://github.com/boldfield/sigil/actions/runs/25903666139)
  on `3175c83` pre-side patched in via
  `scripts/pre-checkpoint-cadence-patch.diff`. `boehm_gc_time_ms
  = 0` on every workload × every checkpoint × every OS — same
  pattern as PR #176's forced-budget run — but this run verifies
  the injection mechanism fired the expected count exactly
  (post-side `SIGIL_COUNTER_FORCED_GC_COUNT` = `alloc_count ÷
  1000` to the unit on every workload; e.g.,
  `descriptor_cache_stress` fired exactly 5000 forced GCs across
  5M allocs). The savings column is structurally zero at OS-
  level ms resolution. The runtime-internal `GC_gcollect()`
  injection delivered the savings-side measurement that
  `SIGIL_MAX_HEAP_SIZE_KB` couldn't (Boehm's heap-size pin is a
  hard ceiling, not a cadence knob); the verdict is the same
  both ways. Phase 3 remains load-bearing for **correctness**
  (false-retention closure, PR #155/#171), not for
  **throughput**. See the "Force-injection follow-up" section of
  [`compiler/docs/plan-e2-phase-3-gc-time-followup.md`](compiler/docs/plan-e2-phase-3-gc-time-followup.md)
  for full tables + decomposition.
- Alloc-trampoline-elision follow-up (2026-05-17) — ✅ **Tasks
  1–6 complete; Task 7 (default-on flip) ready pending merge
  authorization.** PR #178's throughput attribution split the
  ~10–25 ns/alloc Phase 3 overhead into (1) the per-alloc
  `GC_call_with_gc_active` trampoline wrap and (2)
  `SigilCallerFpGuard` capture/drop. (1) is conditionally
  elidable when the thread is already in GC-active state. The
  follow-up plan
  (`done/2026-05-15-sigil-alloc-trampoline-elision.md`) shipped
  the elision behind `SIGIL_ALLOC_ELIDE_WRAP=1`: TLS shadow
  `IS_THREAD_GC_BLOCKING` maintained by `GcBlockingGuard`
  (save/restore semantics for nested `sigil_run_loop` re-entry);
  fast path in `alloc_dispatch_active` short-circuits the env-gate
  first so default-off processes pay zero per-alloc cost;
  `verify_active()` debug-only sanity check; diagnostic counter
  `SIGIL_COUNTER_ALLOC_WRAP_ELIDED_COUNT`.
  **Task 5** (cross-check siblings, PR #182 then tightened in
  PR #183) — CI-green continuously since 2026-05-15. **Task 6**
  (throughput-report workflow_dispatch with `alloc_elide_wrap=1`,
  `pre_sha=1d19f96`, `runs=5`,
  [`run 25981669373`](https://github.com/boldfield/sigil/actions/runs/25981669373))
  — `descriptor_cache_stress` improves −40 ms / −20.0% on ubuntu
  (passes plan's ≥30 ms threshold) and −20 ms / −18.2% on macOS;
  zero regressions across 7 workloads × 2 OSes; counter-grounded
  proof that elision fires (`elided > 0` post / `null` pre) AND
  both paths fire on `fib_cps_perf` (`elided=8 / alloc_count=21898`
  validates the `GcBlockingGuard` save/restore semantics
  empirically on the only workload that parks via
  `sigil_run_loop`). Verdict doc:
  [`compiler/docs/plan-e2-phase-3-alloc-trampoline-elision.md`](compiler/docs/plan-e2-phase-3-alloc-trampoline-elision.md).
  **Task 7** is unblocked — gate ("SIGIL_GC_CROSS_CHECK suite
  stays green for 24+ hours of CI iterations") satisfied; one-line
  default-flip + counter-assertion-update PR pending authorization.
- Mark-phase hypothesis verdict (follow-up #1: forced budget) —
  ✅ **Inconclusive** even under forced budget (superseded by
  #2's Disproven verdict above). See
  [`compiler/docs/plan-e2-phase-3-gc-time-followup.md`](compiler/docs/plan-e2-phase-3-gc-time-followup.md).
  Adds the `SIGIL_MAX_HEAP_SIZE_KB` env var + the always-on
  `SIGIL_COUNTER_PRECISE_WALKER_NS` counter. Workflow run
  [`25899135194`](https://github.com/boldfield/sigil/actions/runs/25899135194)
  at `SIGIL_MAX_HEAP_SIZE_KB=16384` (the smallest budget every
  workload completes under): every workload × every
  checkpoint × every OS shows `boehm_gc_time_ms = 0` — either
  no STW full GC fired, or each one completed in under 1 ms
  (Boehm's ms-resolved counter rounds sub-ms to 0). The unit
  test confirms the mechanism does fire collections at a
  tighter 1 MiB budget; the 16 MiB workload budget sits in a
  regime where the ms-resolved counter can't separate the two
  readings. The hypothesis remains structurally unfalsifiable
  at this resolution; the savings column is zero by
  measurement. The walker-cost column IS measurable —
  0–625 µs cumulative per workload run (~3.1% relative on the
  largest Cps workload) — so half the decomposition was
  landed. Future measurement would need a debug-build-only
  runtime-internal `GC_gcollect()` injection — a separate
  mechanism from the design's option-(b) rejection (that
  was a language-surface intrinsic, not a runtime debug
  knob).

## Deviations

- **Task 3** — plan-body's "stackmap section non-empty" test was
  covered transitively rather than via a fresh test (see the Task 3
  entry above).

- **Task 4 wire format** — diverges from plan-body's literal spec.
  Plan body says: `record = (PC offset, frame size, live-value
  bitmap, register set)`. Shipped: per-record header
  (`pc_offset:4 | frame_size:4 | entry_count:2 | flags:2`) + variable
  per-entry list (`kind:1 | sp_offset:4`). Rationale: PR #151's
  Cranelift 0.131 spike doc found Cranelift exposes
  `(ir::Type, sp_offset)` per entry and recommended carrying the
  per-entry kind byte instead of a packed bitmap (free from
  Cranelift, useful for Phase 2's bitmap-vs-typecheck cross-check).
  No "register set" field — Cranelift's `UserStackMap` does not
  expose register-resident GC refs; all entries are spilled to the
  frame and addressed via `sp_offset`. Per-function blocks (not in
  plan body) added because the runtime needs to map PC ranges to
  the symbol owning them; without grouping the flat record stream
  cannot be resolved against `dlsym`-style base lookups (Task 5).

- **Task 4 unit-test interpretation** — plan body's "hand-build a
  known IR with one alloc + one fn call; emit; parse the section
  back" lands as an integration test
  (`compiler/tests/stackmap_v1_round_trip.rs`) that exercises the
  ObjectModule → user_stack_maps → StackMapV1Builder → serialize →
  parse_section path. The G1 e2e test
  (`compiler/tests/e2e.rs::stackmap_section_parses_v1_with_real_safepoints`)
  covers the same wire-format contract through the full
  `emit_object` + object-file path on a real Sigil program
  (`choose_demo.sigil`). Two layers of coverage on the same shape.

- **Task 5 cross-check hook point** — plan body says "at every GC";
  shipped as "at every `sigil_alloc`". Rationale: walking Sigil
  thread stacks from inside Boehm's mark callback (`GC_set_start_-
  callback`) requires synchronisation with Boehm's stopped-world
  state that v1 doesn't expose; hooking `sigil_alloc` is strictly
  more frequent than per-GC and exercises the precise walker
  against every alloc-bearing safepoint. The cost is the env-var
  cached relaxed-atomic load + branch when disabled (production
  default); enabled, it runs the walker at every alloc inside the
  cross-check tests. The literal "at every GC" semantic is
  achievable once Phase 3 lands an in-runtime GC trigger (Task 11);
  Phase 1's "B ⊆ A + value-shape check" assertion is the same
  either way.

- **Task 5 type-match assertion** — plan body point 4: "Assert
  types match expected GC-ref types per typecheck." Shipped as a
  value-shape check (8-byte-aligned, ≥ 0x1000) — the runtime has
  no typecheck information. The shape check is what Boehm itself
  uses for conservative pointer recognition, so it's the runtime
  equivalent of "this address contains something the conservative
  scanner would also follow." Per-entry kind information is
  reserved by the v1 wire format (`STACKMAP_ENTRY_KIND_HEAP_POINTER`
  vs future kinds) — when Phase 2 adds boxed-scalar kinds and a
  cross-check mode that resolves them against typecheck-derived
  expectations, the assertion at point 4 of the plan can become
  the type-match check the plan body originally specified.

- **Task 5 API signatures** — plan body declares
  `pub fn lookup(pc: usize) -> Option<StackMapEntry>;` and
  `pub fn walk_for_gc(thread: &Thread) -> Vec<RootLocation>;` —
  free functions taking a `Thread` value. Sigil v1 has no `Thread`
  newtype (per-thread state lives in TLS); `StackMapEntry` is not a
  defined type. Shipped:
  `StackmapIndex::lookup(&self, pc) -> Option<&ParsedRecord>` (a
  method on the index) and `walk_for_gc() -> Vec<RootLocation>`
  (no `Thread` arg; implicitly walks the calling thread). The
  semantic surface area is identical; a future Phase 2 or Phase 3
  may introduce `Thread` + `StackMapEntry` newtypes to match the
  plan-body shape literally.

- **Task 5 cross-check breadth** — plan body's "run existing tests
  with `SIGIL_GC_CROSS_CHECK=1`; assert zero divergence" lands as
  12 dedicated `cross_check_*` tests covering: hello, option_demo,
  choose_demo (multi-shot), tree (single-build stress), arith
  (handler frames), catch (raise+catch), div_recover (error
  recovery), fib_cps_perf (CPS-heavy), generic_map (generics),
  higher_order, nested_effects (handler nesting), state, plus
  tree_stress_repeat (10-round drop-repeat). Not every existing
  e2e test runs with the env var set — sudoku / multishot_stress
  are skipped for CI wall-time; interpreter / json have their own
  dedicated tests. Every alloc on every example fires the same
  cross-check path, so the representative subset bounds CI cost
  without losing coverage class.

## Open follow-ups

None — Task 2b's scope is closed in PR #159. Specifically:

- **`lower_heap_pointer_load` helper rollout**: complete. The
  bulk-refactor sweep in PR #159's final commit migrated 40+1 surgical
  heap-pointer load sites to use the helper. Only the helper itself
  contains an internal `declare_value_needs_stack_map(ptr)` call —
  every external surgical pattern is gone. A future contributor
  cannot add an unmarked heap-pointer load via the established
  pattern; the helper is the only path.

- **7 type-aware merge-block params**: complete. Each of codegen.rs's
  7 `append_block_param(*, pointer_ty / result_ty / handler_overall_ty)`
  sites that needed Sigil-Ty threading now uses
  `expr_is_known_heap(arms[0].body, &preview)` (or the body Expr at
  the no-return-arm Handle site) gated by `result_ty == pointer_ty`.
  The four sites with unambiguous heap merges (NextStep / NextStep /
  Option[Char] / Option[Char]) flag unconditionally; the three sites
  where the merge is `arms[0].body`-dependent (lower_match cont in
  Sync + Cps + Cps-match-to-next-step) and the two handler sites
  (return-arm + no-return-arm) gate on the predicate.

  `expr_is_known_heap` is conservative on genuinely ambiguous AST
  shapes (returns `false`): non-ctor `Ident`, `Perform`, `Handle`,
  `Lambda`, `Cast`, `TupleLit`, `Try`. Phase 3 acceptance gating
  re-verifies; if any such ambiguous case becomes load-bearing the
  helper grows additional shapes.

## Open dependencies

- **Task 3 → Task 2b** — Task 3's no-annotation conclusion at
  `return_call*` sites depends on Task 2b flagging fn-entry
  block-params of pointer type on tail-callable fns. **Satisfied
  by PR #159** (`flag_heap_pointer_user_args` for Sync user-fn
  user-args + closure_ptr-at-block_params[0] flagging for Cps/synth
  fns). Re-audit Task 3 after PR #159 lands.
- **Task 4 → G1** — Task 4 lands the v1 section writer + reader path;
  G1's end-to-end verification test ("compile alloc-bearing program,
  assert section has entries") lands with Task 4.
- **Task 11 → Plan E1's drainer spawn site** — already in
  `runtime/src/profile/cpu.rs` since PR #148.
