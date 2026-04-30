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
//!
//! ## Interior-pointer arithmetic
//!
//! As with `array.rs`, slot reads and writes use `obj.add(N)` —
//! literal interior pointers into the GC-allocated object. Boehm's
//! conservative scan tolerates interior pointers (it walks back to
//! the object's base), and every site below uses the pointer
//! transiently for a single aligned read or write before discarding
//! it. The `gc-heap-ptr arithmetic` SAFETY markers acknowledge each
//! site.
//!
//! ## GC reachability under mutation
//!
//! v1's collector is Boehm conservative — slot writes do not need a
//! write barrier (Boehm's mark phase scans the whole heap and finds
//! pointers wherever they live, so a freshly stored slot is reachable
//! by the next collection without explicit help) and do not need a
//! safepoint (stackmaps and safepoints serve precise / moving
//! collectors; Boehm scans conservatively at allocation points). v2
//! migration to a precise / moving GC will need both. See
//! `[DEVIATION Task 66] mutation under v2 GC` (a future entry) for
//! the closure path.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_MUT_ARRAY, TAG_MUT_BYTE_ARRAY};

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
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(len);
    }

    if len > 0 {
        unsafe {
            // SAFETY: gc-heap-ptr arithmetic (offset 16 is a transient u64 store base; indices stay within the allocated payload).
            let elems_ptr: *mut u64 = obj.add(16).cast();
            for i in 0..(len as usize) {
                // SAFETY: gc-heap-ptr arithmetic (transient elems_ptr.add(i) for one aligned u64 write).
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
    // SAFETY: gc-heap-ptr arithmetic (transient base for one u64 read).
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
    // SAFETY: gc-heap-ptr arithmetic (offset 16 base + bounds-checked index).
    let elems_ptr: *const u64 = arr.add(16).cast();
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one aligned u64 read).
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
    // SAFETY: gc-heap-ptr arithmetic (transient base + bounds-checked index for one aligned u64 write).
    let elems_ptr: *mut u64 = arr.add(16).cast();
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one aligned u64 write).
    elems_ptr.add(i as usize).write(val);
}

// ===== Plan C Task 66.6 — `MutByteArray` primitives =====
//
// Mirrors `runtime/src/byte_array.rs`'s immutable surface but with
// in-place mutation. Heap layout is identical: `{header, length:u64,
// byte[0..N]}`. Bitmap is 0 (atomic GC scan); bytes are pure scalars.
// The byte payload is rounded up to a word boundary at allocation
// time so the object's payload-byte size is a multiple of 8.

#[inline]
fn round_up_to_word(n: usize) -> usize {
    (n + 7) & !7
}

#[inline]
fn mut_byte_payload_bytes_for(len: u64) -> usize {
    8usize.saturating_add(round_up_to_word(len as usize))
}

/// Allocate a fresh mutable byte-array of `len` bytes, each
/// initialised to `fill`. Returns the header pointer.
///
/// # Safety
///
/// Safe to call from any thread (Boehm-managed allocation).
#[no_mangle]
pub extern "C" fn sigil_mut_byte_array_new(len: u64, fill: u8) -> *mut u8 {
    let payload_bytes = mut_byte_payload_bytes_for(len);
    let h = Header::new(TAG_MUT_BYTE_ARRAY, 0, 0);
    let obj = sigil_alloc(h.raw(), payload_bytes);

    // Length word at offset 8.
    //
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(len);
    }

    // GC_malloc_atomic returns zeroed memory in Boehm; skip the
    // per-byte fill loop when `fill == 0`.
    if len > 0 && fill != 0 {
        // SAFETY: gc-heap-ptr arithmetic (transient base for a single contiguous byte fill; bounds are `[16, 16+len)`).
        unsafe {
            let bytes_ptr: *mut u8 = obj.add(16);
            std::ptr::write_bytes(bytes_ptr, fill, len as usize);
        }
    }

    counters::incr(CounterId::MutByteArrayAllocCount);
    counters::add(
        CounterId::MutByteArrayAllocBytes,
        (8 + payload_bytes) as u64,
    );

    obj
}

/// Read a mutable byte-array's length.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_MUT_BYTE_ARRAY` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_byte_array_length(arr: *const u8) -> u64 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one u64 read).
    let len_ptr: *const u64 = arr.add(8).cast();
    len_ptr.read()
}

