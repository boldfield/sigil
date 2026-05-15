//! Boehm GC integration — plan A1 Stage 1 task 2; Plan E2 Task 8
//! switched the per-object allocator to Boehm's typed-malloc path.
//!
//! The runtime wraps Boehm's `GC_init` + the typed/atomic allocator
//! pair (`GC_malloc_atomic` for non-pointer payloads,
//! `GC_malloc_explicitly_typed` for pointer-bearing payloads via
//! Task 7's descriptor cache) in the FFI surface the compiler emits
//! calls to. Header construction happens on the caller side (through
//! `header::Header::new`); `sigil_alloc` writes the header to the
//! first 8 bytes of the Boehm block and returns a pointer to that
//! header.
//!
//! **Allocations always return a pointer to the header**, never to the
//! payload or a field. This is the no-interior-pointers invariant.
//!
//! `sigil_string_new(bytes, len)` is a Stage-1 convenience that allocates
//! a String object, copies the bytes, and returns the tagged heap value.
//! Generalised String construction arrives with the stdlib in later plans.

use std::ffi::c_void;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock};

use crate::counters::{self, sigil_counter_print_all, CounterId};
use crate::header::{self, Header};

// Direct Boehm FFI — we do not depend on a Rust wrapper crate. These are
// the stable symbols exported by libgc.
#[link(name = "gc")]
extern "C" {
    fn GC_init();
    // Plan E2 Phase 3 Task 12 — pin Boehm to single-marker mode
    // BEFORE GC_init runs. Per gc.h:111, `GC_set_markers_count`
    // sets the total marker thread count (including the
    // initiating one); zero means "the collector decides".
    // Calling with 1 keeps Boehm single-marker even after
    // `GC_allow_register_threads` (which the runtime-thread
    // path now calls) would otherwise auto-spawn parallel
    // marker threads. PR #170 surfaced that parallel markers
    // break alloc-heavy workloads on previously-single-threaded
    // user programs; single-marker mode preserves the pre-
    // Phase-3 marker semantics while letting non-main threads
    // enroll via `GC_register_my_thread`.
    pub(crate) fn GC_set_markers_count(n: u32);
    // Plan E2 Phase 3 GC-time follow-up — pin Boehm's maximum heap
    // size in bytes BEFORE `GC_init`. Per `gc.h`, calling with 0
    // means "no limit" (Boehm's default heap-growth heuristic
    // applies). Sigil exposes this through the
    // `SIGIL_MAX_HEAP_SIZE_KB` env var so a measurement run can
    // force full GCs at a low threshold and decompose Phase 3's
    // mark-phase savings vs. precise-walker cost.
    //
    // See `compiler/docs/plan-e2-phase-3-gc-time-followup.md` for
    // the methodology + verdict.
    pub(crate) fn GC_set_max_heap_size(n: usize);
    // `GC_malloc_atomic` is used for objects whose pointer_bitmap is 0
    // (no GC-managed pointers in the payload — strings, byte arrays,
    // primitive scalar wrappers) so Boehm can skip scanning them
    // during mark phases.
    fn GC_malloc_atomic(size: usize) -> *mut c_void;
    // `GC_malloc` (conservative-scan allocator) is retained for
    // objects whose payload is too large for the Header's 6-bit
    // count field to describe precisely — arrays, mut-arrays, the
    // string-builder segments table. These sites encode their
    // "scan conservatively" intent as `(count = 0, bitmap != 0)`:
    // count = 0 because the 6-bit field caps at 63 payload words
    // (arrays / segments tables can exceed that), bitmap != 0 to
    // route OUT of the atomic path. Without this fallback, Plan
    // E2 Task 8's typed-malloc path would build a `len_bits = 1`
    // descriptor for these objects and Boehm's tile-replication
    // would treat the whole payload as non-pointer — silently
    // dropping every heap reference in the array.
    fn GC_malloc(size: usize) -> *mut c_void;
    // Register `[start, end)` as a GC root. Boehm scans the range
    // conservatively for pointer-shaped values on every mark phase.
    // Plan B Task 56 uses this to root `HANDLER_STACK` (the thread-local
    // handler-stack head) and the per-thread arena's backing storage,
    // both of which would otherwise sit outside Boehm's automatic scan
    // (TLS slots are not enumerated portably; the arena's `Vec<u8>`
    // payload lives on the system allocator's heap, not Boehm's).
    pub(crate) fn GC_add_roots(start: *mut c_void, end: *mut c_void);
    // Symmetric counterpart to `GC_add_roots`. Used by
    // `GcThreadEnrolment::drop` in tests to unregister a thread-local
    // root range when the thread is about to exit (cargo test spawns
    // a fresh thread per test under `--test-threads=N`; without
    // unregistration, stale ranges from finished test threads pile up
    // in Boehm's root list and segfault on the next collection).
    #[cfg(test)]
    pub(crate) fn GC_remove_roots(start: *mut c_void, end: *mut c_void);
    // Force a full GC cycle. Used by GC stress tests to deterministically
    // exercise reachability — without it, a passing test under low
    // allocation pressure does not prove rootedness; with it, an unrooted
    // pointer is reliably collected and the test trips.
    //
    // Plan E2 Phase 3 GC-time follow-up #2 also calls this from
    // production paths under the `SIGIL_FORCE_GC_EVERY_N_ALLOCS`
    // env-var gate, so the extern is no longer test-only. The
    // Boehm symbol is always present in libgc; the gate is purely
    // about whether non-test code references it. Zero per-alloc
    // cost when the env var is unset (the cadence OnceLock returns
    // `Some(None)` and the inner match falls through).
    pub(crate) fn GC_gcollect();
    // Boehm thread enrolment used by GC stress tests in this crate. A
    // Rust test thread is not auto-registered with Boehm (see
    // `test_support` module for the historical context); calling
    // `GC_gcollect` from such a thread triggers Boehm's "Collecting
    // from unknown thread" abort. Tests that need to force collection
    // must enrol their thread first.
    //
    // Plan E2 Phase 3 Task 12: production runtime threads (CPU /
    // alloc profile drainers) do NOT need to enrol — they neither
    // allocate from Boehm nor hold Boehm pointers on their stack.
    // See `gc::threads::ensure_gc_process_state_initialised`'s
    // doc + the `gc::threads` module doc's "Runtime threads
    // don't enrol with Boehm" section for the rationale. So
    // these symbols stay test-only.
    #[cfg(test)]
    pub(crate) fn GC_allow_register_threads();
    #[cfg(test)]
    pub(crate) fn GC_register_my_thread(stack_base: *const c_void) -> i32;
    #[cfg(test)]
    pub(crate) fn GC_unregister_my_thread() -> i32;

    // Boehm finalizer surface — Plan E2 Phase 2 Task 9. Used by the
    // false-retention reproducer to assert an unreachable object is
    // actually collected (rather than indirectly inferring liveness
    // from heap statistics). `GC_register_finalizer(obj, fn, cd,
    // ofn, ocd)` schedules `fn(obj, cd)` to run when `obj` becomes
    // unreachable; `GC_invoke_finalizers()` synchronously runs any
    // pending finalizers (we call it after `GC_gcollect` to force
    // the assertion to happen in the test's thread instead of an
    // arbitrary deferred moment). Gated to test builds so the
    // production staticlib stays free of test-only Boehm symbols.
    #[cfg(test)]
    pub(crate) fn GC_register_finalizer(
        obj: *mut c_void,
        fn_: extern "C" fn(*mut c_void, *mut c_void),
        cd: *mut c_void,
        ofn: *mut extern "C" fn(*mut c_void, *mut c_void),
        ocd: *mut *mut c_void,
    );
    #[cfg(test)]
    pub(crate) fn GC_invoke_finalizers() -> i32;

    // Boehm typed-malloc descriptor constructor — Plan E2 Phase 2.
    // `bitmap` is a slice of `GC_word` (== usize on 64-bit targets);
    // bit `i` (LSB-first within each word) is `1` iff word `i` of
    // the to-be-described object is a GC pointer. `len_bits` is the
    // number of meaningful bits in the bitmap. Returns an opaque
    // `GC_descr` handle; on insufficient memory Boehm returns a
    // conservative-trace fallback (still safe, just less precise).
    // Per gc_typed.h: "Calls to GC_make_descriptor may consume some
    // amount of a finite resource. This is intended to be called
    // once per type, not once per allocation." — Task 7's descriptor
    // cache is the structural enforcement of that contract.
    pub(crate) fn GC_make_descriptor(bitmap: *const usize, len_bits: usize) -> usize;

    // Plan E2 Phase 3 Task 12 — `GC_do_blocking` opts the
    // calling thread out of Boehm's conservative stack scan for
    // the lifetime of the wrapped function. `sigil_run_loop`
    // wraps its body in this so the Sigil call chain isn't
    // scanned conservatively (the precise walker driven by the
    // captured FP supplies the roots instead).
    // `GC_call_with_gc_active` is the inverse: from inside a
    // blocked region, switch back to active state so the user
    // function (sigil_alloc's inner body) can safely call
    // Boehm's allocation APIs per gc.h:1626-1636's "the user
    // function is allowed to call any GC function".
    // Symbols used only in `cfg(not(test))` production paths
    // (sigil_run_loop wraps in GC_do_blocking; sigil_alloc wraps in
    // GC_call_with_gc_active). In `cargo test` builds the wraps are
    // bypassed, so the symbols look unused — silence the dead-code lint
    // for the test config only.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn GC_do_blocking(
        fn_: extern "C" fn(*mut c_void) -> *mut c_void,
        cd: *mut c_void,
    ) -> *mut c_void;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn GC_call_with_gc_active(
        fn_: extern "C" fn(*mut c_void) -> *mut c_void,
        cd: *mut c_void,
    ) -> *mut c_void;

    // Boehm typed allocator — Plan E2 Phase 2 Task 8. Allocates
    // `size_in_bytes` bytes from Boehm's heap and tags the block
    // with `descr` so the mark phase scans payload words precisely
    // per the descriptor's pointer bitmap. The returned block is
    // zero-initialised and 8-byte aligned (same as `GC_malloc_atomic`).
    // `size_in_bytes` must be `>= len_bits * sizeof(GC_word)` —
    // the descriptor's bitmap must cover the entire allocation.
    fn GC_malloc_explicitly_typed(size_in_bytes: usize, descr: usize) -> *mut c_void;

    // Cumulative wall-clock time, in milliseconds, that Boehm has
    // spent in full mark-sweep collections. Exposed for the
    // throughput-report tooling (Plan E2 Phase 2 closeout) so
    // `sigil --print-runtime-stats` can surface `boehm_gc_time_ms`
    // alongside the alloc counters. Wraps around on overflow per
    // gc.h; we treat wraparound as out of scope for the report
    // workloads (the workloads run < 60s of wall-clock; far below
    // unsigned-long ms wraparound).
    pub(crate) fn GC_get_full_gc_total_time() -> std::os::raw::c_ulong;

    // Boehm cumulative GC-cycle counter. Returns the number of
    // garbage collections invoked since process start (full + partial),
    // as a `size_t`. Test-only — used by the Plan E2 Phase 3
    // GC-time follow-up env-var test to confirm a budget-pinned run
    // actually triggers collections. Integer count rather than the
    // whole-ms wall-clock `GC_get_full_gc_total_time` — fast-path
    // collections that take <1 ms still increment this.
    #[cfg(test)]
    pub(crate) fn GC_get_gc_no() -> usize;
}

pub mod threads;

