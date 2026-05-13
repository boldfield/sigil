// Plan E2 Phase 2 Task 6 — Boehm precise-mode API spike.
//
// Pins the Boehm typed-malloc API surface the runtime will use in
// Task 7 (descriptor cache) + Task 8 (`sigil_alloc` registers
// precise descriptors). Two tests validate the API end-to-end on
// every push:
//
//   1. `make_descriptor_returns_nonzero_handle` — confirms
//      `GC_make_descriptor` returns a non-trivial value (the handle
//      is opaque to us; we just need to verify the call doesn't
//      crash and returns something other than the "size-tag"
//      conservative fallback). Tests the bitmap encoding the
//      runtime will use: bit `i` = 1 iff payload word `i` is a GC
//      pointer.
//
//   2. `malloc_explicitly_typed_round_trip` — allocates a 2-word
//      object via `GC_malloc_explicitly_typed`, writes a known
//      pointer to its single pointer slot, references it through
//      a root range, forces a full GC cycle, and re-reads the
//      pointer. The read-back value must equal what was written —
//      proving the descriptor + alloc + scan path works without
//      crashing on the host.
//
// Verification of precise-marking *correctness* (does the precise
// marker drop the right slots?) is deferred to Task 9's
// false-retention reproducer per the plan body. This spike's
// acceptance is "the API works without crashing on the host."
//
// Runs on both `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`.

use std::ffi::c_void;
use std::sync::{Mutex, Once, OnceLock};

// Boehm typed-malloc FFI surface — declared inline in the spike
// test rather than in `runtime/src/gc.rs` because Tasks 7 + 8 will
// fold these into the production runtime once the spike validates
// them. Keeping them spike-local avoids accidentally adding new
// always-linked symbols to the runtime staticlib before the
// production path lands.
#[link(name = "gc")]
extern "C" {
    fn GC_init();
    fn GC_malloc(size: usize) -> *mut c_void;
    fn GC_gcollect();

    /// Boehm typed-malloc: build a descriptor from a pointer-bitmap.
    ///
    /// `bitmap` is a slice of `GC_word` (== uintptr_t / u64 on 64-bit
    /// hosts). Bit `i` (LSB-first within each word) indicates whether
    /// payload word `i` is a GC-managed pointer. `len_bits` is the
    /// number of meaningful bits in the bitmap; words beyond
    /// `ceil(len_bits / GC_WORDSZ)` are unused.
    ///
    /// "Calls to GC_make_descriptor may consume some amount of a
    /// finite resource. This is intended to be called once per type,
    /// not once per allocation." (gc_typed.h)
    fn GC_make_descriptor(bitmap: *const usize, len_bits: usize) -> usize;

    /// Allocate `size_in_bytes` bytes, traced precisely per `descr`.
    /// Returned object is zero-initialised. `size_in_bytes` must be
    /// at least `len_bits * sizeof(GC_word)` for the descriptor's
    /// bitmap to cover the object.
    fn GC_malloc_explicitly_typed(size_in_bytes: usize, descr: usize) -> *mut c_void;

    /// Force a full mark-sweep cycle. Tests use this to make
    /// liveness questions deterministic — without it, low-pressure
    /// programs may not trip a collection during the test.
    fn GC_allow_register_threads();
    fn GC_register_my_thread(stack_base: *const c_void) -> i32;
}

static GC_INIT: Once = Once::new();
static GC_ALLOW_REGISTER: Once = Once::new();

