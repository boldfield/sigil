//! Plan C addendum (CLI external-system effects, EE2) — shared
//! Sigil-value construction helpers used by the new `Env` / `Fs` /
//! `Process` runtime arm fns. The arm fns return raw shapes (tuples,
//! arrays, scalars) only; user-facing `Result` / `Option` / `List`
//! construction lives in stdlib Sigil wrappers (`std/{env,fs,process}.sigil`).
//!
//! ## Why these helpers exist
//!
//! Each fallible op's arm fn allocates one or more Sigil values
//! (Strings, Tuples, Arrays). Boehm's conservative GC scans the
//! Rust call stack for roots; locally-held `*mut u8` pointers are
//! found, but values stored in containers allocated by Rust's
//! `std` allocator (e.g. `Vec<*mut u8>`) are *not* — the Rust
//! allocator's chunks live outside Boehm's scan. To avoid that
//! invisibility window, helpers that produce a multi-element
//! container do it in two steps: allocate the empty container
//! first (rooted via Rust stack as a `*mut u8`), then fill its
//! slots one element at a time. Each freshly-allocated slot value
//! becomes reachable via the rooted container before the next
//! allocation can run a GC pass.

use crate::gc::sigil_alloc;
use sigil_header_constants::{header_word, MAX_TUPLE_ARITY, TAG_ARRAY, TAG_TUPLE};

/// Allocate a Sigil Tuple holding the given pre-widened 64-bit slot
/// values. `pointer_bitmap` follows the `Expr::Tuple` codegen
/// convention (bit `i` set iff element `i` is a heap pointer that
/// the GC should trace). Caller must keep each element root-alive
/// until this call returns — the slots are uninitialized memory
/// between `sigil_alloc` and the per-slot writes, so an interleaved
/// GC pass would only see roots on the Rust stack.
///
/// # Safety
///
/// Same as `sigil_alloc`. `elems.len()` must satisfy
/// `MAX_TUPLE_ARITY`.
pub unsafe fn alloc_tuple(elems: &[u64], pointer_bitmap: u32) -> *mut u8 {
    let n = elems.len();
    debug_assert!(
        n <= MAX_TUPLE_ARITY,
        "alloc_tuple: arity {n} exceeds MAX_TUPLE_ARITY = {MAX_TUPLE_ARITY}",
    );
    let header = header_word(TAG_TUPLE, n as u8, pointer_bitmap);
    let payload_bytes = n * 8;
    let obj = sigil_alloc(header, payload_bytes);
    // SAFETY: gc-heap-ptr arithmetic (transient base pointer for
    // sequential aligned 8-byte stores into a freshly-allocated
    // payload of size `payload_bytes`).
    let base: *mut u64 = obj.add(8).cast();
    for (i, &v) in elems.iter().enumerate() {
        // SAFETY: gc-heap-ptr arithmetic (each `base.add(i)` stays
        // within the payload region above).
        base.add(i).write(v);
    }
    obj
}

/// Allocate a fresh Sigil Array with `len` slots, all initialized
/// to `0` (null). Use the returned pointer as the rooting target
/// for a multi-step fill: each slot is written via
/// [`array_set_slot_raw`] after its value is allocated, ensuring
/// every freshly-allocated value is reachable from the rooted
/// array before the next GC-touching call.
///
/// Mirrors `sigil_array_alloc(len, 0)` from `runtime/src/array.rs`
/// but exposed as an `unsafe fn` so the helper module is free of
/// the public-FFI-symbol shape.
///
/// # Safety
///
/// Safe to call from any thread; Boehm-managed allocation under
/// the hood.
pub unsafe fn alloc_array_with_capacity(len: usize) -> *mut u8 {
    // Layout matches `runtime/src/array.rs`: header (count=0,
    // bitmap=1 forces conservative scan over the payload),
    // length word at offset 8, elements at offset 16+.
    let header = header_word(TAG_ARRAY, 0, 1);
    let payload_bytes = 8usize.saturating_add(len.saturating_mul(8));
    let obj = sigil_alloc(header, payload_bytes);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one
    // aligned u64 store; the length word lives at offset 8 by
    // contract).
    let len_p: *mut u64 = obj.add(8).cast();
    len_p.write(len as u64);
    if len > 0 {
        // SAFETY: gc-heap-ptr arithmetic (transient base for `len`
        // aligned u64 stores into the element region at offsets
        // 16, 24, 32, ...).
        let elems_p: *mut u64 = obj.add(16).cast();
        for i in 0..len {
            elems_p.add(i).write(0);
        }
    }
    obj
}

