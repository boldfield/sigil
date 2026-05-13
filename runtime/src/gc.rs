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
use std::sync::Once;

use crate::counters::{self, sigil_counter_print_all, CounterId};
use crate::header::{self, Header};

// Direct Boehm FFI — we do not depend on a Rust wrapper crate. These are
// the stable symbols exported by libgc.
#[link(name = "gc")]
extern "C" {
    fn GC_init();
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
    // pointer is reliably collected and the test trips. Not called by
    // production code paths; gated to test builds so the extern linkage
    // is not pulled into release binaries.
    #[cfg(test)]
    pub(crate) fn GC_gcollect();
    // Boehm thread enrolment used by GC stress tests in this crate. A
    // Rust test thread is not auto-registered with Boehm (see
    // `test_support` module for the historical context); calling
    // `GC_gcollect` from such a thread triggers Boehm's "Collecting
    // from unknown thread" abort. Tests that need to force collection
    // must enrol their thread first.
    #[cfg(test)]
    pub(crate) fn GC_allow_register_threads();
    #[cfg(test)]
    pub(crate) fn GC_register_my_thread(stack_base: *const c_void) -> i32;
    #[cfg(test)]
    pub(crate) fn GC_unregister_my_thread() -> i32;

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

    // Boehm typed allocator — Plan E2 Phase 2 Task 8. Allocates
    // `size_in_bytes` bytes from Boehm's heap and tags the block
    // with `descr` so the mark phase scans payload words precisely
    // per the descriptor's pointer bitmap. The returned block is
    // zero-initialised and 8-byte aligned (same as `GC_malloc_atomic`).
    // `size_in_bytes` must be `>= len_bits * sizeof(GC_word)` —
    // the descriptor's bitmap must cover the entire allocation.
    fn GC_malloc_explicitly_typed(size_in_bytes: usize, descr: usize) -> *mut c_void;
}

