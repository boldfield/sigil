//! Trampoline arena allocator — Plan B Task 56.
//!
//! Backs the per-dispatch bump arena that holds `NextStep` records in the
//! CPS trampoline (`sigil_run_loop`). Reset at the top of every loop
//! iteration; only values that explicitly escape a step are copied to the
//! Boehm heap (Task 55 wires the promotion sites).
//!
//! # Layout
//!
//! A single `Vec<u64>` per thread, treated as a bump region. Backing the
//! storage with `u64` instead of `u8` gives the `Vec`'s allocation a
//! natural 8-byte base alignment without depending on the system
//! allocator's `malloc` returning aligned blocks (which is true on every
//! platform Sigil targets, but not a Rust guarantee). v1 is
//! single-threaded, but the `thread_local!` keeps the API
//! forward-compatible with a multi-threaded v2 trampoline that would
//! shard the arena per thread anyway.
//!
//! All `sigil_arena_alloc` returns are 8-byte aligned (the Sigil
//! heap-object alignment) by construction: the underlying word count
//! advances in u64 units, and the byte-level pointer arithmetic
//! preserves alignment through every alloc.
//!
//! # Capacity discipline
//!
//! The arena pre-reserves `INITIAL_CAPACITY_WORDS` u64 slots on first
//! use. Once reserved, allocations are non-reallocating: the underlying
//! `Vec` grows its `len` within the existing capacity using `set_len`.
//! This guarantees pointers returned by earlier `sigil_arena_alloc`
//! calls in the same iteration remain valid through subsequent
//! allocations, which the trampoline relies on (a `sigil_perform` site
//! may produce a `NextStep` while the surrounding caller still holds a
//! pointer into the arena).
//!
//! Exceeding the reserved capacity within a single iteration aborts
//! with a clear message rather than reallocating. v1's CPS-color
//! workloads (fib(20) under a forced effect row, multi-shot Choose
//! demos) fit comfortably under the default budget; if a future
//! workload outgrows it, raise `INITIAL_CAPACITY_WORDS` rather than
//! relax the non-reallocating invariant.
//!
//! Allocation uses `try_reserve_exact` rather than `reserve`. `reserve`
//! panics on OOM; this module's surface is `extern "C"`, and Rust
//! workspaces with the default `panic = "unwind"` policy invoke
//! undefined behavior when a panic crosses a `extern "C"` boundary. The
//! `try_reserve_exact` failure path aborts cleanly via the same
//! `eprintln! + abort()` pattern as the overflow path.
//!
//! # Reentrancy contract
//!
//! `ARENA` is a `RefCell<Vec<u64>>`; reentering `sigil_arena_alloc` (or
//! `sigil_arena_reset`) while another `borrow_mut` is live panics. The
//! trampoline (`sigil_run_loop`) upholds this by reading the current
//! `NextStep`'s dispatch info into stack locals AND calling
//! `sigil_arena_reset` BEFORE invoking the carried `cps_fn`. The
//! `cps_fn` is then free to allocate against a fresh arena window.
//! Plan B v1 has no path that nests `sigil_arena_alloc` calls within a
//! single trampoline iteration; codegen (Task 55) preserves this by
//! emitting a single `NextStep` allocation per cps_fn return.
//!
//! # GC reachability
//!
//! `NextStep::Call` records hold `closure_ptr` and (eventually, via the
//! args buffer) potentially heap-tagged user values. The arena's
//! backing storage is on the system allocator's heap, NOT on Boehm's
//! heap, and Boehm's automatic stack/data-segment scan does not cover
//! it. Without explicit rooting, a Boehm-allocated closure referenced
//! ONLY through an arena slot can be reclaimed mid-iteration.
//!
//! Plan B Task 56 fixes this by registering the arena's storage range
//! `[start, start + capacity * 8)` with `GC_add_roots` once per thread,
//! triggered from `sigil_gc_init` via
//! `register_arena_root_for_calling_thread`. Boehm then scans the
//! range conservatively each mark phase, finding any pointer-shaped
//! u64 in the arena and following it.
//!
//! Conservative scanning of arena bytes admits some pinning of
//! Boehm-heap blocks whose addresses happen to alias non-pointer u64
//! values (raw user `Int` args, packed continuation envelopes, etc.).
//! The pinning is bounded by the arena's lifetime — every
//! `sigil_arena_reset` clears `len` to 0, so the next mark phase sees
//! only the bytes the active iteration just wrote. Tracked as a v1
//! tradeoff in `PLAN_B_DEVIATIONS.md`.
//!
//! # Counters
//!
//! - `SIGIL_COUNTER_ARENA_ALLOC_COUNT` increments per `sigil_arena_alloc`.
//! - `SIGIL_COUNTER_ARENA_ALLOC_BYTES` accumulates the post-alignment
//!   byte count.
//! - `SIGIL_COUNTER_ARENA_ESCAPE_COUNT` increments per
//!   `sigil_arena_promote` call (the runtime-side helper that copies an
//!   in-arena value out to the Boehm heap when codegen detects it must
//!   outlive the next reset). Task 55's CPS lowering wires the call
//!   sites; Task 56 ships the helper.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use crate::counters::{self, CounterId};

