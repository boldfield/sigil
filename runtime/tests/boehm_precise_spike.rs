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
// Runs on `x86_64-unknown-linux-gnu` (ubuntu-24.04 CI) and
// `aarch64-apple-darwin` (macos-14 CI).
//
// # Why each test runs in its own subprocess
//
// Cargo-test spawns a fresh OS thread per `#[test]` (even under
// `--test-threads=1`) and tears it down before the next test
// starts. Boehm's per-thread mark state survives that thread's
// destruction — the next test's `GC_gcollect` then walks a freed
// thread record, surfacing as `signal: 11, SIGSEGV` on Linux or
// `thread_suspend failed` / SIGABRT on Darwin. Symmetric
// `GC_unregister_my_thread` does not fully clean up the per-thread
// record in libgc 8.x under rapid register / unregister / re-register
// cycles (reproduced both on the pod and on CI, both with and
// without the symmetric unregister).
//
// The runtime's own GC stress tests already work around this with a
// subprocess-per-test trick (`runtime/src/handlers.rs::run_stress_in_subprocess`,
// gated on the `SIGIL_GC_STRESS_INNER` env var). The spike applies
// the same pattern: outer-mode invocations re-exec the test binary
// filtered to `--exact <test_name>` with `SIGIL_BOEHM_SPIKE_INNER=1`
// set; the child runs exactly one test in its own fresh process and
// exits. Result: only one Boehm thread registration per process, no
// cross-test pollution.

use std::ffi::c_void;
use std::sync::Once;

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

    /// Per-thread Boehm enrolment. `GC_allow_register_threads` must
    /// be called once on the program at large before any thread that
    /// will register itself is created (in practice, before the
    /// first `GC_register_my_thread`). `GC_register_my_thread(NULL)`
    /// asks Boehm to auto-detect the calling thread's stack base —
    /// the documented form for threads not created via
    /// `GC_pthread_create` (which `std::thread::spawn` does not call).
    fn GC_allow_register_threads();
    fn GC_register_my_thread(stack_base: *const c_void) -> i32;
}

// Boehm return-code constants (from `gc.h`). We pin the values we
// care about as `const`s so the assertion below is self-documenting.
const GC_SUCCESS: i32 = 0;
const GC_DUPLICATE: i32 = 1;

/// Env-var marker that switches a spike test into "inner" mode
/// (run the actual body) instead of "outer" mode (spawn a child
/// subprocess that runs only this one test). Mirrors the runtime
/// crate's `SIGIL_GC_STRESS_INNER` pattern in
/// `runtime/src/handlers.rs`.
const SPIKE_INNER_VAR: &str = "SIGIL_BOEHM_SPIKE_INNER";

fn in_spike_subprocess() -> bool {
    std::env::var(SPIKE_INNER_VAR).is_ok()
}

/// Outer-mode helper: re-exec this integration-test binary, filtered
/// to the given `--exact` test name with the inner-mode env var set.
/// Asserts the child exited zero. Integration-test binaries do NOT
/// carry a module prefix (test functions live at the top level of
/// the file), so the test name passed in is the bare function name.
fn run_spike_in_subprocess(test_name: &str) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("run_spike_in_subprocess: current_exe failed: {e}");
            std::process::abort();
        }
    };
    let status = match std::process::Command::new(&exe)
        .args(["--exact", test_name, "--nocapture"])
        .env(SPIKE_INNER_VAR, "1")
        .status()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("run_spike_in_subprocess: spawn for `{test_name}` failed: {e}");
            std::process::abort();
        }
    };
    assert!(
        status.success(),
        "Boehm-spike subprocess for `{test_name}` failed: {status}"
    );
}

static GC_INIT: Once = Once::new();
static GC_ALLOW_REGISTER: Once = Once::new();

/// One-shot per-process Boehm enrolment for the inner subprocess.
/// Only one test ever runs in each subprocess (see module doc), so
/// there is no register / unregister / re-register churn to defend
/// against. We keep the explicit `GC_init` to match the production
/// `sigil_gc_init` pattern in `runtime/src/gc.rs` — if init ever
/// diverges between the spike and production on a host, the spike
/// surfaces it as an attributable failure rather than as a confused
/// later-allocation error.
///
/// `GC_allow_register_threads` ordering: libgc docs say it must
/// precede any thread that will register itself. The cargo-test
/// runner's worker thread has already been created by the time
/// this fires, but it hasn't yet attempted registration — the
/// Once enforces ordering with respect to the call site, not the
/// thread's creation, which is what libgc actually cares about.
/// The runtime's `test_support` module uses the same pattern with
/// no observed issues.
///
/// We don't unregister: only one test runs per subprocess and the
/// process exits cleanly when the test returns, at which point the
/// OS reaps the thread + libgc's state with it.
fn enrol_gc() {
    // SAFETY: each Once.call_once fires at most once per process.
    unsafe {
        GC_INIT.call_once(|| GC_init());
        GC_ALLOW_REGISTER.call_once(|| GC_allow_register_threads());
    }
    // SAFETY: NULL stack base = Boehm auto-detects the calling
    // thread's stack bottom (documented form for non-Boehm-created
    // threads). Return code:
    //   GC_SUCCESS (0)    — first registration on this thread; good.
    //   GC_DUPLICATE (1)  — thread already registered; also good (idempotent).
    //   GC_UNIMPLEMENTED (3) or other — registration unavailable on
    //                      this host. Abort with diagnostic rather than
    //                      silently SIGSEGV on the first GC_gcollect.
    let rc = unsafe { GC_register_my_thread(std::ptr::null()) };
    assert!(
        rc == GC_SUCCESS || rc == GC_DUPLICATE,
        "GC_register_my_thread returned rc={rc} \
         (expected GC_SUCCESS=0 or GC_DUPLICATE=1)"
    );
}

/// Single-pointer-slot bitmap: word 0 IS a pointer (bit 0 set).
/// The Sigil object header sits BEFORE the payload area, so this
/// bitmap describes the payload only. Each `1` bit corresponds to
/// a payload word that holds a GC-managed pointer.
const SINGLE_PTR_SLOT_BITMAP: [usize; 1] = [0b1];

#[test]
fn make_descriptor_returns_nonzero_handle() {
    if !in_spike_subprocess() {
        run_spike_in_subprocess("make_descriptor_returns_nonzero_handle");
        return;
    }
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
    if !in_spike_subprocess() {
        run_spike_in_subprocess("malloc_explicitly_typed_round_trip");
        return;
    }
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