pub(crate) mod descriptor;

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
        // SAFETY: `Once::call_once` guarantees exactly one invocation even
        // under concurrent entry, so Boehm's non-reentrant init runs on a
        // single thread.
        unsafe { GC_init() };

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
pub extern "C" fn sigil_alloc(header: u64, payload_bytes: usize) -> *mut u8 {
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

    let h = Header(header);
    // Allocator selection. Plan E2 Phase 2 Task 8 splits the bitmap
    // dispatch into three branches based on what the Header's
    // `(count, bitmap)` pair encodes:
    //
    //   - `bitmap == 0`                 → `GC_malloc_atomic`
    //     No GC pointers anywhere in the payload. Boehm skips
    //     scanning entirely — strictly better than precise marking.
    //
    //   - `bitmap != 0 && count == 0`   → `GC_malloc` (conservative)
    //     The Sigil convention for "object too large for the
    //     header's 6-bit count field to describe precisely" — arrays,
    //     mut-arrays, the string-builder segments table. Count=0 is
    //     the structural signal; bitmap=non-zero routes out of the
    //     atomic path. These payloads are scanned conservatively;
    //     element pointers survive GC because Boehm's mark phase
    //     walks the whole block looking for pointer-shaped values.
    //
    //   - `bitmap != 0 && count >  0`   → `GC_malloc_explicitly_typed`
    //     Precise marking via Boehm's typed-malloc. The descriptor
    //     cache (Task 7) hands out one `GC_descr` per shape; first
    //     observation of a `(bitmap, count)` shape builds the
    //     descriptor via `GC_make_descriptor`, subsequent calls
    //     reuse the cached handle. The precondition check lives in
    //     `assert_precise_alloc_size` — see its doc-comment.
    //
    // The count==0 branch closes a silent correctness regression
    // introduced by Task 8's initial drop of `GC_malloc`: a typed
    // descriptor built from `(bitmap=1, count=0)` would have
    // `len_bits = 1` describing only the header word; Boehm's
    // tile-replication would then treat every element slot as a
    // non-pointer, silently collecting any heap-bearing array
    // elements that lacked an independent stack root.
    let raw = if h.pointer_bitmap() == 0 {
        unsafe { GC_malloc_atomic(total) as *mut u8 }
    } else if h.payload_count() == 0 {
        unsafe { GC_malloc(total) as *mut u8 }
    } else {
        let count = h.payload_count();
        assert_precise_alloc_size(total, count, h.pointer_bitmap());
        let descr = descriptor::get_or_create(h.pointer_bitmap(), count);
        // SAFETY: `descr` was built by `GC_make_descriptor` and is
        // alive for the process lifetime (the cache never evicts).
        // `total` meets the descriptor's `len_bits * sizeof(GC_word)`
        // floor (debug_asserted above for non-release builds).
        unsafe { GC_malloc_explicitly_typed(total, descr) as *mut u8 }
    };

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
    raw
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
    let obj = sigil_alloc(h.raw(), payload_bytes);

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
    fn sigil_alloc_routes_nonzero_bitmap_through_descriptor_cache() {
        // Plan E2 Phase 2 Task 8 three-branch wiring proof.
        //   - bitmap=0           → GC_malloc_atomic (cache untouched)
        //   - bitmap!=0, count=0 → GC_malloc        (cache untouched)
        //   - bitmap!=0, count>0 → GC_malloc_explicitly_typed (cache+1)
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        descriptor::clear_cache();
        assert_eq!(descriptor::cache_size(), 0, "cache must start empty");

        // Bitmap=0 path: should NOT touch the cache.
        let zero_bitmap_header = Header::new(header::TAG_INT64, 1, 0).raw();
        let obj_atomic = sigil_alloc(zero_bitmap_header, 8);
        assert!(!obj_atomic.is_null());
        assert_eq!(
            descriptor::cache_size(),
            0,
            "bitmap=0 alloc must not populate the descriptor cache"
        );

        // count=0, bitmap!=0 path (arrays / mut-arrays / segments
        // table): should route to plain GC_malloc, NOT the typed
        // path. Cache untouched.
        let array_header = Header::new(header::TAG_ARRAY, 0, 1).raw();
        let obj_array = sigil_alloc(array_header, 32); // length word + 3 elements
        assert!(!obj_array.is_null());
        assert_eq!(
            descriptor::cache_size(),
            0,
            "count=0 + bitmap!=0 alloc must route to conservative GC_malloc, not typed path"
        );

        // Bitmap=0b1 path: should populate the cache with one entry.
        let one_ptr_header = Header::new(header::TAG_REF, 1, 0b1).raw();
        let obj_precise = sigil_alloc(one_ptr_header, 8);
        assert!(!obj_precise.is_null());
        assert_eq!(
            descriptor::cache_size(),
            1,
            "non-zero bitmap + count>0 alloc must populate the descriptor cache"
        );

        // Second alloc of the same shape: cache size unchanged.
        let obj_precise_2 = sigil_alloc(one_ptr_header, 8);
        assert!(!obj_precise_2.is_null());
        assert_eq!(
            descriptor::cache_size(),
            1,
            "repeat-shape alloc must reuse the cached descriptor"
        );

        // Distinct shape: cache size grows to 2. A closure with one
        // env slot — `count=2` (code_ptr at word 0 + env_slot_0 at
        // word 1), `bitmap=0b10` (only env_slot_0 is a pointer).
        let closure_header = Header::new(header::TAG_CLOSURE, 2, 0b10).raw();
        let obj_closure = sigil_alloc(closure_header, 16);
        assert!(!obj_closure.is_null());
        assert_eq!(
            descriptor::cache_size(),
            2,
            "distinct-shape alloc must add a fresh cache entry"
        );
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
        let array_obj = sigil_alloc(array_header.raw(), payload_bytes);
        assert!(!array_obj.is_null());

        // SAFETY: array_obj is a fresh allocation; we own the full
        // payload range for initialisation.
        unsafe {
            let len_ptr: *mut u64 = array_obj.add(8).cast();
            len_ptr.write(8);
            let elems_ptr = array_obj.add(16) as *mut *mut u8;
            for i in 0..8u8 {
                let s_bytes = [0xA0u8 + i, 0xA1u8 + i, 0xA2u8 + i];
                let s_obj =
                    sigil_string_new(s_bytes.as_ptr(), s_bytes.len());
                assert!(!s_obj.is_null());
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
}