/// Initial reserved capacity for the per-thread arena, in u64 words. 8K
/// words = 64 KiB. Sized to comfortably hold the in-flight `NextStep`
/// records of a deep CPS trampoline iteration without reallocating.
/// Bump if a workload trips `arena overflow` aborts.
const INITIAL_CAPACITY_WORDS: usize = 8 * 1024;

/// Sigil heap-object alignment. Matches the 8-byte object-header
/// alignment so arena-allocated `NextStep` records hold word-aligned
/// u64 fields. The Vec<u64> backing storage is naturally aligned to
/// this; arithmetic on byte offsets stays aligned because every
/// allocation rounds the byte size up to a multiple of `ALIGN`.
const ALIGN: usize = 8;

thread_local! {
    static ARENA: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// Per-thread flag: has this thread's arena storage been registered
    /// as a Boehm GC root? Set by `register_arena_root_for_calling_thread`,
    /// idempotent per thread.
    static ARENA_ROOTED: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn round_up_to_align(n: usize) -> usize {
    // Use checked arithmetic. The naive `(n + ALIGN - 1) & !(ALIGN - 1)`
    // wraps for `n > usize::MAX - 7`, giving a small `aligned` value
    // that bypasses the capacity check. Unreachable for current callers
    // (NextStep + args sizes never approach u64), but the function is a
    // public extern entry-point and any future caller is exposed.
    match n.checked_add(ALIGN - 1) {
        Some(v) => v & !(ALIGN - 1),
        None => {
            eprintln!("round_up_to_align: size {n} overflows usize");
            std::process::abort();
        }
    }
}

/// Reserve the arena's backing capacity and (if Boehm is initialised
/// on this thread) register the storage range as a GC root. Idempotent
/// per thread; safe to call from `sigil_gc_init`.
///
/// Called from `sigil_gc_init` for every thread that will use the
/// trampoline. v1 is single-threaded (only `main` calls into the
/// trampoline in production); test threads that allocate via the
/// arena and store Boehm pointers must call `sigil_gc_init` to opt
/// into rooting.
pub(crate) fn register_arena_root_for_calling_thread() -> (*mut c_void, *mut c_void) {
    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        ensure_capacity_or_abort(&mut arena);
        // SAFETY: gc-heap-ptr arithmetic (passed to GC_add_roots as range start; not retained).
        let start = arena.as_mut_ptr() as *mut c_void;
        let end_bytes = arena.capacity() * 8;
        // SAFETY: gc-heap-ptr arithmetic (the result feeds an FFI
        // call that takes [start, end) as a half-open range, never
        // retained; the storage lives for the thread's lifetime).
        let end = unsafe { (start as *mut u8).add(end_bytes) as *mut c_void };
        let already_registered = ARENA_ROOTED.with(|rooted| {
            let r = rooted.get();
            rooted.set(true);
            r
        });
        if !already_registered {
            unsafe {
                crate::gc::GC_add_roots(start, end);
            }
        }
        (start, end)
    })
}

/// Inverse of `register_arena_root_for_calling_thread`. Used by
/// `GcThreadEnrolment::drop` in tests to unregister the arena's
/// storage range before the thread exits.
#[cfg(test)]
pub(crate) fn unregister_arena_root_for_calling_thread(start: *mut c_void, end: *mut c_void) {
    ARENA_ROOTED.with(|rooted| rooted.set(false));
    unsafe {
        crate::gc::GC_remove_roots(start, end);
    }
}