/// Write `value` into the `idx`-th slot of an array previously
/// allocated by [`alloc_array_with_capacity`]. Caller must
/// guarantee `idx < len`.
///
/// # Safety
///
/// `arr` must be a pointer to a `TAG_ARRAY` header with at least
/// `idx + 1` element slots.
pub unsafe fn array_set_slot_raw(arr: *mut u8, idx: usize, value: u64) {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one
    // aligned u64 store; caller guarantees idx is in range).
    let elems_p: *mut u64 = arr.add(16).cast();
    elems_p.add(idx).write(value);
}

/// Allocate a Sigil Int64 record holding `n`. Mirrors
/// `runtime/src/int64.rs::sigil_int64_from_int` shape (boxed 64-bit
/// signed). Used by `Fs.file_size`'s `(Int, Int64)` tuple-return
/// arm fn since file sizes can exceed `Int`'s 63-bit range.
///
/// # Safety
///
/// Safe to call from any thread.
pub unsafe fn alloc_int64(n: i64) -> *mut u8 {
    use crate::counters::{self, CounterId};
    use sigil_header_constants::TAG_INT64;
    let header = header_word(TAG_INT64, 1, 0);
    let obj = sigil_alloc(header, 8);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one
    // aligned i64 store at offset 8).
    let payload: *mut i64 = obj.add(8).cast();
    payload.write(n);
    counters::incr(CounterId::Int64AllocCount);
    counters::add(CounterId::Int64AllocBytes, 16);
    obj
}

/// Construct a Sigil String from a Rust `&str`. Wraps
/// [`crate::gc::sigil_string_new`] with the `as_ptr()` discipline
/// that the discipline grep flags otherwise.
///
/// # Safety
///
/// Safe to call from any thread; Boehm-managed allocation.
pub unsafe fn alloc_string_from_str(s: &str) -> *mut u8 {
    // SAFETY: gc-heap-ptr arithmetic (Rust-owned `&str` buffer; sigil_string_new copies into a fresh GC alloc).
    crate::gc::sigil_string_new(s.as_ptr(), s.len())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::gc::sigil_string_len;
    use crate::test_support::gc_test_lock;

    #[test]
    fn tuple_of_int_and_string_round_trips() {
        let _g = gc_test_lock();
        unsafe {
            let s = alloc_string_from_str("hello");
            let tup = alloc_tuple(&[42_u64, s as u64], 0b10);
            // Read back: tag at offset 8, value pointer at offset 16.
            let tag = (tup.add(8) as *const i64).read();
            let val_ptr = (tup.add(16) as *const *const u8).read();
            assert_eq!(tag, 42);
            assert_eq!(sigil_string_len(val_ptr), 5);
        }
    }

    #[test]
    fn array_with_capacity_zero_is_legal() {
        let _g = gc_test_lock();
        unsafe {
            let arr = alloc_array_with_capacity(0);
            assert_eq!((arr.add(8) as *const u64).read(), 0);
        }
    }

    #[test]
    fn array_set_slot_round_trips() {
        let _g = gc_test_lock();
        unsafe {
            let arr = alloc_array_with_capacity(3);
            array_set_slot_raw(arr, 0, 100);
            array_set_slot_raw(arr, 1, 200);
            array_set_slot_raw(arr, 2, 300);
            let elems_p: *const u64 = arr.add(16).cast();
            assert_eq!(elems_p.read(), 100);
            assert_eq!(elems_p.add(1).read(), 200);
            assert_eq!(elems_p.add(2).read(), 300);
        }
    }

    #[test]
    fn int64_alloc_round_trips() {
        let _g = gc_test_lock();
        unsafe {
            let p = alloc_int64(1234567890);
            let payload: *const i64 = p.add(8).cast();
            assert_eq!(payload.read(), 1234567890);
        }
    }
}
