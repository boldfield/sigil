//! Immutable `ByteArray` runtime primitives — Plan C Task 66.5.
//!
//! Specialised flat-byte representation: every element is a single
//! byte (1 slot wide), packed back-to-back with no per-element boxing.
//! Compare with `runtime/src/array.rs`'s `Array[A]` which always uses
//! 64-bit slots regardless of element type — the latter pays an 8x
//! space overhead on byte-typed payloads in exchange for uniform
//! widening at codegen time. ByteArray exists for the byte-heavy
//! workloads (network buffers, file IO, string interop).
//!
//! Layout on the heap:
//!
//! ```text
//! offset 0      : 8-byte header (tag = TAG_BYTE_ARRAY, count = 0, bitmap = 0)
//! offset 8      : u64 length (number of bytes)
//! offset 16     : byte[0]  (1 byte)
//! offset 17     : byte[1]
//! ...
//! offset 16 + i : byte[i]
//! offset 16 + n : zero-pad up to a word boundary
//! ```
//!
//! ## Why `count = 0` and `bitmap = 0`
//!
//! The header `count` field is 6 bits — capped at 63 payload words.
//! A 4 KiB byte buffer would overflow. Mirror `TAG_ARRAY`'s
//! workaround: `count = 0` and rely on Boehm's allocator-tracked
//! size for scanning. The bitmap is 0 (atomic alloc): bytes are
//! pure scalars, never pointers, so Boehm uses
//! `GC_malloc_atomic` and skips scanning the payload entirely
//! (saves mark-phase cost vs `TAG_ARRAY`'s conservative-scan
//! bitmap = 1).
//!
//! ## Out-of-bounds access
//!
//! `sigil_byte_array_get` and `sigil_byte_array_slice` abort on
//! out-of-bounds indices, mirroring the `Array` behaviour. v2 may
//! surface these as `Raise[BoundsError]` effects.
//!
//! ## Interior-pointer arithmetic
//!
//! Reads and writes use `obj.add(8)` / `obj.add(16)` — literal
//! interior pointers into the GC-allocated object. Boehm's
//! conservative scan tolerates interior pointers (walks back to the
//! object's base); each site below uses the pointer transiently
//! for a single aligned read or write before discarding it.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_BYTE_ARRAY};

/// Round up to the next multiple of 8, used to compute the padded
/// byte payload size. Mirrors `gc::round_up_to_word` (kept private
/// to its module).
#[inline]
fn round_up_to_word(n: usize) -> usize {
    (n + 7) & !7
}

/// Compute the total payload size for a byte-array of `len` bytes:
/// 8 bytes for the length word + `len` bytes for the payload, padded
/// to a word boundary.
#[inline]
fn payload_bytes_for(len: u64) -> usize {
    8usize.saturating_add(round_up_to_word(len as usize))
}

/// Allocate a fresh byte-array of `len` bytes, each initialised to
/// `fill`. Returns the header pointer. Aborts on negative `len`
/// (when sigil-side `Int` reinterprets to a near-`u64::MAX`) so the
/// failure surfaces as a clear runtime message rather than an
/// opaque allocator error.
///
/// # Safety
///
/// Safe to call from any thread (Boehm-managed allocation).
#[no_mangle]
pub extern "C" fn sigil_byte_array_alloc(len: u64, fill: u8) -> *mut u8 {
    if (len as i64) < 0 {
        eprintln!("sigil_byte_array_alloc: negative length {}", len as i64);
        std::process::abort();
    }
    let payload_bytes = payload_bytes_for(len);
    let h = Header::new(TAG_BYTE_ARRAY, 0, 0);
    let obj = sigil_alloc(h.raw(), payload_bytes);

    // Length word at offset 8.
    //
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(len);
    }

    // Fill byte payload at offsets 16..16+len. GC_malloc_atomic
    // returns zeroed memory in Boehm; skip the per-byte fill loop
    // when `fill == 0` since the allocation is already zeroed.
    if len > 0 && fill != 0 {
        // SAFETY: gc-heap-ptr arithmetic (transient base for a single contiguous byte fill; bounds are `[16, 16+len)`).
        unsafe {
            let bytes_ptr: *mut u8 = obj.add(16);
            std::ptr::write_bytes(bytes_ptr, fill, len as usize);
        }
    }

    counters::incr(CounterId::ByteArrayAllocCount);
    counters::add(CounterId::ByteArrayAllocBytes, (8 + payload_bytes) as u64);

    obj
}

