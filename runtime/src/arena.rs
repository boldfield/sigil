//! Trampoline arena allocator — Plan B Task 56.
//!
//! Backs the per-dispatch bump arena that holds `NextStep` records in the
//! CPS trampoline (`sigil_run_loop`). Reset at the top of every loop
//! iteration; only values that explicitly escape a step are copied to the
//! Boehm heap (Task 55 wires the promotion sites).
//!
//! # Layout
//!
//! A single growable `Vec<u8>` per thread, treated as a bump region. v1
//! is single-threaded but the `thread_local!` keeps the API
//! forward-compatible with a multi-threaded v2 trampoline that would
//! shard the arena per thread anyway. All allocations are 8-byte aligned
//! (the Sigil heap-object alignment); callers pass the requested payload
//! size in bytes and receive an aligned pointer.
//!
//! # Capacity discipline
//!
//! The arena pre-reserves `INITIAL_CAPACITY` bytes on first use. Once
//! reserved, allocations are non-reallocating: the underlying `Vec`
//! grows its `len` within the existing capacity using `set_len`. This
//! guarantees pointers returned by earlier `sigil_arena_alloc` calls in
//! the same iteration remain valid through subsequent allocations,
//! which the trampoline relies on (a `sigil_perform` site may produce a
//! `NextStep` while the surrounding caller still holds a pointer into
//! the arena).
//!
//! Exceeding the reserved capacity within a single iteration aborts
//! with a clear message rather than reallocating. v1's CPS-color
//! workloads (fib(20) under a forced effect row, multi-shot Choose
//! demos) fit comfortably under the default budget; if a future
//! workload outgrows it, raise `INITIAL_CAPACITY` rather than relax the
//! non-reallocating invariant.
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

use std::cell::RefCell;

use crate::counters::{self, CounterId};

/// Initial reserved capacity for the per-thread arena. Sized to comfortably
/// hold the in-flight `NextStep` records of a deep CPS trampoline iteration
/// without reallocating. Bump if a workload trips `arena overflow` aborts.
const INITIAL_CAPACITY: usize = 64 * 1024;

/// Sigil heap-object alignment. Matches the 8-byte object-header alignment
/// so arena-allocated `NextStep` records hold word-aligned u64 fields.
const ALIGN: usize = 8;

thread_local! {
    static ARENA: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[inline]
fn round_up_to_align(n: usize) -> usize {
    (n + ALIGN - 1) & !(ALIGN - 1)
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
/// capacity. v1's workloads fit under `INITIAL_CAPACITY`; trip the abort
/// only when the budget legitimately needs raising, not by reallocating
/// (which would invalidate live pointers from earlier calls in the same
/// iteration).
///
/// # Safety
///
/// Safe to call from any thread (each thread owns its own arena). The
/// returned pointer is valid for `round_up_to_align(size)` bytes.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_alloc(size: usize) -> *mut u8 {
    let aligned = round_up_to_align(size);

    counters::incr(CounterId::ArenaAllocCount);
    counters::add(CounterId::ArenaAllocBytes, aligned as u64);

    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        if arena.capacity() == 0 {
            arena.reserve(INITIAL_CAPACITY);
        }
        let start = arena.len();
        let new_len = start.saturating_add(aligned);
        if new_len > arena.capacity() {
            eprintln!(
                "sigil_arena_alloc: arena overflow — requested {aligned} bytes, \
                 already used {start} of {} (raise INITIAL_CAPACITY)",
                arena.capacity()
            );
            std::process::abort();
        }
        // The arena is an opaque byte buffer that Boehm never scans;
        // arena pointers are not GC references and the
        // no-interior-pointers rule applies only to GC-managed heap
        // objects. The set_len + as_mut_ptr().add(start) pattern
        // writes into reserved capacity without reallocating —
        // pointers handed out by earlier calls in this iteration stay
        // valid.
        arena.set_len(new_len);
        // SAFETY: not an interior pointer (arena is opaque, see comment above).
        arena.as_mut_ptr().add(start)
    })
}