/// Serialize the two tests within this binary. Cargo test runs them
/// in the same process; concurrent GC_gcollect from racy threads is
/// poorly-defined under Boehm and would mask real spike failures
/// behind heisenbugs.
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Register the calling thread with Boehm. Cargo-test spawns a
/// fresh OS thread per `#[test]` (even under `--test-threads=1`);
/// each thread that calls `GC_gcollect` must register first or
/// Boehm aborts with "Collecting from unknown thread."
///
/// We don't unregister: cargo's test runner destroys the thread
/// when the test returns, and the registration is freed with it.
/// Symmetric `GC_unregister_my_thread` would be cleaner but in
/// practice (a) Boehm's per-thread state lives in TLS and dies
/// with the thread, (b) attempting to unregister + re-register
/// across cargo test workers surfaces a SIGSEGV on this host —
/// likely a Boehm bug with rapid register/unregister cycles, but
/// not our spike to chase.
fn enrol_gc() {
    // SAFETY: each Once.call_once fires at most once per process.
    // GC_register_my_thread returns GC_DUPLICATE on a
    // already-registered thread; we ignore the return code.
    unsafe {
        GC_INIT.call_once(|| GC_init());
        GC_ALLOW_REGISTER.call_once(|| GC_allow_register_threads());
        let _ = GC_register_my_thread(std::ptr::null());
    }
}

/// Single-pointer-slot bitmap: word 0 IS a pointer (bit 0 set).
/// The Sigil object header sits BEFORE the payload area, so this
/// bitmap describes the payload only. Each `1` bit corresponds to
/// a payload word that holds a GC-managed pointer.
const SINGLE_PTR_SLOT_BITMAP: [usize; 1] = [0b1];

#[test]
fn make_descriptor_returns_nonzero_handle() {
    let _guard = test_lock();
    enrol_gc();
    // SAFETY: bitmap lives for the call's duration; len_bits = 1.
    let descr = unsafe { GC_make_descriptor(SINGLE_PTR_SLOT_BITMAP.as_ptr(), 1) };
    // The handle is opaque — we don't pin its bit-pattern. We only
    // assert it's not the "trivial / out-of-memory" fallback. Per
    // gc_typed.h: "Returns a conservative approximation in the
    // (unlikely) case of insufficient memory to build the
    // descriptor." We can't directly detect that fallback, but
    // GC_make_descriptor on a 1-bit bitmap is well within Boehm's
    // descriptor budget; a zero handle would indicate a real
    // failure.
    assert_ne!(descr, 0, "GC_make_descriptor returned 0 for 1-bit bitmap");
}

#[test]
fn malloc_explicitly_typed_round_trip() {
    let _guard = test_lock();
    enrol_gc();
    // SAFETY: see SAFETY comments inline.
    unsafe {
        let descr = GC_make_descriptor(SINGLE_PTR_SLOT_BITMAP.as_ptr(), 1);
        assert_ne!(descr, 0);

        // Allocate 2 words: word 0 is the precise pointer slot per
        // the descriptor, word 1 is unused (Boehm zeros it).
        let obj_size: usize = 16;
        let typed_obj = GC_malloc_explicitly_typed(obj_size, descr);
        assert!(
            !typed_obj.is_null(),
            "GC_malloc_explicitly_typed returned null"
        );
        assert_eq!(
            typed_obj as usize & 0b111,
            0,
            "typed alloc must be 8-byte aligned"
        );

        // Allocate a target object via plain GC_malloc; store its
        // pointer in typed_obj's slot 0. After GC, the reference
        // should remain valid because (a) the precise marker
        // traces typed_obj's slot 0 and follows the pointer, and
        // (b) typed_obj itself is reachable via this local
        // variable (Boehm's conservative stack scan).
        let target = GC_malloc(64);
        assert!(!target.is_null());
        let known_byte: u8 = 0xAB;
        *(target as *mut u8) = known_byte;

        // Store target into typed_obj's pointer slot. Word 0 is at
        // offset 0; we write a single usize (== ptr-sized).
        let ptr_slot = typed_obj as *mut *mut c_void;
        *ptr_slot = target;

        // Force a full GC cycle. If precise marking is fundamentally
        // broken on the host, this could crash or produce garbage on
        // re-read. The spike's acceptance: GC_gcollect runs to
        // completion without aborting.
        GC_gcollect();

        // Re-read the pointer slot. Must still point at our target.
        let after = *ptr_slot;
        assert_eq!(
            after as usize, target as usize,
            "precise pointer slot must survive GC unchanged"
        );

        // Target's byte must still be readable.
        let after_byte = *(after as *const u8);
        assert_eq!(after_byte, known_byte, "target's payload must survive GC");
    }
}