/// Read byte `i`. Aborts on out-of-bounds.
///
/// # Safety
///
/// Same as `sigil_mut_byte_array_length`.
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_byte_array_get(arr: *const u8, i: u64) -> u8 {
    let len = sigil_mut_byte_array_length(arr);
    if i >= len {
        eprintln!("sigil_mut_byte_array_get: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: gc-heap-ptr arithmetic (transient base + bounds-checked index for one byte read).
    let bytes_ptr: *const u8 = arr.add(16);
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one byte read).
    bytes_ptr.add(i as usize).read()
}

/// Mutate byte `i` in place. Aborts on out-of-bounds. Returns Unit
/// (zero-valued; sigil's Unit lowers to I8 zero at the surface).
///
/// # Safety
///
/// Same as `sigil_mut_byte_array_length`. Caller must ensure no
/// concurrent reads of the same slot from another thread.
#[no_mangle]
pub unsafe extern "C" fn sigil_mut_byte_array_set(arr: *mut u8, i: u64, val: u8) {
    let len = sigil_mut_byte_array_length(arr);
    if i >= len {
        eprintln!("sigil_mut_byte_array_set: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: gc-heap-ptr arithmetic (transient base + bounds-checked index for one byte write).
    let bytes_ptr: *mut u8 = arr.add(16);
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one byte write).
    bytes_ptr.add(i as usize).write(val);
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
    fn alloc_at_count_field_boundary_works() {
        // Mirror of `array.rs`'s boundary test: len=33 (well below
        // the 6-bit count cap of 63) and len=64 (one past, where
        // count=0's sidestep first becomes load-bearing). Both
        // confirm the count-from-payload-length-word convention is
        // honoured for MutArray.
        let _guard = gc_test_lock();
        let arr_33 = sigil_mut_array_new(33, 5);
        unsafe {
            assert_eq!(sigil_mut_array_length(arr_33), 33);
            sigil_mut_array_set(arr_33, 32, 999);
            assert_eq!(sigil_mut_array_get(arr_33, 32), 999);
            assert_eq!(sigil_mut_array_get(arr_33, 0), 5);
        }
        let arr_64 = sigil_mut_array_new(64, 9);
        unsafe {
            assert_eq!(sigil_mut_array_length(arr_64), 64);
            sigil_mut_array_set(arr_64, 63, 1234);
            assert_eq!(sigil_mut_array_get(arr_64, 63), 1234);
            assert_eq!(sigil_mut_array_get(arr_64, 0), 9);
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

    // ===== Plan C Task 66.6 — MutByteArray primitives =====

    #[test]
    fn mut_byte_array_alloc_zero_length() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_byte_array_new(0, 0);
        unsafe {
            assert_eq!(sigil_mut_byte_array_length(arr), 0);
        }
    }

    #[test]
    fn mut_byte_array_alloc_with_fill_initialises_all_bytes() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_byte_array_new(7, 0xCC);
        unsafe {
            assert_eq!(sigil_mut_byte_array_length(arr), 7);
            for i in 0..7 {
                assert_eq!(sigil_mut_byte_array_get(arr, i), 0xCC, "byte {i}");
            }
        }
    }

    #[test]
    fn mut_byte_array_set_mutates_in_place() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_byte_array_new(3, 0);
        unsafe {
            sigil_mut_byte_array_set(arr, 1, 42);
            assert_eq!(sigil_mut_byte_array_get(arr, 0), 0);
            assert_eq!(sigil_mut_byte_array_get(arr, 1), 42);
            assert_eq!(sigil_mut_byte_array_get(arr, 2), 0);
        }
    }

    #[test]
    fn mut_byte_array_set_chain_accumulates() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_byte_array_new(4, 0);
        unsafe {
            sigil_mut_byte_array_set(arr, 0, 10);
            sigil_mut_byte_array_set(arr, 1, 20);
            sigil_mut_byte_array_set(arr, 2, 30);
            sigil_mut_byte_array_set(arr, 3, 40);
            assert_eq!(sigil_mut_byte_array_get(arr, 0), 10);
            assert_eq!(sigil_mut_byte_array_get(arr, 1), 20);
            assert_eq!(sigil_mut_byte_array_get(arr, 2), 30);
            assert_eq!(sigil_mut_byte_array_get(arr, 3), 40);
        }
    }

    #[test]
    fn mut_byte_array_alloc_at_count_field_boundary() {
        // len=33 (mid-range) and len=64 (one past the 6-bit count
        // cap). Pin the count-from-payload-length convention.
        let _guard = gc_test_lock();
        let arr_33 = sigil_mut_byte_array_new(33, 7);
        unsafe {
            assert_eq!(sigil_mut_byte_array_length(arr_33), 33);
            sigil_mut_byte_array_set(arr_33, 32, 99);
            assert_eq!(sigil_mut_byte_array_get(arr_33, 32), 99);
            assert_eq!(sigil_mut_byte_array_get(arr_33, 0), 7);
        }
        let arr_64 = sigil_mut_byte_array_new(64, 11);
        unsafe {
            assert_eq!(sigil_mut_byte_array_length(arr_64), 64);
            sigil_mut_byte_array_set(arr_64, 63, 200);
            assert_eq!(sigil_mut_byte_array_get(arr_64, 63), 200);
            assert_eq!(sigil_mut_byte_array_get(arr_64, 0), 11);
        }
    }

    #[test]
    fn header_tag_is_mut_byte_array() {
        let _guard = gc_test_lock();
        let arr = sigil_mut_byte_array_new(0, 0);
        unsafe {
            let header_ptr: *const u64 = arr.cast();
            let header = Header(header_ptr.read());
            assert_eq!(header.type_tag(), TAG_MUT_BYTE_ARRAY);
            // Bitmap is zero — bytes are scalars, no GC pointers.
            assert_eq!(header.pointer_bitmap(), 0);
        }
    }
}