#[inline]
fn ensure_capacity_or_abort(arena: &mut Vec<u64>) {
    if arena.capacity() == 0 {
        // `try_reserve_exact` returns an error rather than panicking
        // on OOM. We must NOT panic across the surrounding extern "C"
        // boundary (UB under `panic = "unwind"`); abort instead.
        if let Err(e) = arena.try_reserve_exact(INITIAL_CAPACITY_WORDS) {
            eprintln!(
                "sigil_arena_alloc: try_reserve_exact({} words) failed: {e}",
                INITIAL_CAPACITY_WORDS
            );
            std::process::abort();
        }
        // Zero-fill the freshly-reserved capacity. The arena's storage
        // range is registered with `GC_add_roots`; Boehm's conservative
        // scan visits the entire `[start, start + capacity*8)` range
        // every mark phase, including the no-longer-live portion past
        // `len`. Without zeroing, uninitialised bytes from
        // `try_reserve_exact` (or stale bytes from prior iterations)
        // can alias pointers to freed Boehm blocks, which Boehm then
        // dereferences and segfaults on. The cost is one memset over
        // `INITIAL_CAPACITY_WORDS * 8` bytes (default 64 KiB) per
        // thread per program lifetime — negligible.
        arena.resize(INITIAL_CAPACITY_WORDS, 0);
        // Length back to zero; capacity preserved with zeroed bytes.
        // SAFETY: gc-heap-ptr arithmetic (set_len does not produce a
        // pointer; the bytes we just wrote remain valid in capacity).
        unsafe { arena.set_len(0) };
    }
}

/// Allocate `size` bytes from the current dispatch arena, aligned to 8
/// bytes. Returns a pointer to the start of the allocation.
///
/// The pointer remains valid until the next `sigil_arena_reset`. Callers
/// must not retain it past that point; if a value needs to outlive the
/// reset (e.g. a continuation captured into a handler frame), promote it
/// via `sigil_arena_promote` first.
///
/// # Aborts
///
/// Aborts if the requested allocation would exceed the arena's reserved
/// capacity, or if the initial reserve fails. v1's workloads fit under
/// `INITIAL_CAPACITY_WORDS`; trip the abort only when the budget
/// legitimately needs raising, not by reallocating (which would
/// invalidate live pointers from earlier calls in the same iteration).
///
/// # Safety
///
/// Safe to call from any thread (each thread owns its own arena). The
/// returned pointer is valid for `round_up_to_align(size)` bytes. Must
/// not be called reentrantly within the same thread (the underlying
/// `RefCell` panics on overlapping `borrow_mut`); the trampoline
/// upholds this by resetting before invoking the next dispatch step.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_alloc(size: usize) -> *mut u8 {
    let aligned = round_up_to_align(size);
    let words_needed = aligned / 8;

    counters::incr(CounterId::ArenaAllocCount);
    counters::add(CounterId::ArenaAllocBytes, aligned as u64);

    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        ensure_capacity_or_abort(&mut arena);
        let start_words = arena.len();
        let new_len_words = start_words.saturating_add(words_needed);
        if new_len_words > arena.capacity() {
            eprintln!(
                "sigil_arena_alloc: arena overflow — requested {aligned} bytes, \
                 already used {} of {} (raise INITIAL_CAPACITY_WORDS)",
                start_words * 8,
                arena.capacity() * 8
            );
            std::process::abort();
        }
        // The arena is a separate GC-rooted region; pointers returned
        // here are not into a Sigil heap object. The set_len +
        // as_mut_ptr().add pattern writes into reserved capacity
        // without reallocating — pointers handed out by earlier calls
        // in this iteration stay valid.
        arena.set_len(new_len_words);
        // SAFETY: gc-heap-ptr arithmetic (arena is GC-rooted region, see comment above).
        arena.as_mut_ptr().add(start_words) as *mut u8
    })
}

/// Reset the current dispatch arena. Invalidates every pointer previously
/// returned by `sigil_arena_alloc` since the last reset. The trampoline
/// (`sigil_run_loop`) calls this at the top of each iteration.
///
/// Capacity is preserved — only the length is cleared. The bytes that
/// held the just-cleared region are zeroed so Boehm's conservative scan
/// of the registered arena range (`GC_add_roots` covers
/// `[start, start + capacity*8)`) does not see stale pointers in the
/// no-longer-live portion. The cost is a memset over `len * 8` bytes,
/// which on the trampoline's hot path is typically tens-of-bytes per
/// iteration.
///
/// # Safety
///
/// Marked `unsafe` because invalidating pointers from prior
/// `sigil_arena_alloc` calls is a real precondition the caller must
/// uphold. The trampoline guarantees this by reading the previous
/// iteration's `NextStep` into stack locals before reset.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_reset() {
    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        let used_words = arena.len();
        if used_words > 0 {
            // We write zeros in-place over reserved capacity; no
            // pointer is produced or retained. The arena's backing
            // storage outlives this call; the write_bytes call only
            // touches bytes about to be reused.
            // SAFETY: gc-heap-ptr arithmetic (arena reset zero-fill, see comment above).
            arena.as_mut_ptr().write_bytes(0, used_words);
        }
        // SAFETY: gc-heap-ptr arithmetic (set_len(0) does not produce
        // a pointer; this is bookkeeping only). Capacity preserved.
        unsafe { arena.set_len(0) };
    });
}

