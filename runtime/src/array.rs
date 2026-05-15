//! Immutable `Array[A]` runtime primitives — Plan C Task 65.
//!
//! Layout on the heap:
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_ARRAY, count = 0, bitmap = 1)
//! offset 8  : u64 length (number of elements)
//! offset 16 : element[0]      — 8-byte slot
//! offset 24 : element[1]
//! ...
//! offset 16 + i*8 : element[i]
//! ```
//!
//! Each element occupies a 64-bit slot. `Int` and pointer types fit
//! directly; narrower types (`Bool`, `Char`, `Byte`) are widened at
//! write and narrowed at read by codegen — same convention used at
//! the handler-arm `args_ptr` boundary in `handlers.rs`.
//!
//! ## Why `count = 0` and `bitmap = 1`
//!
//! The 8-byte header's `count` field is 6 bits — capped at 63 payload
//! words. A Sudoku-board array (81 elements + 1 length word = 82
//! payload words) would overflow. v1 sidesteps this by writing
//! `count = 0` and relying on Boehm's allocator-tracked size for
//! scanning. The pointer bitmap is set to a non-zero value (`1`) so
//! `sigil_alloc`'s `(bitmap != 0, count == 0)` dispatch routes the
//! object through `GC_malloc` (conservative scan over the whole
//! block) rather than `GC_malloc_atomic` or
//! `GC_malloc_explicitly_typed`. The runtime cannot distinguish
//! per-element pointer-ness at allocation time — that information
//! lives in the codegen-monomorphized type — so conservative scan is
//! the correct v1 default. The v2 typed-walker work shipped via
//! `TAG_EXTERNAL_DESCRIPTOR` (0xFF) will let arrays of pure-scalar
//! `A` (Int, Bool) opt into atomic scanning.
//!
//! ## Out-of-bounds access
//!
//! `sigil_array_get` and `sigil_array_set` abort on out-of-bounds
//! indices. v2 may surface this as a `Raise[BoundsError]` effect; v1
//! aborts directly so the rest of the runtime stays simple.
//!
//! ## Interior-pointer arithmetic
//!
//! Reads and writes against the length word and element slots use
//! `obj.add(8)` / `obj.add(16)` — literal interior pointers into the
//! GC-allocated object. Boehm's conservative scan tolerates interior
//! pointers (the scan walks any pointer back to the start of its
//! containing allocation), so an interior pointer alone is sufficient
//! to keep the object live. The `gc-heap-ptr arithmetic` SAFETY
//! markers below acknowledge each such site; the parenthetical
//! reasoning notes that each interior pointer is transient (used for
//! a single aligned read or write) and never escapes into long-lived
//! storage.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_ARRAY};

/// Allocate a fresh array of `len` elements, each initialised to
/// `fill`. Returns the header pointer (the start of the GC-allocated
/// object). `len` is the number of elements; the total allocation is
/// `8 (header) + 8 (length) + 8*len (elements)`.
///
/// # Safety
///
/// Safe to call from any thread (Boehm-managed allocation under the
/// hood). On extreme allocation failure Boehm aborts the process.
#[no_mangle]
pub extern "C" fn sigil_array_alloc(len: u64, fill: u64) -> *mut u8 {
    let payload_bytes = 8usize.saturating_add((len as usize).saturating_mul(8));
    // count = 0 (unused for arrays — see module docs); bitmap = 1
    // forces Boehm conservative scan over the payload.
    let h = Header::new(TAG_ARRAY, 0, 1);
    // count=0 + bitmap!=0 → conservative GC_malloc path;
    // descriptor_index unused.
    let obj = sigil_alloc(h.raw(), payload_bytes, u32::MAX);

    // Length word at offset 8.
    //
    // SAFETY: gc-heap-ptr arithmetic (used transiently for a single
    // aligned u64 store). `obj` was just returned by `sigil_alloc` and
    // owns at least `8 + payload_bytes` bytes.
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(len);
    }

    // Fill element slots at offsets 16, 24, 32, ...
    if len > 0 {
        unsafe {
            // SAFETY: gc-heap-ptr arithmetic (offset 16 is used for a transient u64 store base; indices stay within the allocated payload).
            let elems_ptr: *mut u64 = obj.add(16).cast();
            for i in 0..(len as usize) {
                // SAFETY: gc-heap-ptr arithmetic (transient elems_ptr.add(i) for one aligned u64 write).
                elems_ptr.add(i).write(fill);
            }
        }
    }

    counters::incr(CounterId::ArrayAllocCount);
    counters::add(CounterId::ArrayAllocBytes, (8 + payload_bytes) as u64);

    obj
}