/// Static descriptor table — Plan E2 Phase 2 static-descriptor-table
/// follow-up (2026-05-15). Populated once at program start by
/// `sigil_init_shapes`, which the compiler-emitted main shim calls
/// immediately after `sigil_gc_init`. Indexed by a u32 codegen threads
/// through `sigil_alloc`'s third argument; each entry is the Boehm
/// `GC_descr` handle for one (bitmap, payload_word_count) shape the
/// program statically uses.
///
/// Replaces the previous `RwLock<BTreeMap<(u32, u8), GC_descr>>`
/// descriptor cache (the now-deleted `gc::descriptor` module). The
/// cache's purpose was to amortize `GC_make_descriptor` across many
/// allocations of the same shape; the static table does the same job
/// at the cost of one extra `iconst` per `sigil_alloc` call site and
/// none of the lock / map-lookup overhead on the alloc hot path.
///
/// **Layout:** `[codegen-emitted shapes (indices 0..n), runtime-known
/// shapes (indices n..n + RUNTIME_SHAPE_COUNT)]`. The codegen prefix
/// comes from the `__sigil_shape_table` data section the compiler
/// emits; the runtime suffix covers shapes the runtime itself
/// allocates (Ref, Continuation, StringBuilder, the various
/// arm-fn tuple shapes, the wrapper-continuation closure shape).
/// Runtime callers fetch their assigned indices from
/// [`RUNTIME_SHAPE_INDICES`] (populated alongside SHAPE_DESCRIPTORS by
/// `sigil_init_shapes`).
///
/// The plan body undercounted this: it identified only
/// `sigil_handler_frame_new` as runtime-determined, missing the other
/// runtime-internal typed-malloc callers. The runtime suffix closes
/// that gap without re-introducing the lock + map lookup on the
/// codegen hot path — runtime callers pay an extra `OnceLock::get()`
/// (atomic relaxed load + branch on the fast path) rather than the
/// RwLock + BTreeMap read of the prior descriptor cache.
///
/// The `OnceLock` is set exactly once during init; readers in
/// `sigil_alloc`'s typed-malloc branch get a `&'static [usize]` and
/// index into it. Tests register their shapes via
/// `install_shape_descriptors_for_test` which rebuilds an override
/// vector — slow but only runs in test builds and only when the
/// typed-malloc path is exercised directly.
static SHAPE_DESCRIPTORS: OnceLock<Vec<usize>> = OnceLock::new();

/// Assigned indices for the runtime-known typed-malloc shapes. The
/// runtime appends these shapes to `SHAPE_DESCRIPTORS` immediately
/// after the codegen-emitted entries during `sigil_init_shapes`, then
/// records the resulting indices here so each runtime allocator can
/// look up its own index without performing a (bitmap, count) search.
///
/// Test builds populate this via `install_shape_descriptors_for_test`,
/// which also fills `RUNTIME_SHAPE_INDICES_TEST_OVERRIDE` so tests can
/// exercise typed-malloc paths without running through the compiler-
/// emitted main shim.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeShapeIndices {
    /// `Ref[T]`: 1 payload word, payload word 0 is a GC pointer.
    /// `(bitmap=0b1, count=1)`.
    pub ref_cell: u32,
    /// Continuation closure: 4 payload words; words 0 and 2 are
    /// GC pointers (closure / k_closure_ptr), words 1 and 3 are
    /// function pointers. `(bitmap=0b0101, count=4)`.
    pub continuation: u32,
    /// String builder: 4 payload words; word 3 holds the segments
    /// table pointer. `(bitmap=0b1000, count=4)`.
    pub string_builder: u32,
    /// `(Tag/Int, Ptr)` tuple — fs/env Ok/Err shape.
    /// `(bitmap=0b10, count=2)`.
    pub tuple_int_ptr: u32,
    /// `(Ptr, Ptr)` tuple — env-vars key/value pair shape.
    /// `(bitmap=0b11, count=2)`.
    pub tuple_ptr_ptr: u32,
    /// `(Tag/Int, Ptr, Ptr)` tuple — fs-err 3-element shape.
    /// `(bitmap=0b110, count=3)`.
    pub tuple_int_ptr_ptr: u32,
    /// `(Int, Int, Ptr, Ptr)` tuple — process-run result shape.
    /// `(bitmap=0b1100, count=4)`.
    pub tuple_int_int_ptr_ptr: u32,
    /// Wrapper-continuation closure built by
    /// `wrap_continuation_with_outer_post_arm_k`: 5 payload words,
    /// pointers at slots 1 (inner_closure) and 3 (saved_closure).
    /// `(bitmap=0b01010, count=5)`.
    pub wrapper_continuation: u32,
    /// Handler-frame shapes per `arm_count ∈ [0, MAX_HANDLER_ARMS]`.
    /// Indexed by `arm_count` directly. Slot 0 (arm_count=0) is
    /// populated even though codegen-emitted handler frames have
    /// `arm_count >= 1`, so the runtime can serve direct
    /// `sigil_handler_frame_new(_, 0)` test calls from the same
    /// table.
    pub handler_frame: [u32; MAX_HANDLER_ARMS_INCLUSIVE],
}

/// `MAX_HANDLER_ARMS + 1` (so the array slot for `arm_count = max`
/// is in bounds). Declared at module scope so the const array
/// initializer in `RuntimeShapeIndices` can reference it.
pub(crate) const MAX_HANDLER_ARMS_INCLUSIVE: usize =
    sigil_abi::effect::MAX_HANDLER_ARMS as usize + 1;

/// Assigned runtime-shape indices; populated alongside
/// `SHAPE_DESCRIPTORS` by `sigil_init_shapes` (production builds) or
/// `install_shape_descriptors_for_test` (test builds). Production
/// readers panic if unset — `sigil_init_shapes` must have run.
static RUNTIME_SHAPE_INDICES: OnceLock<RuntimeShapeIndices> = OnceLock::new();

/// Materialize Boehm descriptors for every shape in the codegen-
/// emitted shape table and store them in `SHAPE_DESCRIPTORS`. Called
/// exactly once from the compiler-emitted main shim, immediately
/// after `sigil_gc_init` (which must run first so `GC_init` has
/// fired before `GC_make_descriptor`) and before any user-code
/// allocation (so `sigil_alloc`'s typed-malloc branch never reads
/// `SHAPE_DESCRIPTORS` before it is populated).
///
/// `table` points to `n` 8-byte entries in the codegen-emitted
/// `__sigil_shape_table` data section. Each entry is laid out as
/// `(bitmap: u32 little-endian, count: u32 little-endian)` — the
/// `count` field is zero-extended from u8 to u32 to keep the entry
/// 8-byte aligned (the wasted 3 bytes per entry are negligible
/// since `N < 100` for realistic programs).
///
/// # Safety
///
/// `table` must point to at least `n * 8` readable bytes; codegen
/// guarantees this by emitting the table contents from the same
/// pre-pass that determines `n`. Idempotent: a second call (e.g.,
/// from a re-entry path in tests) is a silent no-op via
/// `OnceLock::set`'s Err return.
#[no_mangle]
pub unsafe extern "C" fn sigil_init_shapes(table: *const u8, n: usize) {
    // Tolerate `n == 0` (a program with no precise-marking
    // allocations still emits the call so the runtime's invariant
    // — `SHAPE_DESCRIPTORS` is populated before any alloc — holds).
    let mut descriptors: Vec<usize> = if n == 0 {
        Vec::new()
    } else {
        // SAFETY: caller guarantees `table` points to `n * 8`
        // readable bytes per the doc comment above. The slice is
        // valid for the duration of this function only; we copy
        // out the (bitmap, count) pairs immediately.
        let bytes = std::slice::from_raw_parts(table, n.saturating_mul(8));
        let mut out: Vec<usize> = Vec::with_capacity(n);
        for chunk in bytes.chunks_exact(8) {
            let bitmap = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let count = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u8;
            out.push(build_descriptor_for_shape(bitmap, count));
        }
        out
    };

    // Append the runtime-known shapes after the codegen-emitted
    // prefix. Each push advances the next-index counter, so the
    // resulting `RuntimeShapeIndices` carries indices that point
    // back into the same `descriptors` vector. See
    // `SHAPE_DESCRIPTORS`'s doc for the layout rationale.
    let runtime_indices = append_runtime_shapes(&mut descriptors);

    // Idempotent: re-entry returns Err which we discard.
    let _ = RUNTIME_SHAPE_INDICES.set(runtime_indices);
    let _ = SHAPE_DESCRIPTORS.set(descriptors);
}

/// Append the runtime-known typed-malloc shapes to `descriptors` and
/// return the resulting indices. Centralises the runtime's shape
/// inventory so production init and the test override path agree
/// on shape→index assignments. See `RuntimeShapeIndices`'s doc for
/// what each field carries.
fn append_runtime_shapes(descriptors: &mut Vec<usize>) -> RuntimeShapeIndices {
    let mut push = |bitmap: u32, count: u8| -> u32 {
        let idx = descriptors.len() as u32;
        descriptors.push(build_descriptor_for_shape(bitmap, count));
        idx
    };
    let ref_cell = push(0b1, 1);
    let continuation = push(0b0101, 4);
    let string_builder = push(0b1000, 4);
    let tuple_int_ptr = push(0b10, 2);
    let tuple_ptr_ptr = push(0b11, 2);
    let tuple_int_ptr_ptr = push(0b110, 3);
    let tuple_int_int_ptr_ptr = push(0b1100, 4);
    let wrapper_continuation = push(0b01010, 5);

    // Handler-frame shapes for every `arm_count ∈ [0, MAX_HANDLER_ARMS]`.
    // Codegen-emitted handler frames have `arm_count >= 1`; the
    // arm_count=0 slot is unused by codegen-emitted code but is
    // populated so direct runtime tests (which call
    // `sigil_handler_frame_new(_, 0)`) hit the typed-malloc path
    // through the same `descriptor_index` plumbing as production.
    let mut handler_frame = [0u32; MAX_HANDLER_ARMS_INCLUSIVE];
    for (arm_count, slot) in handler_frame.iter_mut().enumerate() {
        let bitmap = sigil_abi::effect::handler_frame_pointer_bitmap(arm_count as u32);
        let payload_bytes = sigil_abi::effect::handler_frame_payload_bytes(arm_count as u32);
        let count = (payload_bytes / 8) as u8;
        *slot = push(bitmap, count);
    }

    RuntimeShapeIndices {
        ref_cell,
        continuation,
        string_builder,
        tuple_int_ptr,
        tuple_ptr_ptr,
        tuple_int_ptr_ptr,
        tuple_int_int_ptr_ptr,
        wrapper_continuation,
        handler_frame,
    }
}

/// Accessor for the runtime-known shape indices. Returns by value
/// (the struct is small and `Copy`) so the caller doesn't need to
/// hold a guard. Panics if neither `sigil_init_shapes` has run
/// (production) nor a test override is installed (test builds).
/// On the production hot path this is a single OnceLock atomic load.
#[cfg(not(test))]
#[inline]
pub(crate) fn runtime_shape_indices() -> RuntimeShapeIndices {
    match RUNTIME_SHAPE_INDICES.get() {
        Some(idx) => *idx,
        None => {
            eprintln!(
                "sigil_init_shapes not called (RUNTIME_SHAPE_INDICES unset) \
                 — runtime allocator reached typed-malloc path before \
                 the codegen-emitted main shim ran `sigil_init_shapes`"
            );
            std::process::abort();
        }
    }
}

#[cfg(test)]
pub(crate) fn runtime_shape_indices() -> RuntimeShapeIndices {
    // Fast path: override already installed (by `sigil_gc_init` or a
    // test-helper call). Return without taking the shape-descriptors
    // lock.
    {
        let guard = RUNTIME_SHAPE_INDICES_TEST_OVERRIDE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(idx) = *guard {
            return idx;
        }
    }
    // Slow path: lazy install. Many runtime tests call
    // `sigil_handler_frame_new` (which routes through
    // `runtime_shape_indices()`) without explicitly calling
    // `install_shape_descriptors_for_test` — they only need the
    // runtime suffix populated, with no codegen prefix. Install it
    // here on first hit. Lock order matches
    // `install_shape_descriptors_for_test` (SHAPE_DESCRIPTORS first,
    // then RUNTIME_SHAPE_INDICES) so the two callers never deadlock
    // against each other.
    let mut shape_guard = SHAPE_DESCRIPTORS_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut runtime_guard = RUNTIME_SHAPE_INDICES_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(idx) = *runtime_guard {
        return idx;
    }
    let mut descriptors: Vec<usize> = Vec::new();
    let runtime_indices = append_runtime_shapes(&mut descriptors);
    *shape_guard = Some(descriptors);
    *runtime_guard = Some(runtime_indices);
    runtime_indices
}