/// Promote an in-arena value to the Boehm heap. Allocates a fresh
/// header-prefixed object, copies `payload_bytes` from `src` into the
/// payload region (offset 8 from the returned header pointer), and
/// returns the new heap-object pointer.
///
/// Used by Task 55's CPS codegen when a value (typically a captured
/// continuation closure) must outlive the next `sigil_arena_reset`.
/// Increments `SIGIL_COUNTER_ARENA_ESCAPE_COUNT`.
///
/// # Safety
///
/// `src` must point to at least `payload_bytes` readable bytes (or be
/// null when `payload_bytes == 0`). `header` is the precomputed 8-byte
/// header word for the new heap object.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_promote(
    src: *const u8,
    header: u64,
    payload_bytes: usize,
    descriptor_index: u32,
) -> *mut u8 {
    counters::incr(CounterId::ArenaEscapeCount);

    let obj = crate::gc::sigil_alloc(header, payload_bytes, descriptor_index);
    if payload_bytes > 0 && !src.is_null() {
        // SAFETY: gc-heap-ptr arithmetic (the destination pointer is
        // computed from the just-allocated `obj` purely to drive a
        // single byte-range copy; it is neither stored nor returned to
        // the caller).
        let dst = obj.add(8);
        std::ptr::copy_nonoverlapping(src, dst, payload_bytes);
    }
    obj
}