/// Allocate an empty byte-array.
#[no_mangle]
pub extern "C" fn sigil_byte_array_empty() -> *mut u8 {
    sigil_byte_array_alloc(0, 0)
}

/// Read a byte-array's length.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_BYTE_ARRAY` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_byte_array_length(arr: *const u8) -> u64 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one u64 read).
    let len_ptr: *const u64 = arr.add(8).cast();
    len_ptr.read()
}

/// Read byte `i`. Aborts on out-of-bounds.
///
/// # Safety
///
/// Same as `sigil_byte_array_length`. `i` must be `< length(arr)`.
#[no_mangle]
pub unsafe extern "C" fn sigil_byte_array_get(arr: *const u8, i: u64) -> u8 {
    if (i as i64) < 0 {
        eprintln!("sigil_byte_array_get: negative index {}", i as i64);
        std::process::abort();
    }
    let len = sigil_byte_array_length(arr);
    if i >= len {
        eprintln!("sigil_byte_array_get: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: gc-heap-ptr arithmetic (transient base + bounds-checked offset for one byte read).
    let bytes_ptr: *const u8 = arr.add(16);
    // SAFETY: gc-heap-ptr arithmetic (transient add(i) for one byte read).
    bytes_ptr.add(i as usize).read()
}

/// Concatenate two byte-arrays into a fresh array.
///
/// # Safety
///
/// `a` and `b` must each be pointers to valid `TAG_BYTE_ARRAY`
/// headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_byte_array_concat(a: *const u8, b: *const u8) -> *mut u8 {
    let la = sigil_byte_array_length(a);
    let lb = sigil_byte_array_length(b);
    // Abort on length overflow rather than silently producing a
    // wrong-sized allocation. Saturating addition would truncate
    // the requested size and leave the user with an array shorter
    // than `la + lb`; honest abort surfaces the impossibility.
    let total = match la.checked_add(lb) {
        Some(n) => n,
        None => {
            eprintln!("sigil_byte_array_concat: length overflow ({la} + {lb} exceeds u64::MAX)");
            std::process::abort();
        }
    };
    let out = sigil_byte_array_alloc(total, 0);

    if la > 0 {
        // SAFETY: gc-heap-ptr arithmetic (contiguous source/destination payload regions; lengths bounded by allocation).
        let src: *const u8 = a.add(16);
        let dst: *mut u8 = out.add(16);
        std::ptr::copy_nonoverlapping(src, dst, la as usize);
    }
    if lb > 0 {
        // SAFETY: gc-heap-ptr arithmetic (destination offset by la, source from b's payload).
        let src: *const u8 = b.add(16);
        let dst: *mut u8 = out.add(16).add(la as usize);
        std::ptr::copy_nonoverlapping(src, dst, lb as usize);
    }
    out
}

/// Slice `[start, end)` from `arr` into a fresh byte-array. Aborts
/// when `start > end` or `end > length(arr)`. An empty slice
/// (`start == end`) returns a fresh zero-length array.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_BYTE_ARRAY` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_byte_array_slice(arr: *const u8, start: u64, end: u64) -> *mut u8 {
    if (start as i64) < 0 {
        eprintln!("sigil_byte_array_slice: negative start {}", start as i64);
        std::process::abort();
    }
    if (end as i64) < 0 {
        eprintln!("sigil_byte_array_slice: negative end {}", end as i64);
        std::process::abort();
    }
    let len = sigil_byte_array_length(arr);
    if start > end {
        eprintln!("sigil_byte_array_slice: start {start} > end {end}");
        std::process::abort();
    }
    if end > len {
        eprintln!("sigil_byte_array_slice: end {end} out of bounds (len {len})");
        std::process::abort();
    }
    let slice_len = end - start;
    let out = sigil_byte_array_alloc(slice_len, 0);
    if slice_len > 0 {
        // SAFETY: gc-heap-ptr arithmetic (contiguous source range bounded by `[start, end) <= [0, len)`; dst is a fresh allocation).
        let src: *const u8 = arr.add(16).add(start as usize);
        let dst: *mut u8 = out.add(16);
        std::ptr::copy_nonoverlapping(src, dst, slice_len as usize);
    }
    out
}