/// Allocate an empty array. Functionally `sigil_array_alloc(0, 0)`
/// but exposed as a separate FFI symbol so codegen can lower
/// `array_empty[A]()` (a generic builtin with no value args) without
/// having to synthesize a default value of type `A`. The empty
/// array's `fill` byte pattern is irrelevant — there are no slots to
/// initialise.
#[no_mangle]
pub extern "C" fn sigil_array_empty() -> *mut u8 {
    sigil_array_alloc(0, 0)
}

/// Read an array's length. Caller passes the header-pointer form.
///
/// # Safety
///
/// `arr` must be a pointer to a valid array header previously
/// returned by `sigil_array_alloc` (or `sigil_array_empty` /
/// `sigil_array_set`).
#[no_mangle]
pub unsafe extern "C" fn sigil_array_length(arr: *const u8) -> u64 {
    // SAFETY: gc-heap-ptr arithmetic (used transiently for one read).
    let len_ptr: *const u64 = arr.add(8).cast();
    len_ptr.read()
}

/// Read element `i` from the array. Aborts on out-of-bounds.
///
/// # Safety
///
/// `arr` must be a pointer to a valid array header previously
/// returned by `sigil_array_alloc` / `sigil_array_empty` /
/// `sigil_array_set`. `i` must be `< sigil_array_length(arr)` —
/// out-of-bounds aborts the process.
#[no_mangle]
pub unsafe extern "C" fn sigil_array_get(arr: *const u8, i: u64) -> u64 {
    let len = sigil_array_length(arr);
    if i >= len {
        eprintln!("sigil_array_get: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: gc-heap-ptr arithmetic (offset 16 is a transient base for one aligned u64 read; bounds-checked above).
    let elems_ptr: *const u64 = arr.add(16).cast();
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one aligned u64 read).
    elems_ptr.add(i as usize).read()
}

/// Set element `i` to `val`, returning a fresh array — the
/// immutable contract. The original array is unchanged. Aborts on
/// out-of-bounds.
///
/// # Safety
///
/// Same as `sigil_array_get`. The returned pointer is a fresh
/// allocation; the caller is responsible for using it (the original
/// `arr` remains valid and unchanged for as long as it has GC roots).
#[no_mangle]
pub unsafe extern "C" fn sigil_array_set(arr: *const u8, i: u64, val: u64) -> *mut u8 {
    let len = sigil_array_length(arr);
    if i >= len {
        eprintln!("sigil_array_set: index {i} out of bounds (len {len})");
        std::process::abort();
    }

    // Allocate a fresh array of the same length, zero-filled. Zero is
    // a GC-safe bit pattern for any A: a null pointer is reachable as
    // null, and an integer zero is harmless. The fill is overwritten
    // immediately by the `copy_nonoverlapping` below; passing zero
    // avoids the wasted per-slot fill loop inside `sigil_array_alloc`
    // for the case where every slot will be replaced.
    let new_arr = sigil_array_alloc(len, 0);

    // Copy the original elements into the new array.
    // SAFETY: gc-heap-ptr arithmetic (transient base for one contiguous u64 region copy; both arrays have the same len by construction).
    let src_ptr: *const u64 = arr.add(16).cast();
    if len > 0 {
        // SAFETY: gc-heap-ptr arithmetic (transient base for one contiguous u64 region copy; both arrays have the same len by construction).
        let dst_ptr: *mut u64 = new_arr.add(16).cast();
        std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, len as usize);
    }

    // Overwrite slot `i`.
    // SAFETY: gc-heap-ptr arithmetic (transient base + add for one aligned u64 write; bounds-checked above).
    let dst_ptr: *mut u64 = new_arr.add(16).cast();
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one aligned u64 write).
    dst_ptr.add(i as usize).write(val);

    new_arr
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    #[test]
    fn alloc_zero_length_returns_valid_array() {
        let _guard = gc_test_lock();
        let arr = sigil_array_alloc(0, 0);
        unsafe {
            assert_eq!(sigil_array_length(arr), 0);
        }
    }

    #[test]
    fn alloc_with_fill_initialises_all_slots() {
        let _guard = gc_test_lock();
        let arr = sigil_array_alloc(5, 42);
        unsafe {
            assert_eq!(sigil_array_length(arr), 5);
            for i in 0..5 {
                assert_eq!(sigil_array_get(arr, i), 42, "slot {i}");
            }
        }
    }

    #[test]
    fn empty_returns_zero_length() {
        let _guard = gc_test_lock();
        let arr = sigil_array_empty();
        unsafe {
            assert_eq!(sigil_array_length(arr), 0);
        }
    }

    #[test]
    fn set_returns_fresh_array_preserving_original() {
        let _guard = gc_test_lock();
        let arr = sigil_array_alloc(3, 0);
        unsafe {
            let arr2 = sigil_array_set(arr, 1, 99);
            // Original unchanged.
            assert_eq!(sigil_array_get(arr, 0), 0);
            assert_eq!(sigil_array_get(arr, 1), 0);
            assert_eq!(sigil_array_get(arr, 2), 0);
            // Fresh array has the update.
            assert_eq!(sigil_array_get(arr2, 0), 0);
            assert_eq!(sigil_array_get(arr2, 1), 99);
            assert_eq!(sigil_array_get(arr2, 2), 0);
            // Pointers are distinct allocations.
            assert_ne!(arr as *const u8, arr2 as *const u8);
        }
    }

    #[test]
    fn set_chain_threads_updates() {
        let _guard = gc_test_lock();
        let arr0 = sigil_array_alloc(4, 0);
        unsafe {
            let arr1 = sigil_array_set(arr0, 0, 10);
            let arr2 = sigil_array_set(arr1, 1, 20);
            let arr3 = sigil_array_set(arr2, 2, 30);
            let arr4 = sigil_array_set(arr3, 3, 40);

            assert_eq!(sigil_array_get(arr4, 0), 10);
            assert_eq!(sigil_array_get(arr4, 1), 20);
            assert_eq!(sigil_array_get(arr4, 2), 30);
            assert_eq!(sigil_array_get(arr4, 3), 40);
        }
    }

    #[test]
    fn alloc_at_sudoku_size_works_despite_count_field_overflow() {
        // 81-element array — Sudoku-board sized. The header's count
        // field is 6 bits (max 63) so this would overflow if the
        // runtime relied on `count` for size. The Plan C v1 design
        // writes `count = 0` and relies on Boehm's allocator-tracked
        // size; this test pins that the layout works at sizes
        // beyond the count cap.
        let _guard = gc_test_lock();
        let arr = sigil_array_alloc(81, 0);
        unsafe {
            assert_eq!(sigil_array_length(arr), 81);
            for i in 0..81 {
                assert_eq!(sigil_array_get(arr, i), 0);
            }
        }
    }

    #[test]
    fn alloc_at_count_field_boundary_works() {
        // The 6-bit header count field caps at 63. Two boundary
        // tests: len=33 (mid-range, not yet overflowing the cap)
        // and len=64 (one past the cap, the first size where
        // count=0's sidestep is load-bearing). Both confirm the
        // count-from-payload-length-word convention works
        // end-to-end without relying on the header `count` field.
        let _guard = gc_test_lock();
        let arr_33 = sigil_array_alloc(33, 7);
        unsafe {
            assert_eq!(sigil_array_length(arr_33), 33);
            for i in 0..33 {
                assert_eq!(sigil_array_get(arr_33, i), 7, "slot {i} of len-33 arr");
            }
        }
        let arr_64 = sigil_array_alloc(64, 11);
        unsafe {
            assert_eq!(sigil_array_length(arr_64), 64);
            for i in 0..64 {
                assert_eq!(sigil_array_get(arr_64, i), 11, "slot {i} of len-64 arr");
            }
        }
    }

    #[test]
    fn header_tag_is_array() {
        let _guard = gc_test_lock();
        let arr = sigil_array_alloc(0, 0);
        unsafe {
            let header_ptr: *const u64 = arr.cast();
            let header = Header(header_ptr.read());
            assert_eq!(header.type_tag(), TAG_ARRAY);
            assert_eq!(header.payload_count(), 0);
            assert_ne!(header.pointer_bitmap(), 0);
        }
    }
}