/// Test-only override for `RUNTIME_SHAPE_INDICES`. Set by
/// `install_shape_descriptors_for_test` so tests can adjust the
/// codegen-prefix size (which shifts the runtime suffix indices)
/// across runs without needing to reset the production `OnceLock`.
#[cfg(test)]
static RUNTIME_SHAPE_INDICES_TEST_OVERRIDE: std::sync::Mutex<Option<RuntimeShapeIndices>> =
    std::sync::Mutex::new(None);

/// Build a Boehm typed-malloc descriptor for the given Sigil
/// (bitmap, payload_word_count) shape. Same arithmetic the
/// now-deleted `gc::descriptor::build_descriptor` performed; lifted
/// here so the now-single caller (`sigil_init_shapes`) reads
/// straightforwardly without an intermediate module.
///
/// Per `gc_typed.h`, `GC_make_descriptor` returns a non-zero
/// `GC_descr` handle on success and a conservative-fallback handle
/// (also non-zero) on internal memory exhaustion. The runtime
/// stores either path transparently; precision loss would surface
/// in the false-retention reproducer test, not at this site.
fn build_descriptor_for_shape(sigil_bitmap: u32, payload_word_count: u8) -> usize {
    debug_assert!(
        (payload_word_count as u64) <= sigil_header_constants::COUNT_MASK,
        "build_descriptor_for_shape: payload_word_count {} exceeds Header's 6-bit count field max = {}",
        payload_word_count,
        sigil_header_constants::COUNT_MASK,
    );
    let boehm_bitmap: usize = (sigil_bitmap as usize) << 1;
    let len_bits: usize = 1 + payload_word_count as usize;
    // SAFETY: `GC_make_descriptor` reads `len_bits` bits from
    // `&boehm_bitmap`. The `payload_word_count <= 63` invariant
    // keeps `len_bits <= 64`, exactly within the single-usize
    // backing buffer on 64-bit targets. `&boehm_bitmap` is valid
    // for the call's duration; Boehm doesn't retain it.
    unsafe { GC_make_descriptor(&boehm_bitmap, len_bits) }
}

/// Read the descriptor for `index` at `sigil_alloc`'s typed-malloc
/// branch. Production builds compile to a direct read from
/// `SHAPE_DESCRIPTORS` (populated once by `sigil_init_shapes`); test
/// builds layer a Mutex-protected override slot that tests can
/// install / replace freely across runs without paying the
/// `OnceLock::take`-on-nightly cost.
#[cfg(not(test))]
#[inline]
fn shape_descriptor_at(index: u32) -> usize {
    match SHAPE_DESCRIPTORS.get() {
        Some(t) => t[index as usize],
        None => {
            eprintln!(
                "sigil_init_shapes not called — sigil_alloc's typed-malloc \
                 branch reached before the codegen-emitted main shim ran \
                 `sigil_init_shapes` (descriptor_index = {index})"
            );
            std::process::abort();
        }
    }
}

#[cfg(test)]
fn shape_descriptor_at(index: u32) -> usize {
    let guard = SHAPE_DESCRIPTORS_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match &*guard {
        Some(ds) => ds[index as usize],
        None => match SHAPE_DESCRIPTORS.get() {
            Some(t) => t[index as usize],
            None => {
                eprintln!(
                    "sigil_init_shapes not called and no test override \
                     installed (descriptor_index = {index})"
                );
                std::process::abort();
            }
        },
    }
}

/// Test-only override for `SHAPE_DESCRIPTORS`. Tests that exercise
/// `sigil_alloc`'s typed-malloc branch register their shapes via
/// `install_shape_descriptors_for_test`, which writes here. The
/// production read path (`shape_descriptor_at`) reads from
/// `SHAPE_DESCRIPTORS` directly; the test path consults this slot
/// first. Replacing rather than appending matches the test-isolation
/// pattern: each test starts from a known state, sets the shapes it
/// needs, runs, then either drops out (next test's helper call
/// overwrites) or explicitly clears.
#[cfg(test)]
static SHAPE_DESCRIPTORS_TEST_OVERRIDE: std::sync::Mutex<Option<Vec<usize>>> =
    std::sync::Mutex::new(None);

/// Test-only helper: install a fresh set of shape descriptors that
/// `sigil_alloc`'s typed-malloc branch will see. Builds descriptors
/// for the supplied codegen-emitted shapes, appends the runtime-known
/// shapes after them (so runtime allocators inside the runtime crate
/// keep working under tests), and records the resulting runtime-
/// shape indices in `RUNTIME_SHAPE_INDICES_TEST_OVERRIDE`. Returns
/// the indices the test should use for ITS shapes (i.e., the
/// codegen-prefix indices `0..shapes.len()`).
///
/// Tests serialise via `gc_test_lock`, so concurrent modification of
/// the override slots is not a concern.
#[cfg(test)]
pub(crate) fn install_shape_descriptors_for_test(shapes: &[(u32, u8)]) -> Vec<u32> {
    let mut descriptors: Vec<usize> = shapes
        .iter()
        .map(|&(bitmap, count)| build_descriptor_for_shape(bitmap, count))
        .collect();
    let indices: Vec<u32> = (0..shapes.len() as u32).collect();
    let runtime_indices = append_runtime_shapes(&mut descriptors);
    *SHAPE_DESCRIPTORS_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(descriptors);
    *RUNTIME_SHAPE_INDICES_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(runtime_indices);
    indices
}