/// Convert a Sigil `String` to a `ByteArray` containing the same
/// UTF-8 byte payload. Always succeeds (Sigil strings are guaranteed
/// valid UTF-8 by construction).
///
/// # Safety
///
/// `s` must be a pointer to a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_to_bytes(s: *const u8) -> *mut u8 {
    let len = crate::gc::sigil_string_len(s) as u64;
    let out = sigil_byte_array_alloc(len, 0);
    if len > 0 {
        // SAFETY: gc-heap-ptr arithmetic (source is the string's UTF-8 payload at offset 16; dst is the byte-array's payload at offset 16; same length on both sides).
        let src: *const u8 = s.add(16);
        let dst: *mut u8 = out.add(16);
        std::ptr::copy_nonoverlapping(src, dst, len as usize);
    }
    out
}

/// Validate a byte-array as UTF-8. Returns `-1` (as `i64`) when the
/// payload is valid UTF-8; otherwise the byte offset (`>= 0`) of the
/// first invalid byte. Sigil-side `string_from_bytes` consumes this
/// to construct `Result[String, Utf8Error]` (Ok / InvalidUtf8(offset)).
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_BYTE_ARRAY` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_from_bytes_validate(arr: *const u8) -> i64 {
    let len = sigil_byte_array_length(arr) as usize;
    let bytes_ptr: *const u8 = arr.add(16);
    let slice = std::slice::from_raw_parts(bytes_ptr, len);
    match std::str::from_utf8(slice) {
        Ok(_) => -1,
        Err(e) => e.valid_up_to() as i64,
    }
}

/// Allocate a fresh `String` from a previously-validated byte-array.
/// The caller is responsible for having called
/// `sigil_string_from_bytes_validate` and confirmed the byte payload
/// is valid UTF-8 — this primitive copies the bytes verbatim into a
/// `TAG_STRING` header without re-validating.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_BYTE_ARRAY` header whose
/// byte payload is valid UTF-8 (per
/// `sigil_string_from_bytes_validate(arr) == -1`).
#[no_mangle]
pub unsafe extern "C" fn sigil_string_from_bytes_alloc(arr: *const u8) -> *mut u8 {
    let len = sigil_byte_array_length(arr) as usize;
    let bytes_ptr: *const u8 = arr.add(16);
    crate::gc::sigil_string_new(bytes_ptr, len)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    #[test]
    fn alloc_zero_length_returns_valid_array() {
        let _guard = gc_test_lock();
        let arr = sigil_byte_array_alloc(0, 0);
        unsafe {
            assert_eq!(sigil_byte_array_length(arr), 0);
        }
    }

    #[test]
    fn alloc_with_fill_initialises_all_bytes() {
        let _guard = gc_test_lock();
        let arr = sigil_byte_array_alloc(7, 0xAB);
        unsafe {
            assert_eq!(sigil_byte_array_length(arr), 7);
            for i in 0..7 {
                assert_eq!(sigil_byte_array_get(arr, i), 0xAB, "byte {i}");
            }
        }
    }

    #[test]
    fn empty_returns_zero_length() {
        let _guard = gc_test_lock();
        let arr = sigil_byte_array_empty();
        unsafe {
            assert_eq!(sigil_byte_array_length(arr), 0);
        }
    }

    #[test]
    fn alloc_at_word_padding_boundaries() {
        // Lengths 1, 7, 8, 9, 33, 64 — exercise the round-up-to-word
        // logic across the boundary cases.
        let _guard = gc_test_lock();
        for len in [1u64, 7, 8, 9, 33, 64] {
            let arr = sigil_byte_array_alloc(len, 0x42);
            unsafe {
                assert_eq!(sigil_byte_array_length(arr), len);
                for i in 0..len {
                    assert_eq!(sigil_byte_array_get(arr, i), 0x42, "len={len} i={i}");
                }
            }
        }
    }

    #[test]
    fn concat_joins_payloads() {
        let _guard = gc_test_lock();
        unsafe {
            let a = sigil_byte_array_alloc(3, 1);
            let b = sigil_byte_array_alloc(2, 2);
            let c = sigil_byte_array_concat(a, b);
            assert_eq!(sigil_byte_array_length(c), 5);
            assert_eq!(sigil_byte_array_get(c, 0), 1);
            assert_eq!(sigil_byte_array_get(c, 1), 1);
            assert_eq!(sigil_byte_array_get(c, 2), 1);
            assert_eq!(sigil_byte_array_get(c, 3), 2);
            assert_eq!(sigil_byte_array_get(c, 4), 2);
        }
    }

    #[test]
    fn concat_empty_left_returns_right_contents() {
        let _guard = gc_test_lock();
        unsafe {
            let a = sigil_byte_array_empty();
            let b = sigil_byte_array_alloc(3, 0xFF);
            let c = sigil_byte_array_concat(a, b);
            assert_eq!(sigil_byte_array_length(c), 3);
            for i in 0..3 {
                assert_eq!(sigil_byte_array_get(c, i), 0xFF);
            }
        }
    }

    #[test]
    fn concat_empty_right_returns_left_contents() {
        let _guard = gc_test_lock();
        unsafe {
            let a = sigil_byte_array_alloc(3, 0x10);
            let b = sigil_byte_array_empty();
            let c = sigil_byte_array_concat(a, b);
            assert_eq!(sigil_byte_array_length(c), 3);
            for i in 0..3 {
                assert_eq!(sigil_byte_array_get(c, i), 0x10);
            }
        }
    }

    #[test]
    fn slice_extracts_subrange() {
        let _guard = gc_test_lock();
        unsafe {
            // Build [0, 1, 2, 3, 4] by setting individual bytes via
            // concat-of-singletons; sigil_byte_array_set doesn't exist
            // (immutable surface).
            let mut arr = sigil_byte_array_alloc(0, 0);
            for i in 0u8..5 {
                let one = sigil_byte_array_alloc(1, i);
                arr = sigil_byte_array_concat(arr, one);
            }
            let s = sigil_byte_array_slice(arr, 1, 4);
            assert_eq!(sigil_byte_array_length(s), 3);
            assert_eq!(sigil_byte_array_get(s, 0), 1);
            assert_eq!(sigil_byte_array_get(s, 1), 2);
            assert_eq!(sigil_byte_array_get(s, 2), 3);
        }
    }

    #[test]
    fn slice_empty_range_returns_zero_length() {
        let _guard = gc_test_lock();
        unsafe {
            let arr = sigil_byte_array_alloc(5, 0xCC);
            let s = sigil_byte_array_slice(arr, 2, 2);
            assert_eq!(sigil_byte_array_length(s), 0);
        }
    }

    #[test]
    fn header_tag_is_byte_array() {
        let _guard = gc_test_lock();
        let arr = sigil_byte_array_alloc(0, 0);
        unsafe {
            let header_ptr: *const u64 = arr.cast();
            let header = Header(header_ptr.read());
            assert_eq!(header.type_tag(), TAG_BYTE_ARRAY);
            // Bitmap is zero — bytes are scalars, no GC pointers.
            assert_eq!(header.pointer_bitmap(), 0);
        }
    }

    #[test]
    fn string_to_bytes_round_trips_ascii() {
        let _guard = gc_test_lock();
        unsafe {
            let src = b"hello";
            // SAFETY: gc-heap-ptr arithmetic (src is a static byte literal, not a heap object — false-positive on the heuristic grep).
            let s = crate::gc::sigil_string_new(src.as_ptr(), src.len());
            let ba = sigil_string_to_bytes(s);
            assert_eq!(sigil_byte_array_length(ba), 5);
            for (i, b) in src.iter().enumerate() {
                assert_eq!(sigil_byte_array_get(ba, i as u64), *b);
            }
        }
    }

    #[test]
    fn string_from_bytes_validate_accepts_valid_utf8() {
        let _guard = gc_test_lock();
        unsafe {
            // Build a ByteArray containing "hi" (ASCII => valid UTF-8).
            let mut arr = sigil_byte_array_alloc(0, 0);
            for b in [b'h', b'i'] {
                let one = sigil_byte_array_alloc(1, b);
                arr = sigil_byte_array_concat(arr, one);
            }
            assert_eq!(sigil_string_from_bytes_validate(arr), -1);
            let s = sigil_string_from_bytes_alloc(arr);
            assert_eq!(crate::gc::sigil_string_len(s), 2);
        }
    }

    #[test]
    fn string_from_bytes_validate_rejects_invalid_utf8() {
        let _guard = gc_test_lock();
        unsafe {
            // 0xFF is invalid as a leading UTF-8 byte at any position.
            // Build [0x68 'h', 0xFF, 0x69 'i']; offset 1 is the first
            // invalid byte.
            let mut arr = sigil_byte_array_alloc(0, 0);
            for b in [0x68, 0xFF, 0x69] {
                let one = sigil_byte_array_alloc(1, b);
                arr = sigil_byte_array_concat(arr, one);
            }
            assert_eq!(sigil_string_from_bytes_validate(arr), 1);
        }
    }
}