/// Test-only hook to verify the arena overflow abort triggers under a
/// shrunken capacity. Allocates against a thread-local override
/// capacity instead of `INITIAL_CAPACITY_WORDS`.
#[cfg(test)]
pub(crate) fn force_capacity_for_test(words: usize) {
    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        // SAFETY: gc-heap-ptr arithmetic (set_len(0) only updates the
        // bookkeeping; no pointer is produced).
        unsafe { arena.set_len(0) };
        // Drop-and-rebuild: `Vec` does not expose an API to shrink
        // capacity below the current allocation, so we replace the
        // backing entirely.
        *arena = Vec::with_capacity(words);
    });
    ARENA_ROOTED.with(|rooted| rooted.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena_used_words() -> usize {
        ARENA.with(|cell| cell.borrow().len())
    }

    fn arena_used_bytes() -> usize {
        arena_used_words() * 8
    }

    fn reset_state() {
        // SAFETY: tests that hold the gc_test_lock or otherwise serialise
        // alloc activity satisfy the no-live-pointer precondition.
        unsafe {
            sigil_arena_reset();
        }
    }

    #[test]
    fn alloc_round_trips_and_aligns_to_eight() {
        reset_state();
        let p1 = unsafe { sigil_arena_alloc(1) };
        let p2 = unsafe { sigil_arena_alloc(7) };
        let p3 = unsafe { sigil_arena_alloc(8) };
        assert!(!p1.is_null() && !p2.is_null() && !p3.is_null());
        let d12 = p2 as usize - p1 as usize;
        let d23 = p3 as usize - p2 as usize;
        assert_eq!(d12, 8);
        assert_eq!(d23, 8);
        assert_eq!(arena_used_bytes(), 24);
        // Absolute alignment: every returned pointer is 8-aligned.
        assert_eq!(p1 as usize % 8, 0);
        assert_eq!(p2 as usize % 8, 0);
        assert_eq!(p3 as usize % 8, 0);
        reset_state();
    }

    #[test]
    fn reset_clears_length_preserves_capacity() {
        reset_state();
        let _ = unsafe { sigil_arena_alloc(128) };
        let cap_before = ARENA.with(|c| c.borrow().capacity());
        assert!(arena_used_bytes() > 0);
        reset_state();
        assert_eq!(arena_used_bytes(), 0);
        let cap_after = ARENA.with(|c| c.borrow().capacity());
        assert_eq!(cap_before, cap_after);
    }

    #[test]
    fn alloc_writes_are_observable_through_returned_pointer() {
        reset_state();
        let p = unsafe { sigil_arena_alloc(16) };
        unsafe {
            let words: *mut u64 = p.cast();
            words.write(0xDEADBEEF);
            words.add(1).write(0xFEEDFACE);
            assert_eq!(words.read(), 0xDEADBEEF);
            assert_eq!(words.add(1).read(), 0xFEEDFACE);
        }
        reset_state();
    }

    #[test]
    fn alloc_does_not_invalidate_earlier_pointers_within_capacity() {
        reset_state();
        let p_first = unsafe { sigil_arena_alloc(8) };
        unsafe {
            (p_first as *mut u64).write(0xABCD);
        }
        for i in 0..512 {
            let _ = unsafe { sigil_arena_alloc(64) };
            let observed = unsafe { (p_first as *const u64).read() };
            assert_eq!(observed, 0xABCD, "pointer invalidated after {i} allocs");
        }
        reset_state();
    }

    #[test]
    fn promote_copies_bytes_and_returns_heap_object() {
        let _guard = crate::test_support::gc_test_lock();
        crate::gc::sigil_gc_init();
        reset_state();
        let arena_buf = unsafe { sigil_arena_alloc(16) };
        unsafe {
            (arena_buf as *mut u64).write(0x1111_2222);
            (arena_buf as *mut u64).add(1).write(0x3333_4444);
        }
        let header = crate::header::Header::new(crate::header::TAG_INT64, 2, 0);
        let escape_before = counters::read(CounterId::ArenaEscapeCount);
        // bitmap=0 → atomic path inside sigil_alloc; descriptor_index
        // ignored.
        let promoted = unsafe { sigil_arena_promote(arena_buf, header.raw(), 16, u32::MAX) };
        assert!(!promoted.is_null());
        unsafe {
            let payload: *const u64 = promoted.add(8).cast();
            assert_eq!(payload.read(), 0x1111_2222);
            assert_eq!(payload.add(1).read(), 0x3333_4444);
        }
        let escape_after = counters::read(CounterId::ArenaEscapeCount);
        assert_eq!(escape_after - escape_before, 1);
        reset_state();
    }

    #[test]
    fn alloc_increments_count_and_bytes_counters() {
        reset_state();
        let count_before = counters::read(CounterId::ArenaAllocCount);
        let bytes_before = counters::read(CounterId::ArenaAllocBytes);
        let _ = unsafe { sigil_arena_alloc(13) };
        let _ = unsafe { sigil_arena_alloc(3) };
        let count_after = counters::read(CounterId::ArenaAllocCount);
        let bytes_after = counters::read(CounterId::ArenaAllocBytes);
        // Sibling tests in parallel can also bump the counters; assert
        // minimum delta. Concurrent activity inflates, never deflates.
        assert!(count_after - count_before >= 2);
        assert!(bytes_after - bytes_before >= 24);
        reset_state();
    }

    #[test]
    fn round_up_to_align_at_zero() {
        assert_eq!(round_up_to_align(0), 0);
        assert_eq!(round_up_to_align(1), 8);
        assert_eq!(round_up_to_align(7), 8);
        assert_eq!(round_up_to_align(8), 8);
        assert_eq!(round_up_to_align(9), 16);
    }

    #[test]
    fn arena_overflow_aborts() -> Result<(), Box<dyn std::error::Error>> {
        // Plan State-Cell — was previously `#[ignore]`'d ("abort tests
        // are not directly observable from cargo test"). Self-execed
        // subprocess approach: re-run this test binary with
        // `SIGIL_TEST_ARENA_OVERFLOW_CHILD=1`; the child takes the
        // child branch, calls the abort path, dies via `process::abort`,
        // and the parent's `Command::output` reports a non-zero exit
        // status that the assert below pins. The fn returns
        // `Result<(), Box<dyn Error>>` so fallible setup steps can
        // route through `?` instead of `expect()` (the workspace
        // clippy config disallows `expect`/`unwrap` to keep error
        // paths explicit). Same pattern absorbs future abort-path
        // tests without re-introducing `#[ignore]`.
        if std::env::var_os("SIGIL_TEST_ARENA_OVERFLOW_CHILD").is_some() {
            // Child mode: trigger the abort path. The child subprocess
            // will die via `process::abort` here; control never
            // returns to the test harness.
            force_capacity_for_test(1); // 8 bytes capacity
            let _ = unsafe { sigil_arena_alloc(8) }; // fills it
            let _ = unsafe { sigil_arena_alloc(8) }; // aborts
            unreachable!("arena overflow path should have aborted");
        }
        let exe = std::env::current_exe()?;
        let output = std::process::Command::new(&exe)
            .args([
                "arena::tests::arena_overflow_aborts",
                "--exact",
                "--nocapture",
            ])
            .env("SIGIL_TEST_ARENA_OVERFLOW_CHILD", "1")
            .output()?;
        assert!(
            !output.status.success(),
            "child subprocess succeeded; expected SIGABRT from arena overflow. \
             stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("arena overflow"),
            "expected `arena overflow` abort message; got stderr: {stderr}"
        );
        Ok(())
    }
}