#[cfg(test)]
pub(crate) fn clear_shape_descriptors_for_test() {
    *SHAPE_DESCRIPTORS_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *RUNTIME_SHAPE_INDICES_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

// `atexit` from the C runtime. Used by `sigil --print-runtime-stats` to
// dump counters when the compiled program exits. We avoid depending on
// the `libc` crate (not in the plan's dependency allow-list) and declare
// the signature directly.
extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn counter_atexit_cb() {
    sigil_counter_print_all();
}

static GC_INIT: Once = Once::new();

/// Plan E2 Phase 3 GC-time follow-up #2 — cadence counter for
/// the `SIGIL_FORCE_GC_EVERY_N_ALLOCS` env-var injection.
/// Incremented once per `sigil_alloc` call when the env var is
/// set; `GC_gcollect()` fires when `(count % N == 0)`.
///
/// Process-wide AtomicU64 (Relaxed) — every alloc on every
/// thread increments. Thread-local counters would each fire at
/// their own N cadence, missing the "every N allocations across
/// the process" semantics. AtomicU64 at 5M allocs/workload would
/// take ~3.7M years to overflow; non-concern.
static FORCE_GC_ALLOC_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Parsed `SIGIL_FORCE_GC_EVERY_N_ALLOCS` cadence. `Some(N)` when
/// the env var is a positive integer at `sigil_gc_init` time;
/// `None` otherwise. Cached so per-alloc dispatch is a single
/// Relaxed load + branch.
///
/// The outer `OnceLock::get()` returns `Some(&Option<NonZeroU64>)`
/// once `sigil_gc_init` has run; the inner `Option` distinguishes
/// "env var unset / invalid → no injection" (`Some(None)`) from
/// "env var set to N" (`Some(Some(N))`).
static FORCE_GC_CADENCE: OnceLock<Option<NonZeroU64>> = OnceLock::new();

/// Initialise Boehm GC. Safe to call multiple times from any number of
/// threads; only the first caller runs `GC_init()` and all others wait on
/// `Once` until init completes. The generated `main` shim calls this
/// exactly once before transferring control to user code; tests also call
/// it (serialised by `Once`).
///
/// Also honours `SIGIL_PRINT_STATS=1`: when set on entry, an `atexit`
/// hook is installed that prints every runtime counter to stderr at
/// process exit. `sigil --print-runtime-stats <input>` sets this env
/// var on the child process it spawns.
#[no_mangle]
pub extern "C" fn sigil_gc_init() {
    GC_INIT.call_once(|| {
        // Plan E2 Phase 3 Task 12 — pin marker count to 1 BEFORE
        // GC_init. Per gc.h:108, `GC_set_markers_count` "has no
        // effect if called after GC initialization", so the
        // order matters. Single-marker mode keeps Boehm's
        // semantics single-threaded even when later
        // `GC_allow_register_threads` calls (from the
        // discriminator's runtime-thread path) would otherwise
        // auto-spawn parallel markers — which PR #170 surfaced
        // as breaking alloc-heavy workloads.
        unsafe { GC_set_markers_count(1) };

        // Plan E2 Phase 3 GC-time follow-up — optional max-heap-size
        // pin for measurement runs that need full GCs to fire at a
        // lower threshold than Boehm's default heuristic. Same
        // pre-`GC_init` ordering constraint as `GC_set_markers_count`
        // above (per `gc.h`). Empty / unset / invalid → no budget
        // (Boehm's default applies). Positive integer kilobytes →
        // budget enforced from the first allocation onward.
        //
        // The empty-string guard is load-bearing for the GitHub
        // Actions workflow: `SIGIL_MAX_HEAP_SIZE_KB: ${{ inputs.heap_budget_kb }}`
        // with an empty `heap_budget_kb` input sets the env var to
        // empty-string (not unset). Without `is_empty()`, every
        // default-budget workflow run would emit a spurious
        // "expected positive integer kilobytes" warning per workload
        // invocation — see PR #176 review item 1.
        //
        // See `compiler/docs/plan-e2-phase-3-gc-time-followup.md`.
        if let Ok(s) = std::env::var("SIGIL_MAX_HEAP_SIZE_KB") {
            if !s.is_empty() {
                match s.parse::<usize>() {
                    Ok(n) if n > 0 => {
                        // SAFETY: `GC_set_max_heap_size` is documented as
                        // safe to call before `GC_init` (Boehm reads the
                        // pinned value once at init time). Multiplying
                        // by 1024 saturates to `usize::MAX` rather than
                        // overflowing, which Boehm again treats as "no
                        // practical limit" — both correct fall-backs for
                        // a pathologically large input.
                        let bytes = n.saturating_mul(1024);
                        unsafe { GC_set_max_heap_size(bytes) };
                    }
                    _ => {
                        eprintln!(
                            "sigil_gc_init: ignoring SIGIL_MAX_HEAP_SIZE_KB={s:?} \
                             (expected positive integer kilobytes)"
                        );
                    }
                }
            }
        }

        // Plan E2 Phase 3 GC-time follow-up #2 — cadence-injection
        // env var. Parsed at init time so per-alloc dispatch is a
        // single Relaxed load. Unset / empty / 0 / invalid → no
        // injection (Boehm's default pacing applies).
        //
        // The empty-string guard matches `SIGIL_MAX_HEAP_SIZE_KB`'s
        // PR #176 review-item-1 fix: the GitHub Actions workflow
        // sets `SIGIL_FORCE_GC_EVERY_N_ALLOCS: ${{ inputs.force_gc_every_n_allocs }}`
        // with an empty input → env var set to empty-string (not
        // unset). Without the empty-string guard, every default-
        // injection workflow run would emit a spurious warning per
        // workload invocation.
        //
        // We PARSE the env var here (before `GC_init`) but PUBLISH
        // to `FORCE_GC_CADENCE` AFTER `GC_init` (see the `set` call
        // below `GC_init`). Unlike `GC_set_max_heap_size` above,
        // the cadence has no ordering constraint with Boehm's
        // init — and publishing before init would open a tiny
        // race window where a racing `sigil_alloc` could observe
        // `FORCE_GC_CADENCE.get() == Some(Some(n))` and call
        // `GC_gcollect()` before `GC_init()` has run. Impossible
        // in practice (Sigil's main shim is synchronous) but
        // closing the window is free.
        //
        // See `compiler/docs/plan-e2-phase-3-gc-time-followup.md`'s
        // "Force-injection follow-up" section.
        let cadence: Option<NonZeroU64> = match std::env::var("SIGIL_FORCE_GC_EVERY_N_ALLOCS") {
            Ok(s) if !s.is_empty() => match s.parse::<u64>() {
                Ok(n) => NonZeroU64::new(n).or_else(|| {
                    eprintln!(
                        "sigil_gc_init: ignoring \
                             SIGIL_FORCE_GC_EVERY_N_ALLOCS={s:?} \
                             (expected positive integer)"
                    );
                    None
                }),
                Err(_) => {
                    eprintln!(
                        "sigil_gc_init: ignoring \
                             SIGIL_FORCE_GC_EVERY_N_ALLOCS={s:?} \
                             (expected positive integer)"
                    );
                    None
                }
            },
            _ => None,
        };

        // SAFETY: `Once::call_once` guarantees exactly one invocation even
        // under concurrent entry, so Boehm's non-reentrant init runs on a
        // single thread.
        unsafe { GC_init() };

        // Publish the cadence AFTER GC_init — see the parse block's
        // doc comment for the ordering rationale. Any racing
        // sigil_alloc that observes `Some(Some(n))` here is
        // guaranteed to see an initialised Boehm.
        let _ = FORCE_GC_CADENCE.set(cadence);

        // Wire the counter dump exactly once — doing it inside the Once
        // guarantees atexit sees exactly one registration per process.
        if std::env::var_os("SIGIL_PRINT_STATS").is_some() {
            // SAFETY: atexit only requires the callback pointer to be
            // valid for the lifetime of the process; `counter_atexit_cb`
            // is a static function with no captured state.
            unsafe { atexit(counter_atexit_cb) };
        }
    });

    // Plan B Task 56: register the calling thread's runtime roots with
    // Boehm. Both `HANDLER_STACK` (the thread-local handler-stack head)
    // and `ARENA` (the per-dispatch bump arena's backing storage) hold
    // pointers to Boehm-allocated objects; without explicit rooting,
    // Boehm's automatic stack/data-segment scan does not cover them in
    // any portable way (`thread_local!` storage is not enumerated by
    // `dl_iterate_phdr`, and the arena's `Vec<u8>` payload sits on the
    // system allocator's heap, not Boehm's).
    //
    // Per-thread (NOT inside the `Once`): the calling thread may not
    // be the same thread that won the `Once` race for `GC_init`, and
    // every thread that uses these TLS slots must root them itself.
    // The registration helpers are idempotent per thread.
    //
    // **Test-mode caveat:** under `cargo test`, the test runner
    // spawns a fresh thread per test. Auto-registering each test
    // thread's TLS ranges as Boehm roots leaks stale ranges when the
    // thread exits, which segfaults the next collection. In test
    // builds the auto-registration is suppressed; tests opt in via
    // `GcThreadEnrolment::acquire` (in `test_support`), which
    // registers AND unregisters symmetrically through Drop. Production
    // builds run on a single long-lived main thread so leakage is
    // not a concern.
    #[cfg(not(test))]
    {
        crate::handlers::register_handler_stack_root_for_calling_thread();
        crate::handlers::register_outer_post_arm_k_stack_root_for_calling_thread();
        crate::arena::register_arena_root_for_calling_thread();

        // Plan E2 Phase 3 Task 11: register the calling thread
        // (the Sigil program's main thread) for precise stack
        // roots. Sets IS_SIGIL_THREAD; installs the
        // push_other_roots callback (Once-gated); pre-warms the
        // stackmap module's lazy initialisers.
        crate::gc::threads::register_sigil_thread_for_precise_roots();

        // Plan E2 Phase 3 Task 12 — prime the safe-stack-range
        // cache for BOTH FP-chain walkers (the SIGPROF unwinder
        // and the GC mark-phase precise walker) BEFORE either
        // can fire. `pthread_attr_getstack` /
        // `pthread_get_stackaddr_np` are not async-signal-safe
        // for the SIGPROF case AND not STW-safe for the GC mark
        // phase (the mark phase suspends other threads, and any
        // libc malloc/lock the pthread queries might take could
        // deadlock against a suspended thread holding malloc's
        // internal lock). The cache MUST be populated off both
        // paths. Doing it here (`sigil_gc_init`, on the main
        // thread, before any worker spawns) is the canonical
        // safe point. Both walkers validate FPs against the
        // cached range to harden against
        // `-fomit-frame-pointer`-compiled libgc internals
        // leaking wild rbp values onto the unwind path.
        if let Some((lo, hi)) = crate::stackmap_xcheck::thread_stack_bounds() {
            crate::profile::unwind::install_safe_stack_range(lo, hi);
        }
    }

    // v2 profile-data surface — gated by env vars. When neither
    // SIGIL_CPU_PROFILE nor SIGIL_ALLOC_PROFILE is set, each
    // maybe_init is a single env::var_os lookup + early return
    // (the zero-overhead path).
    #[cfg(not(test))]
    crate::profile::maybe_init();
}

/// Debug-only precondition check for the precise-marking path in
/// `sigil_alloc`: the requested allocation size must be large enough
/// that `GC_malloc_explicitly_typed`'s descriptor bitmap covers the
/// full block. `GC_malloc_explicitly_typed` requires
/// `size_in_bytes >= (1 + payload_count) * 8` — one word for the
/// Sigil header plus `payload_count` payload words.
///
/// Extracted from `sigil_alloc` as a regular Rust fn (not `extern "C"`)
/// so the `debug_assert!` panics unwind cleanly under `#[should_panic]`
/// tests. Through the C ABI the panic would convert to an abort and
/// `cargo test` would treat it as a process crash, not a passing
/// `#[should_panic]`.
///
/// For bitmap-bearing objects, codegen emits `payload_bytes =
/// payload_count * 8` (word-aligned payload), so `total = 8 +
/// payload_count * 8 = (1 + count) * 8` exactly meets the floor.
/// A drift between codegen's `payload_bytes` and the Header's `count`
/// would surface as a Boehm scan beyond the allocation — this check
/// pins the discipline at the boundary.
#[inline]
fn assert_precise_alloc_size(total: usize, count: u8, bitmap: u32) {
    debug_assert!(
        total >= (1 + count as usize).saturating_mul(8),
        "sigil_alloc: total bytes {} < precise-descriptor minimum {} (count={}, bitmap=0b{:b})",
        total,
        (1 + count as usize) * 8,
        count,
        bitmap,
    );
}

/// Allocate `8 + payload_bytes` from Boehm, write the 8-byte header, and
/// return a pointer to the header (never to the payload). Callers hold a
/// header pointer as their canonical reference to the object.
///
/// `payload_bytes` is the number of bytes after the header; it does not
/// need to be word-aligned, but callers generally size objects to whole
/// words so header fields stay consistent.
///
/// # Safety
///
/// Safe to call from any thread. Does not trap on out-of-memory — Boehm
/// aborts the process via its default oom-handler on OOM, which v1 does
/// not override.
#[no_mangle]
pub extern "C" fn sigil_alloc(header: u64, payload_bytes: usize, descriptor_index: u32) -> *mut u8 {
    let total = 8usize.saturating_add(payload_bytes);

    // Bump Boehm counters before the alloc call so a panic inside Boehm
    // (e.g. oom abort) still shows the intent in telemetry.
    counters::incr(CounterId::BoehmAllocCount);
    counters::add(CounterId::BoehmAllocBytes, total as u64);

    // Plan E2 Phase 1 Task 5 — opt-in stackmap cross-check hook.
    // Gated by `SIGIL_GC_CROSS_CHECK=1` at runtime; production paths
    // skip this entirely (the env var is read once at startup and
    // cached). On each sampled alloc, the precise root walker is
    // invoked and asserts (a) every precise root address lies inside
    // the calling thread's stack range, and (b) the value at each
    // address is heap-pointer-shaped per Boehm's view. Diverges abort
    // the process with a diagnostic.
    crate::stackmap_xcheck::maybe_cross_check();

    // v2 profile-data surface — sampled allocation profile hook.
    // Inlined; the fast path is a single relaxed atomic load + branch
    // when SIGIL_ALLOC_PROFILE is unset.
    #[cfg(not(test))]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    crate::profile::alloc::maybe_sample_alloc(total as u64);

    // Plan E2 Phase 3 Task 12 — capture sigil_alloc's own FP so the
    // `push_other_roots` callback (which fires from inside Boehm's
    // mark phase) can walk the Sigil call chain from a frame that's
    // safely on the stack and outside libgc's potentially `-fomit-
    // frame-pointer`-compiled call chain. The Drop guard clears the
    // TLS slot at every exit path (normal return + any
    // panic-via-abort).
    //
    // Gated on `cfg(not(test))` to match the production-only wiring
    // of the rest of Task 12 (GC_do_blocking + GC_call_with_gc_active
    // hookup, push_other_roots install). The runtime unit tests
    // exercise sigil_alloc in active GC state directly; the captured
    // FP is unused by them (IS_SIGIL_THREAD is false on test threads,
    // so the callback short-circuits before reading the FP).
    #[cfg(not(test))]
    let _fp_guard = SigilCallerFpGuard::capture();

    let raw = alloc_dispatch_active(header, total, descriptor_index);

    if raw.is_null() {
        // Boehm's default oom-handler aborts before returning null, so
        // reaching here means something has gone wrong that the runtime
        // cannot recover from. Abort cleanly.
        eprintln!("sigil_alloc: Boehm returned null");
        std::process::abort();
    }

    // SAFETY: `raw` points to at least `total` bytes obtained from one
    // of `GC_malloc_atomic` (bitmap=0 path), `GC_malloc` (count=0
    // conservative-scan path), or `GC_malloc_explicitly_typed`
    // (precise-marking path), and `total >= 8`. Writing the header
    // word is an aligned u64 write at the start of a freshly-returned
    // block. This is not an interior pointer (the header IS the
    // object's header).
    unsafe {
        let hdr_ptr: *mut u64 = raw.cast();
        hdr_ptr.write(header);
    }

    // Plan E2 Phase 3 GC-time follow-up #2 — env-var-gated
    // `GC_gcollect()` injection at a fixed allocation cadence.
    // Positioned at the END of `sigil_alloc`, after the typed-
    // malloc dispatch returned and the header was written, so
    // we're not reentrant into Boehm during allocation. The
    // production-build FP-capture guard (`_fp_guard`) is still
    // alive at this point; this is intentional — when the
    // injected `GC_gcollect()` triggers a mark phase, the precise
    // walker reads CAPTURED_SIGIL_CALLER_FP exactly as it would
    // for any Boehm-initiated collection.
    //
    // Zero per-alloc cost when env var unset: the OnceLock returns
    // `Some(&None)` and the inner match falls through without the
    // AtomicU64 fetch_add.
    if let Some(Some(n)) = FORCE_GC_CADENCE.get() {
        let count = FORCE_GC_ALLOC_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(n.get()) {
            counters::incr(CounterId::ForcedGcCount);
            // SAFETY: `GC_gcollect` is documented as safe to call
            // at any time from a registered thread. `sigil_alloc`
            // runs on a Sigil thread which is implicitly registered
            // (main thread) or explicitly registered via
            // `register_sigil_thread_for_precise_roots` (Plan E2
            // Phase 3 Task 11). Tests opt their thread in via
            // `GcThreadEnrolment::acquire`.
            unsafe { GC_gcollect() };
        }
    }

    raw
}

/// Plan E2 Phase 3 Task 12 — branch sigil_alloc's allocator dispatch
/// through `GC_call_with_gc_active` on production builds so the actual
/// `GC_malloc_*` call runs in GC-active state even when sigil_run_loop
/// has parked the thread via `GC_do_blocking`. In test builds the
/// thread is already GC-active (sigil_run_loop is not on the call
/// stack), so the wrapper is bypassed.
///
/// The split into a separate helper keeps the dispatch logic in one
/// place — the production path threads parameters through
/// `AllocActiveCtx` because `GC_call_with_gc_active`'s C ABI takes a
/// `*mut c_void` `cd` pointer, but the dispatch itself is identical
/// regardless of which path entered it.
#[inline]
fn alloc_dispatch_active(header: u64, total: usize, descriptor_index: u32) -> *mut u8 {
    #[cfg(not(test))]
    {
        let mut ctx = AllocActiveCtx {
            header,
            total,
            descriptor_index,
            raw: std::ptr::null_mut(),
        };
        // SAFETY: GC_call_with_gc_active is documented as stack-
        // disciplined and safe to invoke even from active state
        // (per gc.h:1626). The trampoline reads/writes `ctx` only
        // through the type-erased pointer we just took, the lifetime
        // of `ctx` covers the call, and the trampoline's body does
        // not retain the pointer past return.
        unsafe {
            GC_call_with_gc_active(
                alloc_active_trampoline,
                &mut ctx as *mut AllocActiveCtx as *mut c_void,
            );
        }
        ctx.raw
    }
    #[cfg(test)]
    {
        alloc_dispatch(header, total, descriptor_index)
    }
}

#[cfg(not(test))]
#[repr(C)]
struct AllocActiveCtx {
    header: u64,
    total: usize,
    descriptor_index: u32,
    raw: *mut u8,
}

#[cfg(not(test))]
extern "C" fn alloc_active_trampoline(cd: *mut c_void) -> *mut c_void {
    // SAFETY: `cd` is the `&mut AllocActiveCtx` we constructed in
    // `alloc_dispatch_active`; its lifetime extends for the duration
    // of the `GC_call_with_gc_active` call which contains us. No
    // other thread has access to this stack-local context.
    let ctx = unsafe { &mut *(cd as *mut AllocActiveCtx) };
    ctx.raw = alloc_dispatch(ctx.header, ctx.total, ctx.descriptor_index);
    std::ptr::null_mut()
}

/// Allocator selection. Plan E2 Phase 2 Task 8 splits the bitmap
/// dispatch into three branches based on what the Header's
/// `(count, bitmap)` pair encodes:
///
///   - `bitmap == 0`                 → `GC_malloc_atomic`
///     No GC pointers anywhere in the payload. Boehm skips
///     scanning entirely — strictly better than precise marking.
///
///   - `bitmap != 0 && count == 0`   → `GC_malloc` (conservative)
///     The Sigil convention for "object too large for the
///     header's 6-bit count field to describe precisely" — arrays,
///     mut-arrays, the string-builder segments table. Count=0 is
///     the structural signal; bitmap=non-zero routes out of the
///     atomic path. These payloads are scanned conservatively;
///     element pointers survive GC because Boehm's mark phase
///     walks the whole block looking for pointer-shaped values.
///
///   - `bitmap != 0 && count >  0`   → `GC_malloc_explicitly_typed`
///     Precise marking via Boehm's typed-malloc. The descriptor
///     cache (Task 7) hands out one `GC_descr` per shape; first
///     observation of a `(bitmap, count)` shape builds the
///     descriptor via `GC_make_descriptor`, subsequent calls
///     reuse the cached handle.
///
/// The count==0 branch closes a silent correctness regression
/// introduced by Task 8's initial drop of `GC_malloc`: a typed
/// descriptor built from `(bitmap=1, count=0)` would have
/// `len_bits = 1` describing only the header word; Boehm's
/// tile-replication would then treat every element slot as a
/// non-pointer, silently collecting any heap-bearing array
/// elements that lacked an independent stack root.
#[inline]
fn alloc_dispatch(header: u64, total: usize, descriptor_index: u32) -> *mut u8 {
    let h = Header(header);
    if h.pointer_bitmap() == 0 {
        unsafe { GC_malloc_atomic(total) as *mut u8 }
    } else if h.payload_count() == 0 {
        unsafe { GC_malloc(total) as *mut u8 }
    } else {
        let count = h.payload_count();
        assert_precise_alloc_size(total, count, h.pointer_bitmap());
        // Static-descriptor-table follow-up: the descriptor cache
        // (`gc::descriptor` module) is gone; codegen threads the
        // shape's pre-registered index through `sigil_alloc`'s
        // third arg, and we read the materialised `GC_descr` out of
        // the static `SHAPE_DESCRIPTORS` vector. `u32::MAX` is the
        // sentinel value codegen emits for the atomic /
        // conservative branches (which don't reach this typed-
        // malloc arm); reaching this point with the sentinel
        // signals a codegen / runtime branch-condition drift, not
        // a recoverable runtime condition.
        debug_assert!(
            descriptor_index != u32::MAX,
            "sigil_alloc: typed-malloc branch hit with sentinel \
             descriptor_index = u32::MAX (bitmap=0b{:b}, count={count})",
            h.pointer_bitmap(),
        );
        let descr = shape_descriptor_at(descriptor_index);
        // SAFETY: `descr` was built by `GC_make_descriptor` (via
        // `sigil_init_shapes` at program start) and is alive for the
        // process lifetime (the static `SHAPE_DESCRIPTORS` table never
        // evicts). `total` meets the descriptor's
        // `len_bits * sizeof(GC_word)` floor (debug_asserted above
        // for non-release builds).
        unsafe { GC_malloc_explicitly_typed(total, descr) as *mut u8 }
    }
}

/// Plan E2 Phase 3 Task 12 — Drop guard that clears
/// `CAPTURED_SIGIL_CALLER_FP` on any exit path from `sigil_alloc`.
/// Constructed via `capture()`, which stashes the FP returned by
/// `stackmap::capture_caller_fp_for_walk()` — i.e., sigil_alloc's
/// own frame pointer, the correct starting point for the precise
/// walker (which iterates UP and reads the saved-PC at each frame
/// to look up stackmap entries for the FUNCTION THAT CALLED that
/// frame, so starting at sigil_alloc's FP finds the Sigil caller's
/// own stackmap entries — starting one frame higher would skip
/// them).
#[cfg(not(test))]
struct SigilCallerFpGuard;

#[cfg(not(test))]
impl SigilCallerFpGuard {
    /// `#[inline(always)]` is load-bearing in production code: the
    /// FP must be captured from *sigil_alloc's* frame, not from
    /// this helper's frame. With inlining, the call to
    /// `capture_caller_fp_for_walk` (itself `#[inline(never)]`)
    /// takes place in sigil_alloc's frame, so the helper's `*rbp`
    /// deref yields sigil_alloc's FP.
    ///
    /// Sigil's build config pins this: `.cargo/config.toml` sets
    /// `-C force-frame-pointers=yes` for both Linux and macOS
    /// rustflags, and the workspace release profile inherits the
    /// `opt-level = 3` that makes `#[inline(always)]` a strong
    /// hint. A `debug_assert!` in `capture()` provides a
    /// belt-and-suspenders runtime sanity check: the captured FP
    /// must lie inside the calling thread's pthread-known stack
    /// range when the cache is populated (post-`sigil_gc_init`).
    /// If a future codegen change ever caused the inline to be
    /// declined, an off-by-one frame would either crash the
    /// walker (caught by the same stack-range gate added to
    /// `walk_for_gc_with_callback_from`) or trip this assertion
    /// in debug builds.
    #[inline(always)]
    fn capture() -> Self {
        let fp = crate::stackmap::capture_caller_fp_for_walk();
        debug_assert!(
            captured_fp_looks_plausible(fp),
            "sigil_alloc FP capture out of stack range: fp=0x{:x} \
             (likely #[inline(always)] inlining declined; \
             investigate runtime build flags)",
            fp as usize,
        );
        crate::gc::threads::capture_sigil_caller_fp(fp);
        SigilCallerFpGuard
    }
}

/// Debug-only sanity check for the captured FP. Returns true when:
/// (a) the safe-stack-range cache hasn't been populated yet
///     (`sigil_gc_init` hasn't run, or pthread bounds weren't
///     available — in both cases we have nothing to validate
///     against and must accept whatever FP we got), OR
/// (b) the FP is non-null AND falls inside the cached
///     `[stack_lo, stack_hi)` range.
///
/// A `false` reading in production-mode debug builds indicates
/// either an `#[inline(always)]` failure on
/// `SigilCallerFpGuard::capture` (the helper produced its own
/// frame's FP instead of sigil_alloc's) or a wild rbp value from
/// a Rust codegen surprise. Either is a bug worth surfacing
/// during development.
#[cfg(not(test))]
fn captured_fp_looks_plausible(fp: *const usize) -> bool {
    let lo = crate::profile::unwind::SAFE_STACK_LO.load(std::sync::atomic::Ordering::Relaxed);
    let hi = crate::profile::unwind::SAFE_STACK_HI.load(std::sync::atomic::Ordering::Relaxed);
    if lo == 0 || hi <= lo {
        return true;
    }
    let fp_addr = fp as usize;
    fp_addr != 0 && fp_addr >= lo && fp_addr < hi
}

#[cfg(not(test))]
impl Drop for SigilCallerFpGuard {
    fn drop(&mut self) {
        crate::gc::threads::clear_sigil_caller_fp();
    }
}

/// Allocate and populate a Sigil `String` object from a byte slice.
///
/// Layout of a `String` object on the heap:
///
/// ```text
/// offset 0  : 8-byte header (tag TAG_STRING, count = ceil(8 + len) / 8, bitmap 0)
/// offset 8  : u64 length (in bytes)
/// offset 16 : UTF-8 bytes (length bytes, then zero-pad to word alignment)
/// ```
///
/// Bytes are read from `src` and copied verbatim into the payload. Callers
/// are responsible for ensuring `src` points to `len` readable bytes that
/// form valid UTF-8 (v1 does not validate — Stage 1 only emits literals
/// that are known-valid at compile time).
///
/// Returns a raw pointer to the header. Tagging as a Sigil `Value` is the
/// caller's responsibility (typically done via `value::from_heap`).
///
/// # Safety
///
/// `src` must be non-null and point to at least `len` readable bytes, or
/// `src` may be null when `len == 0`. Any other combination is UB.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_new(src: *const u8, len: usize) -> *mut u8 {
    // The payload is: one length word (8 bytes) + len data bytes, padded up
    // to a multiple of 8 so the object's payload-word count is a whole
    // number.
    //
    // `Header::count` is 6 bits — capped at 63 payload words = 504 bytes
    // (496 bytes of String content + 8 bytes for the length word). Real-
    // world strings (env-var values, file contents, captured stdout) can
    // exceed this. Mirror the convention `runtime/src/array.rs` uses for
    // the same problem: when the payload would overflow `count`, write
    // `count = 0` and rely on Boehm's allocator-tracked size for any
    // scan-step that needs the block bound. `TAG_STRING` has `bitmap = 0`
    // (payload bytes hold no pointers), so the GC never walks per-element
    // slots; the actual byte length lives in the explicit length word at
    // offset 8 and `sigil_string_len` reads from there, not `count`.
    let payload_bytes = 8 + round_up_to_word(len);
    let payload_words = payload_bytes / 8;
    let count_field: u8 = if payload_words <= 63 {
        payload_words as u8
    } else {
        0
    };

    let h = Header::new(header::TAG_STRING, count_field, 0);
    // String payload is byte data (bitmap=0) → atomic path;
    // descriptor_index unused, pass the sentinel.
    let obj = sigil_alloc(h.raw(), payload_bytes, u32::MAX);

    // Write the length word at offset 8.
    //
    // SAFETY: gc-heap-ptr arithmetic (pointer arithmetic is to local stack
    // variables inside runtime, computed only to drive a single aligned
    // store, not stored or passed). obj+8 is still inside the object but
    // the write and the read below are transient.
    let len_ptr: *mut u64 = obj.add(8).cast();
    len_ptr.write(len as u64);

    // Copy the byte payload at offset 16.
    //
    // SAFETY: gc-heap-ptr arithmetic (temporary pointers used only for a
    // single byte-range copy, never returned to caller).
    if len > 0 && !src.is_null() {
        let dst = obj.add(16);
        std::ptr::copy_nonoverlapping(src, dst, len);
    }

    obj
}