/// Reset the current dispatch arena. Invalidates every pointer previously
/// returned by `sigil_arena_alloc` since the last reset. The trampoline
/// (`sigil_run_loop`) calls this at the top of each iteration.
///
/// Capacity is preserved — only the length is cleared.
///
/// # Safety
///
/// Safe to call from any thread. Caller must not hold any live pointers
/// into the arena (this is what the trampoline guarantees by reading the
/// previous iteration's `NextStep` into stack locals before reset).
#[no_mangle]
pub extern "C" fn sigil_arena_reset() {
    ARENA.with(|cell| {
        let mut arena = cell.borrow_mut();
        // SAFETY: not an interior pointer (set_len(0) does not produce a
        // pointer; this is bookkeeping only). Capacity is preserved.
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
) -> *mut u8 {
    counters::incr(CounterId::ArenaEscapeCount);

    let obj = crate::gc::sigil_alloc(header, payload_bytes);
    if payload_bytes > 0 && !src.is_null() {
        // SAFETY: not an interior pointer (the destination pointer is
        // computed from the just-allocated `obj` purely to drive a
        // single byte-range copy; it is neither stored nor returned to
        // the caller).
        let dst = obj.add(8);
        std::ptr::copy_nonoverlapping(src, dst, payload_bytes);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena_used() -> usize {
        ARENA.with(|cell| cell.borrow().len())
    }

    fn reset_and_zero_counters() {
        sigil_arena_reset();
        // Counters are global atomic; we cannot zero them without an
        // unsafe back door. Tests instead read deltas (capture before
        // and after).
    }

    #[test]
    fn alloc_round_trips_and_aligns_to_eight() {
        reset_and_zero_counters();
        let p1 = unsafe { sigil_arena_alloc(1) };
        let p2 = unsafe { sigil_arena_alloc(7) };
        let p3 = unsafe { sigil_arena_alloc(8) };
        assert!(!p1.is_null() && !p2.is_null() && !p3.is_null());
        // Each alloc rounded up to 8: addresses 8 bytes apart.
        let d12 = p2 as usize - p1 as usize;
        let d23 = p3 as usize - p2 as usize;
        assert_eq!(d12, 8);
        assert_eq!(d23, 8);
        // Total used = 24.
        assert_eq!(arena_used(), 24);
        sigil_arena_reset();
    }

    #[test]
    fn reset_clears_length_preserves_capacity() {
        reset_and_zero_counters();
        let _ = unsafe { sigil_arena_alloc(128) };
        let cap_before = ARENA.with(|c| c.borrow().capacity());
        assert!(arena_used() > 0);
        sigil_arena_reset();
        assert_eq!(arena_used(), 0);
        let cap_after = ARENA.with(|c| c.borrow().capacity());
        assert_eq!(cap_before, cap_after);
    }

    #[test]
    fn alloc_writes_are_observable_through_returned_pointer() {
        reset_and_zero_counters();
        let p = unsafe { sigil_arena_alloc(16) };
        unsafe {
            // Write two u64s and read them back.
            let words: *mut u64 = p.cast();
            words.write(0xDEADBEEF);
            words.add(1).write(0xFEEDFACE);
            assert_eq!(words.read(), 0xDEADBEEF);
            assert_eq!(words.add(1).read(), 0xFEEDFACE);
        }
        sigil_arena_reset();
    }

    #[test]
    fn alloc_does_not_invalidate_earlier_pointers_within_capacity() {
        // The non-reallocating invariant: pointers from earlier allocs
        // must remain valid as we keep allocating, as long as we stay
        // within the reserved capacity. This is the property the
        // trampoline relies on.
        reset_and_zero_counters();
        let p_first = unsafe { sigil_arena_alloc(8) };
        unsafe {
            (p_first as *mut u64).write(0xABCD);
        }
        // Allocate enough small blocks to exercise the path many times
        // without ever spilling capacity.
        for i in 0..512 {
            let _ = unsafe { sigil_arena_alloc(64) };
            // Re-read the first pointer after each subsequent alloc.
            let observed = unsafe { (p_first as *const u64).read() };
            assert_eq!(observed, 0xABCD, "pointer invalidated after {i} allocs");
        }
        sigil_arena_reset();
    }

    #[test]
    fn promote_copies_bytes_and_returns_heap_object() {
        // Boehm needs init before sigil_alloc is callable; serialise
        // through the shared GC-test mutex so parallel tests don't race
        // the mark phase against unregistered Rust threads.
        let _guard = crate::test_support::gc_test_lock();
        crate::gc::sigil_gc_init();
        reset_and_zero_counters();
        let arena_buf = unsafe { sigil_arena_alloc(16) };
        unsafe {
            (arena_buf as *mut u64).write(0x1111_2222);
            (arena_buf as *mut u64).add(1).write(0x3333_4444);
        }
        // Allocate a heap object with the same payload bytes. Tag is
        // arbitrary for this byte-copy test; pick TAG_INT64 with
        // payload-word count = 2 and a zero pointer bitmap (so
        // sigil_alloc routes through GC_malloc_atomic, no GC scan
        // required for the test bytes).
        let header = crate::header::Header::new(
            crate::header::TAG_INT64,
            /* payload_words */ 2,
            /* pointer_bitmap */ 0,
        );
        let escape_before = counters::read(CounterId::ArenaEscapeCount);
        let promoted = unsafe { sigil_arena_promote(arena_buf, header.raw(), 16) };
        assert!(!promoted.is_null());
        unsafe {
            let payload: *const u64 = promoted.add(8).cast();
            assert_eq!(payload.read(), 0x1111_2222);
            assert_eq!(payload.add(1).read(), 0x3333_4444);
        }
        let escape_after = counters::read(CounterId::ArenaEscapeCount);
        assert_eq!(escape_after - escape_before, 1);
        sigil_arena_reset();
    }

    #[test]
    fn alloc_increments_count_and_bytes_counters() {
        // The counters are global atomics; sibling arena/handlers tests
        // running in parallel can also bump them. We assert
        // minimum-delta — concurrent activity can only inflate the
        // observed count, never deflate it, so `>= 2` and `>= 24` are
        // both true regardless of interleaving.
        reset_and_zero_counters();
        let count_before = counters::read(CounterId::ArenaAllocCount);
        let bytes_before = counters::read(CounterId::ArenaAllocBytes);
        let _ = unsafe { sigil_arena_alloc(13) };
        let _ = unsafe { sigil_arena_alloc(3) };
        let count_after = counters::read(CounterId::ArenaAllocCount);
        let bytes_after = counters::read(CounterId::ArenaAllocBytes);
        assert!(
            count_after - count_before >= 2,
            "expected ≥2 alloc-count increments, got {}",
            count_after - count_before
        );
        // Our two allocs round to 16 + 8 = 24 bytes; concurrent allocs
        // may add more.
        assert!(
            bytes_after - bytes_before >= 24,
            "expected ≥24 alloc-byte increments, got {}",
            bytes_after - bytes_before
        );
        sigil_arena_reset();
    }
}
