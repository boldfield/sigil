//! Mutable `MutArray[A]` runtime primitives — Plan C Task 66.
//!
//! Layout on the heap is identical to `array.rs`'s immutable Array
//! (header + length word + N element slots), differing only in:
//!
//! 1. Header tag is `TAG_MUT_ARRAY (0x05)` instead of `TAG_ARRAY
//!    (0x04)` so a v2 GC walker can apply per-tag write-barrier
//!    semantics.
//! 2. The set primitive (`sigil_mut_array_set`) mutates the slot
//!    in place and returns Unit — no fresh allocation.
//!
//! ## The Mem effect (Plan C Task 66)
//!
//! `MutArray` operations live behind a `Mem` marker effect: each
//! builtin scheme declares `effects: vec!["Mem"]`. Code that
//! mutates must declare `![Mem]` in its effect row; the compiler
//! rejects mutation calls otherwise. `main()` declares `![Mem]`
//! to permit mutation; the v1 "top-level Mem handler" is the
//! type-level absence of a deeper override (Mem has zero ops, so
//! there's no per-op handler arm to install in the main shim).
//! See `[DEVIATION Task 66]` in `PLAN_C_DEVIATIONS.md` for the v2
//! closure path.
//!
//! ## Out-of-bounds access
//!
//! `sigil_mut_array_get` and `sigil_mut_array_set` abort on out-
//! of-bounds indices, mirroring the immutable Array behaviour.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_MUT_ARRAY};

/// Allocate a fresh mutable array of `len` elements, each initialised
/// to `fill`. Returns a header pointer.
///
/// # Safety
///
/// Safe to call from any thread (Boehm-managed allocation).
#[no_mangle]
pub extern "C" fn sigil_mut_array_new(len: u64, fill: u64) -> *mut u8 {
    let payload_bytes = 8usize.saturating_add((len as usize).saturating_mul(8));
    let h = Header::new(TAG_MUT_ARRAY, 0, 1);
    let obj = sigil_alloc(h.raw(), payload_bytes);

    // Length word at offset 8.
    //
    // SAFETY: not an interior pointer (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(len);
    }

    if len > 0 {
        unsafe {
            // SAFETY: not an interior pointer (offset 16 is a transient u64 store base; indices stay within the allocated payload).
            let elems_ptr: *mut u64 = obj.add(16).cast();
            for i in 0..(len as usize) {
                // SAFETY: not an interior pointer (transient elems_ptr.add(i) for one aligned u64 write).
                elems_ptr.add(i).write(fill);
            }
        }
    }

    counters::incr(CounterId::MutArrayAllocCount);
    counters::add(CounterId::MutArrayAllocBytes, (8 + payload_bytes) as u64);

    obj
}

/// Read a mutable array's length.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_MUT_ARRAY` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_array_length(arr: *const u8) -> u64 {
    // SAFETY: not an interior pointer (transient base for one u64 read).
    let len_ptr: *const u64 = arr.add(8).cast();
    len_ptr.read()
}

/// Read element `i`. Aborts on out-of-bounds.
///
/// # Safety
///
/// Same as `sigil_mut_array_length`.
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_array_get(arr: *const u8, i: u64) -> u64 {
    let len = sigil_mut_array_length(arr);
    if i >= len {
        eprintln!("sigil_mut_array_get: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: not an interior pointer (offset 16 base + bounds-checked index).
    let elems_ptr: *const u64 = arr.add(16).cast();
    // SAFETY: not an interior pointer (transient add(i) for one aligned u64 read).
    elems_ptr.add(i as usize).read()
}

/// Mutate element `i` in place. Aborts on out-of-bounds. Returns
/// Unit (zero-valued u64; sigil's Unit lowers to I8 zero at the
/// surface but the FFI declares u64 for ABI uniformity).
///
/// # Safety
///
/// Same as `sigil_mut_array_length`. Caller must ensure no concurrent
/// reads of the same slot from another thread (v1 has no thread
/// synchronisation primitives; mutations are single-threaded by
/// construction).
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_array_set(arr: *mut u8, i: u64, val: u64) {
    let len = sigil_mut_array_length(arr);
    if i >= len {
        eprintln!("sigil_mut_array_set: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: not an interior pointer (transient base + bounds-checked index for one aligned u64 write).
    let elems_ptr: *mut u64 = arr.add(16).cast();
    // SAFETY: not an interior pointer (transient add(i) for one aligned u64 write).
    elems_ptr.add(i as usize).write(val);
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    #[test]
    fn alloc_zero_length_returns_valid_array() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(0, 0);
        unsafe {
            assert_eq!(sigil_mut_array_length(arr), 0);
        }
    }

    #[test]
    fn alloc_with_fill_initialises_all_slots() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(5, 99);
        unsafe {
            assert_eq!(sigil_mut_array_length(arr), 5);
            for i in 0..5 {
                assert_eq!(sigil_mut_array_get(arr, i), 99, "slot {i}");
            }
        }
    }

    #[test]
    fn set_mutates_in_place() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(3, 0);
        unsafe {
            sigil_mut_array_set(arr, 1, 42);
            // Same pointer; same slot now reads 42.
            assert_eq!(sigil_mut_array_get(arr, 0), 0);
            assert_eq!(sigil_mut_array_get(arr, 1), 42);
            assert_eq!(sigil_mut_array_get(arr, 2), 0);
        }
    }

    #[test]
    fn set_chain_accumulates_in_one_array() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(4, 0);
        unsafe {
            sigil_mut_array_set(arr, 0, 10);
            sigil_mut_array_set(arr, 1, 20);
            sigil_mut_array_set(arr, 2, 30);
            sigil_mut_array_set(arr, 3, 40);
            assert_eq!(sigil_mut_array_get(arr, 0), 10);
            assert_eq!(sigil_mut_array_get(arr, 1), 20);
            assert_eq!(sigil_mut_array_get(arr, 2), 30);
            assert_eq!(sigil_mut_array_get(arr, 3), 40);
        }
    }

    #[test]
    fn alloc_at_sudoku_size_works() {
        // 81-element MutArray (Sudoku-board-sized). Same count-field
        // overflow consideration as TAG_ARRAY — count=0, length in
        // payload word 0.
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(81, 0);
        unsafe {
            assert_eq!(sigil_mut_array_length(arr), 81);
            sigil_mut_array_set(arr, 80, 7);
            assert_eq!(sigil_mut_array_get(arr, 80), 7);
        }
    }

    #[test]
    fn header_tag_is_mut_array() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_array_new(0, 0);
        unsafe {
            let header_ptr: *const u64 = arr.cast();
            let header = Header(header_ptr.read());
            assert_eq!(header.type_tag(), TAG_MUT_ARRAY);
            assert_ne!(header.pointer_bitmap(), 0);
        }
    }
}