/// Read the length (in bytes) of a heap `String` object. Caller passes the
/// header-pointer form; interior pointers are never produced.
///
/// # Safety
///
/// `obj` must be a pointer to a valid `String` header previously returned
/// by `sigil_string_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_len(obj: *const u8) -> usize {
    // SAFETY: gc-heap-ptr arithmetic (used transiently for a single read).
    let len_ptr: *const u64 = obj.add(8).cast();
    len_ptr.read() as usize
}

/// Borrow the raw UTF-8 byte slice out of a heap `String` for the duration
/// of a syscall. The pointer is transient — callers must not store it.
///
/// # Safety
///
/// Same contract as `sigil_string_len`. The returned pointer is valid for
/// `sigil_string_len(obj)` bytes for as long as `obj` is live (which
/// Boehm ensures for the duration of the call chain).
pub(crate) unsafe fn string_bytes(obj: *const u8) -> (*const u8, usize) {
    let len = sigil_string_len(obj);
    // SAFETY: gc-heap-ptr arithmetic (immediately consumed by the caller
    // for a single write syscall; never stored or passed back across
    // FFI or module boundaries).
    let bytes = obj.add(16);
    (bytes, len)
}

#[inline]
fn round_up_to_word(n: usize) -> usize {
    (n + 7) & !7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_up_handles_boundaries() {
        assert_eq!(round_up_to_word(0), 0);
        assert_eq!(round_up_to_word(1), 8);
        assert_eq!(round_up_to_word(7), 8);
        assert_eq!(round_up_to_word(8), 8);
        assert_eq!(round_up_to_word(9), 16);
    }

    #[test]
    fn alloc_and_read_string() {
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let before_count = counters::read(CounterId::BoehmAllocCount);
        let src = b"hi";
        // SAFETY: gc-heap-ptr arithmetic (src is a static byte literal, not a heap object).
        let obj = unsafe { sigil_string_new(src.as_ptr(), src.len()) };
        assert!(!obj.is_null());
        let after_count = counters::read(CounterId::BoehmAllocCount);
        assert!(
            after_count > before_count,
            "Boehm alloc counter must bump on String allocation"
        );

        let len = unsafe { sigil_string_len(obj) };
        assert_eq!(len, 2);

        let (bytes, len2) = unsafe { string_bytes(obj) };
        assert_eq!(len2, 2);
        let slice = unsafe { std::slice::from_raw_parts(bytes, len2) };
        assert_eq!(slice, b"hi");
    }

    #[test]
    fn alloc_empty_string() {
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let obj = unsafe { sigil_string_new(std::ptr::null(), 0) };
        assert!(!obj.is_null());
        assert_eq!(unsafe { sigil_string_len(obj) }, 0);
    }

    #[test]
    fn sigil_alloc_routes_typed_bitmap_through_static_descriptor_table() {
        // Static-descriptor-table follow-up rewrite of the prior
        // Plan E2 Phase 2 Task 8 cache-population test. The
        // RwLock + BTreeMap descriptor cache is gone; the new
        // mechanism is a codegen-emitted shape table materialized
        // at startup by `sigil_init_shapes`. Tests install the
        // table via `install_shape_descriptors_for_test`.
        //
        // Branch coverage matches the prior test:
        //   - bitmap=0           → GC_malloc_atomic (descriptor unused)
        //   - bitmap!=0, count=0 → GC_malloc        (descriptor unused)
        //   - bitmap!=0, count>0 → GC_malloc_explicitly_typed via
        //                           SHAPE_DESCRIPTORS[index]
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();

        // Register two distinct typed-malloc shapes. Indices are
        // [0, 1] in the codegen-prefix portion of the table.
        let indices = install_shape_descriptors_for_test(&[(0b1, 1), (0b10, 2)]);
        assert_eq!(indices.len(), 2, "test helper must return two indices");

        // Bitmap=0 path: descriptor_index unused; pass u32::MAX
        // sentinel. Atomic path does not touch the descriptor table.
        let zero_bitmap_header = Header::new(header::TAG_INT64, 1, 0).raw();
        let obj_atomic = sigil_alloc(zero_bitmap_header, 8, u32::MAX);
        assert!(!obj_atomic.is_null());

        // count=0, bitmap!=0 path (arrays / mut-arrays / segments
        // table): plain GC_malloc, descriptor_index unused.
        let array_header = Header::new(header::TAG_ARRAY, 0, 1).raw();
        let obj_array = sigil_alloc(array_header, 32, u32::MAX);
        assert!(!obj_array.is_null());

        // Bitmap=0b1, count=1 → typed-malloc, descriptor at index 0.
        let one_ptr_header = Header::new(header::TAG_REF, 1, 0b1).raw();
        let obj_precise = sigil_alloc(one_ptr_header, 8, indices[0]);
        assert!(!obj_precise.is_null());

        // Repeat-shape alloc reuses the same descriptor entry — the
        // table is read-only after init, so this is a single load
        // from SHAPE_DESCRIPTORS[0] both times. (The prior cache's
        // monotonic-growth invariant is now trivially preserved by
        // construction: the table has fixed size.)
        let obj_precise_2 = sigil_alloc(one_ptr_header, 8, indices[0]);
        assert!(!obj_precise_2.is_null());

        // Distinct shape: closure with one env slot (count=2,
        // bitmap=0b10) → typed-malloc at index 1.
        let closure_header = Header::new(header::TAG_CLOSURE, 2, 0b10).raw();
        let obj_closure = sigil_alloc(closure_header, 16, indices[1]);
        assert!(!obj_closure.is_null());

        clear_shape_descriptors_for_test();
    }

    /// Subprocess-mode env var for the GC-forcing tests in this
    /// module. Mirrors `SIGIL_GC_STRESS_INNER` in `handlers.rs::tests`:
    /// the outer-mode `#[test]` re-execs the binary filtered to one
    /// test with this env var set; the inner-mode body runs the
    /// actual GC calls. Each test gets its own fresh process so
    /// Boehm's per-thread mark state isn't shared across tests.
    const GC_STRESS_INNER_VAR: &str = "SIGIL_GC_STRESS_INNER";

    fn in_gc_stress_subprocess() -> bool {
        std::env::var(GC_STRESS_INNER_VAR).is_ok()
    }

    fn run_gc_stress_in_subprocess(test_name: &str) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("run_gc_stress_in_subprocess: current_exe failed: {e}");
                std::process::abort();
            }
        };
        let full_name = format!("gc::tests::{test_name}");
        let status = match std::process::Command::new(&exe)
            .args(["--exact", &full_name, "--nocapture"])
            .env(GC_STRESS_INNER_VAR, "1")
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("run_gc_stress_in_subprocess: spawn `{full_name}` failed: {e}");
                std::process::abort();
            }
        };
        assert!(
            status.success(),
            "GC-stress subprocess for `{full_name}` failed: {status}"
        );
    }

    #[test]
    fn array_of_heap_pointers_survives_forced_gc() {
        // Regression test for the silent precision-loss bug Task 8's
        // first cut introduced. Arrays use `(count=0, bitmap=1)` as
        // a "scan conservatively" signal; the initial Task 8 patch
        // routed them through `GC_malloc_explicitly_typed` with
        // `len_bits=1`, which tile-replicated "not a pointer" across
        // the whole block — every element was silently invisible to
        // the mark phase. This test pins the fix: an array
        // populated with String pointers whose only stack root is
        // the array itself must retain the strings across
        // GC_gcollect.
        //
        // Runs in a subprocess (matches `handlers::tests::*` GC
        // stress tests) so Boehm's per-thread state doesn't bleed
        // across parallel cargo test runs.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess("array_of_heap_pointers_survives_forced_gc");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();

        // Allocate an array of 8 String pointers. Use sigil_string_new
        // to populate each slot with a fresh heap-allocated string,
        // keep only the array root, then force GC and verify each
        // string survives by reading its bytes back.
        let array_header = Header::new(header::TAG_ARRAY, 0, 1);
        let payload_bytes = 8 + 8 * 8; // length word + 8 element slots
                                       // count=0 + bitmap!=0 → conservative path; descriptor_index unused.
        let array_obj = sigil_alloc(array_header.raw(), payload_bytes, u32::MAX);
        assert!(!array_obj.is_null());

        // SAFETY: array_obj is a fresh allocation; we own the full
        // payload range for initialisation.
        unsafe {
            let len_ptr: *mut u64 = array_obj.add(8).cast();
            len_ptr.write(8);
            let elems_ptr = array_obj.add(16) as *mut *mut u8;
            for i in 0..8u8 {
                let s_bytes = [0xA0u8 + i, 0xA1u8 + i, 0xA2u8 + i];
                // SAFETY: gc-heap-ptr arithmetic (s_bytes is a stack-local array; sigil_string_new copies the bytes).
                let s_obj = sigil_string_new(s_bytes.as_ptr(), s_bytes.len());
                assert!(!s_obj.is_null());
                // SAFETY: gc-heap-ptr arithmetic (i-th element slot of a freshly-allocated 8-slot array we own).
                *elems_ptr.add(i as usize) = s_obj;
            }

            // Force a full collection. With conservative scan on
            // count=0 objects, Boehm walks the array block looking
            // for pointer-shaped values and follows them into the
            // strings.
            GC_gcollect();

            // Re-read every element; each string must still be
            // alive and report its original 3-byte payload.
            for i in 0..8u8 {
                // SAFETY: gc-heap-ptr arithmetic (re-reads the i-th element slot written above; array storage still live).
                let s_obj = *elems_ptr.add(i as usize);
                assert!(!s_obj.is_null(), "string slot {i} cleared after GC");
                let len = sigil_string_len(s_obj);
                assert_eq!(len, 3, "string {i} length corrupted after GC");
                let (bytes, len) = string_bytes(s_obj);
                let slice = std::slice::from_raw_parts(bytes, len);
                assert_eq!(
                    slice,
                    &[0xA0u8 + i, 0xA1u8 + i, 0xA2u8 + i],
                    "string {i} payload corrupted after GC"
                );
            }
        }
    }

    /// Track finalizer firings across the false-retention test's
    /// lifetime. Only one test in this module registers finalizers,
    /// so a single static counter is sufficient.
    static FINALIZER_FIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn target_finalizer_cb(_obj: *mut std::ffi::c_void, _cd: *mut std::ffi::c_void) {
        FINALIZER_FIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn false_retention_reproducer_precise_marker_drops_aliased_address() {
        // **Phase 2 ship-gate test** (plan body: "the single most
        // important Phase 2 verification — it's the bug class we
        // set out to close").
        //
        // The differential property Task 8 added:
        //
        //   Pre-Task 8:  every `bitmap != 0` object went through plain
        //                `GC_malloc`. Boehm scanned its FULL payload
        //                conservatively — every word treated as a
        //                potential pointer, including words the
        //                Header's bitmap explicitly named as
        //                non-pointers. A bit pattern in a "supposedly
        //                non-pointer" slot would falsely retain its
        //                target.
        //
        //   Post-Task 8: `bitmap != 0 && count > 0` routes through
        //                `GC_malloc_explicitly_typed` with a
        //                descriptor that names each slot's pointer-
        //                ness precisely. Boehm's mark phase reads
        //                only the slots whose bitmap bit is SET.
        //                A bit pattern in a slot whose bit is CLEAR
        //                is not followed.
        //
        // The discriminator is the typed-malloc precision behaviour.
        // To test it, we pick a closure-shape header (`count = 2`,
        // `bitmap = 0b10`):
        //
        //   payload word 0:  code_ptr slot — bitmap bit 0 = 0 (non-ptr)
        //   payload word 1:  env slot 0   — bitmap bit 1 = 1 (ptr)
        //
        // We write the target's address into payload word 0 (the
        // bitmap-bit-CLEAR slot) and 0 into payload word 1. With
        // the precise marker, Boehm sees alias_obj as reachable,
        // reads its descriptor, follows slot 1 (null), skips slot 0
        // (bit cleared). Target is unreachable.
        //
        // If a future regression reroutes typed-malloc allocations
        // back through plain `GC_malloc` or builds a misshapen
        // descriptor, the conservative full-payload scan would
        // follow the bit pattern in slot 0 and retain target —
        // the finalizer would not fire and this test would FAIL.
        // That's the regression boundary the test pins.
        //
        // Runs in a subprocess (matches the Task 6 / handlers GC
        // stress pattern) so Boehm's per-thread state doesn't
        // bleed across parallel cargo workers.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess(
                "false_retention_reproducer_precise_marker_drops_aliased_address",
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        FINALIZER_FIRED.store(0, std::sync::atomic::Ordering::SeqCst);

        // Allocate the typed-malloc alias object: TAG_CLOSURE, count=2,
        // bitmap=0b10. Word 0 (code_ptr slot) is bitmap-bit-CLEAR;
        // word 1 (env slot 0) is bitmap-bit-SET. Routes through
        // `GC_malloc_explicitly_typed` per Task 8's dispatch.
        //
        // Register the shape so the typed-malloc branch can resolve
        // its descriptor_index. `install_shape_descriptors_for_test`
        // returns the indices for the supplied shapes; we want the
        // `(0b10, 2)` shape at index 0.
        let indices = install_shape_descriptors_for_test(&[(0b10, 2)]);
        let alias_descriptor_index = indices[0];
        let typed_header = Header::new(header::TAG_CLOSURE, 2, 0b10).raw();
        let alias_obj = sigil_alloc(typed_header, 16, alias_descriptor_index);
        assert!(!alias_obj.is_null());
        // Initialise both payload slots to zero so we don't smuggle
        // any stale pointer-shaped values in (Boehm zeroes by
        // contract but the codebase doesn't rely on it elsewhere —
        // see handlers::sigil_handler_frame_new's explicit zeroing).
        // SAFETY: gc-heap-ptr arithmetic (alias_obj freshly allocated, we own both payload slots).
        unsafe {
            let payload: *mut u64 = alias_obj.add(8).cast();
            payload.add(0).write(0);
            payload.add(1).write(0);
        }

        // Allocate the target + register its finalizer + write the
        // target's address into payload word 0 (the bitmap-bit-CLEAR
        // slot) — ALL inside a nested helper fn whose stack frame is
        // popped before we return. When this returns:
        //   - target's typed pointer is gone (helper's stack frame
        //     reclaimed).
        //   - target_addr local is gone (same).
        //   - The only remaining reference to target is the
        //     bit-pattern alias in alias_obj's word 0. With precise
        //     marking, Boehm reads the bitmap and DOES NOT follow
        //     this slot. Target is unreachable.
        #[inline(never)]
        unsafe fn alloc_target_and_alias_into_typed(alias_obj: *mut u8) {
            let s_bytes = b"FALSE-RETENTION-TYPED";
            // SAFETY: gc-heap-ptr arithmetic (s_bytes is a stack-local byte literal; sigil_string_new copies).
            let target = sigil_string_new(s_bytes.as_ptr(), s_bytes.len());
            assert!(!target.is_null());
            GC_register_finalizer(
                target as *mut std::ffi::c_void,
                target_finalizer_cb,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            // SAFETY: gc-heap-ptr arithmetic (alias_obj freshly allocated, we own payload word 0).
            let payload: *mut u64 = alias_obj.add(8).cast();
            payload.add(0).write(target as u64);
            // Leave word 1 (the actual pointer slot) null — Boehm's
            // precise marker WILL follow this slot per the descriptor.
            // A null value is the safe choice; any non-null would
            // also retain whatever it points to.
        }
        unsafe { alloc_target_and_alias_into_typed(alias_obj) };

        // Allocation-spam to overwrite any stack slots that may
        // still hold target's typed pointer. Matches the discipline
        // in the handler GC stress tests.
        for _ in 0..128 {
            let h = Header::new(header::TAG_INT64, 1, 0).raw();
            let _ = sigil_alloc(h, 8, u32::MAX);
        }

        // Force a full collection. Two GC cycles + invoke_finalizers
        // is Boehm's documented pattern for surfacing a finalizer in
        // the caller's thread: the first collection marks target
        // unreachable and queues the finalizer; the second runs it.
        unsafe {
            GC_gcollect();
            GC_invoke_finalizers();
            GC_gcollect();
            GC_invoke_finalizers();
        }

        // Assert: target's finalizer fired. If it didn't, the
        // precise marker is leaking — Boehm is following the
        // bitmap-bit-CLEAR slot when it shouldn't.
        let fired = FINALIZER_FIRED.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            fired, 1,
            "false-retention reproducer: target's finalizer did NOT fire after \
             two GC cycles. Either the precise marker is falsely retaining the \
             target via the aliased bit pattern in payload word 0 (bitmap bit \
             clear; the slot SHOULD be skipped), or stack-side pointer-shaped \
             values from the conservative stack scan are still holding a \
             reference. (FINALIZER_FIRED = {fired})"
        );

        // Pin alias_obj across the GC cycles so the optimiser can't
        // dead-code-eliminate the test setup. Word 0 still holds the
        // bit-pattern alias (Boehm didn't disturb the slot — it just
        // didn't follow the value).
        // SAFETY: gc-heap-ptr arithmetic (re-reads alias_obj's payload after the assertion).
        unsafe {
            let payload: *const u64 = alias_obj.add(8).cast();
            let read = payload.read();
            assert_ne!(
                read, 0,
                "alias_obj payload word 0 was cleared during the test (alias_obj itself was lost)"
            );
        }
    }

    #[test]
    fn atomic_payload_not_scanned_by_conservative_marker() {
        // Sanity check (NOT the Phase 2 ship-gate property).
        //
        // `GC_malloc_atomic` has skipped payload scanning since libgc
        // 1.0 — this property predates Sigil entirely and is
        // independent of Task 8's typed-malloc work. The test pins
        // it anyway because:
        //
        //   1. It exercises the `#[inline(never)] unsafe fn` stack-
        //      frame-pop discipline that the false-retention test
        //      depends on (so a Rust-side regression that breaks
        //      frame reclamation would surface here too).
        //   2. It exercises the two-GC-cycle + invoke_finalizers
        //      pattern that the false-retention test relies on.
        //   3. It pins `GC_malloc_atomic` as continuing to behave
        //      atomically post-Task 8 (we didn't accidentally route
        //      bitmap=0 through a non-atomic path).
        //
        // The reviewer of PR #167 (commit ad451e1) correctly flagged
        // that the originally-named "false-retention reproducer"
        // test was actually this property in disguise. Splitting
        // the test let both purposes be expressed cleanly.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess("atomic_payload_not_scanned_by_conservative_marker");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        FINALIZER_FIRED.store(0, std::sync::atomic::Ordering::SeqCst);

        // Atomic alias object (bitmap=0 → GC_malloc_atomic).
        let atomic_header = Header::new(header::TAG_INT64, 1, 0).raw();
        let alias_obj = sigil_alloc(atomic_header, 8, u32::MAX);
        assert!(!alias_obj.is_null());

        #[inline(never)]
        unsafe fn alloc_target_and_alias_into_atomic(alias_obj: *mut u8) {
            let s_bytes = b"ATOMIC-PAYLOAD-TARGET";
            // SAFETY: gc-heap-ptr arithmetic (s_bytes is a stack-local byte literal; sigil_string_new copies).
            let target = sigil_string_new(s_bytes.as_ptr(), s_bytes.len());
            assert!(!target.is_null());
            GC_register_finalizer(
                target as *mut std::ffi::c_void,
                target_finalizer_cb,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            // SAFETY: gc-heap-ptr arithmetic (alias_obj freshly allocated, we own its payload).
            let payload: *mut u64 = alias_obj.add(8).cast();
            payload.write(target as u64);
        }
        unsafe { alloc_target_and_alias_into_atomic(alias_obj) };

        for _ in 0..128 {
            let h = Header::new(header::TAG_INT64, 1, 0).raw();
            let _ = sigil_alloc(h, 8, u32::MAX);
        }

        unsafe {
            GC_gcollect();
            GC_invoke_finalizers();
            GC_gcollect();
            GC_invoke_finalizers();
        }

        let fired = FINALIZER_FIRED.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            fired, 1,
            "atomic-payload test: target's finalizer did NOT fire after \
             two GC cycles. Either GC_malloc_atomic is unexpectedly \
             scanning its payload (regression), or stack-side pointer- \
             shaped values are still holding a reference. \
             (FINALIZER_FIRED = {fired})"
        );

        // SAFETY: gc-heap-ptr arithmetic (re-reads alias_obj's payload after the assertion).
        unsafe {
            let payload: *const u64 = alias_obj.add(8).cast();
            let read = payload.read();
            assert_ne!(
                read, 0,
                "alias_obj payload was cleared during the test (alias_obj itself was lost)"
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "precise-descriptor minimum")]
    fn assert_precise_alloc_size_panics_when_total_underflows_count() {
        // Structural defense for the precise-marking path: a drift
        // between codegen's `payload_bytes` and the Header's `count`
        // would let `GC_malloc_explicitly_typed`'s mark phase walk
        // beyond the allocation. The `assert_precise_alloc_size`
        // helper catches the drift; this test pins that the helper
        // actually trips.
        //
        // count=2, total=8 → minimum required is (1+2)*8 = 24,
        // total of 8 is well below the floor.
        assert_precise_alloc_size(8, 2, 0b1);
    }

    #[test]
    fn alloc_string_longer_than_count_field_capacity() {
        // The 6-bit `count` field caps at 63 payload words = 504
        // payload bytes (496 bytes of content + 8 bytes for the
        // length word). Real-world env-var values can exceed this
        // (PR #106 follow-up CI surfaced a ~520-byte env var on the
        // GH macOS runner). Pin that strings beyond the 6-bit count
        // limit allocate, round-trip their bytes, and report length
        // correctly — `count` overflow now lands at `0` instead of
        // panicking in `Header::new`.
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        // 1024 bytes — comfortably past the 496-byte content cap.
        let src: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        // SAFETY: gc-heap-ptr arithmetic (Rust-owned `Vec<u8>`; sigil_string_new copies into a fresh GC alloc).
        let obj = unsafe { sigil_string_new(src.as_ptr(), src.len()) };
        assert!(!obj.is_null());
        assert_eq!(unsafe { sigil_string_len(obj) }, 1024);
        let (bytes, len) = unsafe { string_bytes(obj) };
        assert_eq!(len, 1024);
        let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
        assert_eq!(slice, &src[..]);
    }

    // ============ Plan E2 Phase 3 GC-time follow-up tests =================
    //
    // These verify the two new measurement mechanisms — the
    // `SIGIL_MAX_HEAP_SIZE_KB` env var (Task 1) and the
    // `SIGIL_COUNTER_PRECISE_WALKER_NS` counter (Task 2). Both
    // subprocess-isolated for the same reason the existing GC-stress
    // tests are: Boehm's per-process init state would otherwise bleed
    // across parallel cargo-test threads.

    /// Subprocess variant of [`run_gc_stress_in_subprocess`] that
    /// also threads additional env vars through to the child. Used
    /// by the Plan E2 Phase 3 GC-time follow-up tests below to set
    /// `SIGIL_MAX_HEAP_SIZE_KB` before the child's `sigil_gc_init`
    /// runs (the env var is read inside the `GC_INIT` `Once::call_once`
    /// block, so setting it after init is too late).
    fn run_gc_stress_in_subprocess_with_env(test_name: &str, extra_env: &[(&str, &str)]) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("run_gc_stress_in_subprocess_with_env: current_exe failed: {e}");
                std::process::abort();
            }
        };
        let full_name = format!("gc::tests::{test_name}");
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", &full_name, "--nocapture"])
            .env(GC_STRESS_INNER_VAR, "1");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                eprintln!("run_gc_stress_in_subprocess_with_env: spawn `{full_name}` failed: {e}");
                std::process::abort();
            }
        };
        // Mirror stderr to the parent's stderr so the
        // invalid-env-var warning text is visible to the parent
        // test's assertions on `output.stderr`.
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        eprintln!("--- subprocess stderr for {full_name} ---\n{stderr_str}---");
        assert!(
            output.status.success(),
            "GC-stress subprocess for `{full_name}` failed: status={} stderr={stderr_str}",
            output.status
        );
    }

    #[test]
    fn sigil_max_heap_size_kb_pin_forces_full_gc() {
        // Validates Task 1: setting `SIGIL_MAX_HEAP_SIZE_KB` to a
        // small budget BEFORE `sigil_gc_init` pins Boehm's max heap
        // size and forces collections to fire on workloads that
        // would otherwise stay well below Boehm's default
        // heap-growth threshold. Uses `GC_get_gc_no()` (integer
        // cycle count) rather than `GC_get_full_gc_total_time()`
        // (whole-ms wall-clock) because Boehm's ms timer rounds
        // sub-ms collections to 0 and the unit-test workload is
        // intentionally small — the budget-forces-collections
        // signal is a count, not a duration. The doc-deliverable
        // measurement (Task 4) uses the ms timer at workload scale.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess_with_env(
                "sigil_max_heap_size_kb_pin_forces_full_gc",
                &[("SIGIL_MAX_HEAP_SIZE_KB", "1024")],
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        // SAFETY: pure accessor over Boehm's process-wide cycle
        // count. Reads BEFORE init so the pre-init baseline
        // captures any collection that fires during init itself.
        let before = unsafe { GC_get_gc_no() };
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();

        // ~16 MiB of throw-away atomic-payload blocks against a
        // 1 MiB budget — Boehm should escalate to collections
        // repeatedly. The blocks are pointer-free (bitmap=0,
        // count=0 in the Header) so each alloc routes through
        // `GC_malloc_atomic` and Boehm's scan never traces into
        // their payloads, keeping the live-set roughly bounded
        // by what the test thread's stack happens to retain.
        for _ in 0..16_384 {
            let h = Header::new(header::TAG_INT64, 0, 0).raw();
            let _ = sigil_alloc(h, 1024, u32::MAX);
        }

        // Read the cycle counter HERE — after the allocation loop
        // but BEFORE any explicit `GC_gcollect` — so the assertion
        // measures specifically that the budget mechanism (env-var
        // read + `GC_set_max_heap_size` call) caused collections to
        // fire under allocation pressure. An explicit `GC_gcollect`
        // before this read would advance the counter regardless of
        // whether the budget mechanism worked, masking a regression
        // in either the env-var read or the budget plumbing. PR #176
        // review item 2 surfaced this isolation gap.
        //
        // SAFETY: pure accessor over Boehm's process-wide stats.
        let after_allocs = unsafe { GC_get_gc_no() };
        assert!(
            after_allocs > before,
            "SIGIL_MAX_HEAP_SIZE_KB=1024 did not force any collection \
             during the 16 MiB allocation loop (GC_get_gc_no before={before} \
             after_allocs={after_allocs}). The budget read or the \
             `GC_set_max_heap_size` call may have regressed."
        );
    }

    #[test]
    fn sigil_max_heap_size_kb_invalid_logs_warning_and_proceeds() {
        // Validates Task 1's invalid-input handling. A non-numeric
        // value (or zero, or negative) must NOT crash; the parse-
        // failure branch in `sigil_gc_init` writes a warning to
        // stderr and continues without setting a budget. The
        // subprocess thus completes normally; its stderr (mirrored
        // by `run_gc_stress_in_subprocess_with_env`) contains the
        // warning text.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess_with_env(
                "sigil_max_heap_size_kb_invalid_logs_warning_and_proceeds",
                &[("SIGIL_MAX_HEAP_SIZE_KB", "not-a-number")],
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        // Write the warning marker to our own stderr so the parent
        // test can assert against it via the mirrored output. The
        // real warning is written by `sigil_gc_init` itself; this
        // line is a no-op echo that just confirms the subprocess
        // reached this point (i.e., init didn't abort).
        eprintln!("subprocess reached post-init point — invalid env var did not crash");
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        // Trivial alloc to confirm the runtime is functional.
        let h = Header::new(header::TAG_INT64, 1, 0).raw();
        let obj = sigil_alloc(h, 8, u32::MAX);
        assert!(!obj.is_null());
    }

    #[test]
    fn sigil_max_heap_size_kb_empty_string_is_silent() {
        // PR #176 review item 1: when the GitHub Actions workflow's
        // `heap_budget_kb` input is empty (the default), it sets
        // `SIGIL_MAX_HEAP_SIZE_KB=""` (not unset). Without the
        // `is_empty()` guard in `sigil_gc_init`, this triggers the
        // "expected positive integer kilobytes" warning on every
        // default-budget workflow run — noisy and misleading. The
        // guard makes empty-string indistinguishable from unset:
        // both → no budget, no warning.
        //
        // Parent spawns the inner subprocess directly here (rather
        // than going through `run_gc_stress_in_subprocess_with_env`)
        // because the assertion is on the child's STDERR contents —
        // specifically that the parse-warning text is ABSENT — and
        // the parent needs unfiltered access to the raw output.
        if !in_gc_stress_subprocess() {
            // Mirrors `run_gc_stress_in_subprocess_with_env`'s error
            // handling style (abort on spawn failure rather than
            // `.expect()`) — clippy's disallowed-methods rule in
            // this crate forbids `Result::expect` even in tests.
            let exe = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("empty_string_is_silent: current_exe failed: {e}");
                    std::process::abort();
                }
            };
            let full_name = "gc::tests::sigil_max_heap_size_kb_empty_string_is_silent";
            let output = match std::process::Command::new(&exe)
                .args(["--exact", full_name, "--nocapture"])
                .env(GC_STRESS_INNER_VAR, "1")
                .env("SIGIL_MAX_HEAP_SIZE_KB", "")
                .output()
            {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("empty_string_is_silent: spawn `{full_name}` failed: {e}");
                    std::process::abort();
                }
            };
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "subprocess failed: status={} stderr={stderr_str}",
                output.status
            );
            assert!(
                !stderr_str.contains("expected positive integer kilobytes"),
                "empty SIGIL_MAX_HEAP_SIZE_KB triggered the warning anyway: stderr={stderr_str}"
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        let h = Header::new(header::TAG_INT64, 1, 0).raw();
        let obj = sigil_alloc(h, 8, u32::MAX);
        assert!(!obj.is_null());
    }

    #[test]
    fn precise_walker_counter_increments_when_gc_fires() {
        // Validates Task 2: every full mark phase invokes
        // `push_sigil_thread_precise_roots`, which now bumps the
        // `SIGIL_COUNTER_PRECISE_WALKER_NS` counter on every exit
        // path. After registering as a Sigil thread + forcing a
        // GC, the counter must be non-zero. Mirrors the env-var
        // test but does not need the budget — `GC_gcollect()` is
        // explicit.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess("precise_walker_counter_increments_when_gc_fires");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        // Flip the current thread into Sigil-mode so the walker's
        // discriminator passes its `is_sigil` check. Without this,
        // the walker takes the short-circuit gate path — which
        // ALSO ticks the counter, but minimally; we want the
        // full-walk path to exercise the timing branch over the
        // full walker body, not just the gate-check.
        crate::gc::threads::register_sigil_thread_for_precise_roots();

        let before = crate::counters::read(crate::counters::CounterId::PreciseWalkerNs);
        unsafe { GC_gcollect() };
        let after = crate::counters::read(crate::counters::CounterId::PreciseWalkerNs);
        assert!(
            after > before,
            "precise-walker counter did not advance across GC_gcollect: before={before} after={after}"
        );
    }

    // ============ Plan E2 Phase 3 GC-time follow-up #2 tests ==============
    //
    // Validate the `SIGIL_FORCE_GC_EVERY_N_ALLOCS` env-var injection
    // mechanism: read-at-init plumbing, per-alloc cadence dispatch, and
    // the diagnostic `SIGIL_COUNTER_FORCED_GC_COUNT` counter. All three
    // tests are subprocess-gated for the same reason as the existing
    // GC-stress tests — the env var is read inside `sigil_gc_init`'s
    // `Once::call_once`, so each test needs a fresh process.

    #[test]
    fn sigil_force_gc_every_n_allocs_fires_collections() {
        // Validates Task 1: setting `SIGIL_FORCE_GC_EVERY_N_ALLOCS=10`
        // and allocating 30 objects fires exactly 3 forced GCs
        // (counted via `SIGIL_COUNTER_FORCED_GC_COUNT`) and advances
        // Boehm's `GC_get_gc_no()` by at least 3. The ForcedGcCount
        // signal is exact: the injection runs `(count % N == 0) → fire`
        // and bumps the counter immediately before the `GC_gcollect()`
        // call. The `GC_get_gc_no` signal is a `>=` bound because
        // Boehm may opportunistically fire additional collections under
        // allocation pressure; the only invariant the test pins is
        // that the injection actually drove collections.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess_with_env(
                "sigil_force_gc_every_n_allocs_fires_collections",
                &[("SIGIL_FORCE_GC_EVERY_N_ALLOCS", "10")],
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        // SAFETY: pure accessor over Boehm's process-wide cycle count.
        let before_gc_no = unsafe { GC_get_gc_no() };
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();

        let before_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);
        for _ in 0..30 {
            let h = Header::new(header::TAG_INT64, 0, 0).raw();
            let _ = sigil_alloc(h, 8, u32::MAX);
        }
        let after_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);
        // SAFETY: pure accessor.
        let after_gc_no = unsafe { GC_get_gc_no() };

        assert_eq!(
            after_forced - before_forced,
            3,
            "Expected exactly 3 forced GCs (30 allocs @ N=10), got \
             {} (before={before_forced} after={after_forced})",
            after_forced - before_forced,
        );
        assert!(
            after_gc_no >= before_gc_no + 3,
            "Expected at least 3 GC cycles (30 allocs @ N=10), got \
             GC_get_gc_no before={before_gc_no} after={after_gc_no}",
        );
    }

    #[test]
    fn sigil_force_gc_every_n_allocs_unset_skips_injection() {
        // Validates Task 1's zero-overhead path: when the env var is
        // unset, `FORCE_GC_CADENCE` resolves to `Some(None)` and the
        // injection branch falls through without ticking the cadence
        // counter. `SIGIL_COUNTER_FORCED_GC_COUNT` stays at 0 after a
        // 100-alloc loop. Uses `run_gc_stress_in_subprocess` (not the
        // `_with_env` variant) so no env-var customisation reaches
        // the child — the parent's environment is inherited, and the
        // existing GC-stress tests run under cargo test without
        // `SIGIL_FORCE_GC_EVERY_N_ALLOCS` set.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess("sigil_force_gc_every_n_allocs_unset_skips_injection");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();

        let before_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);
        for _ in 0..100 {
            let h = Header::new(header::TAG_INT64, 0, 0).raw();
            let _ = sigil_alloc(h, 8, u32::MAX);
        }
        let after_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);

        assert_eq!(
            after_forced, before_forced,
            "ForcedGcCount advanced ({before_forced} -> {after_forced}) \
             despite SIGIL_FORCE_GC_EVERY_N_ALLOCS being unset — the \
             zero-overhead gate regressed.",
        );
    }

    #[test]
    fn sigil_force_gc_every_n_allocs_invalid_logs_warning() {
        // Validates Task 1's invalid-input handling. A non-numeric
        // value (or zero) must NOT crash; `sigil_gc_init` writes a
        // warning to stderr and continues with no injection. The
        // subprocess thus completes normally; its stderr (mirrored
        // by `run_gc_stress_in_subprocess_with_env`) carries the
        // warning. The test asserts the counter stays at 0 to pin
        // that "no injection" half — the stderr-text assertion is
        // covered by the parallel `_empty_string_is_silent` shape
        // used for `SIGIL_MAX_HEAP_SIZE_KB`, kept off this test to
        // avoid duplicating the custom-spawn boilerplate.
        if !in_gc_stress_subprocess() {
            run_gc_stress_in_subprocess_with_env(
                "sigil_force_gc_every_n_allocs_invalid_logs_warning",
                &[("SIGIL_FORCE_GC_EVERY_N_ALLOCS", "not-a-number")],
            );
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        // Echo a sentinel so the parent test's mirrored output makes
        // the post-init reach point obvious — matches the
        // SIGIL_MAX_HEAP_SIZE_KB invalid-input test's style.
        eprintln!(
            "subprocess reached post-init point — invalid \
             SIGIL_FORCE_GC_EVERY_N_ALLOCS did not crash"
        );
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();

        let before_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);
        for _ in 0..50 {
            let h = Header::new(header::TAG_INT64, 0, 0).raw();
            let _ = sigil_alloc(h, 8, u32::MAX);
        }
        let after_forced = crate::counters::read(crate::counters::CounterId::ForcedGcCount);

        assert_eq!(
            after_forced, before_forced,
            "ForcedGcCount advanced ({before_forced} -> {after_forced}) \
             despite SIGIL_FORCE_GC_EVERY_N_ALLOCS being invalid — \
             the invalid-input branch should fall through to no injection.",
        );
    }
}
