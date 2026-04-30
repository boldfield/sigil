//! Boxed `Int64` runtime primitives — Plan C Task 69.
//!
//! Sigil's user-facing `Int` is a 63-bit signed integer (one bit
//! is reserved for the value-tag at the FFI boundary; range
//! `[-2^62, 2^62)`). For computations that need the full
//! 64-bit-signed range — Unix nanosecond timestamps, large
//! identifiers, OS entropy seeds — `Int64` ships as a boxed,
//! heap-allocated record carrying a native `i64` payload.
//!
//! Layout on the heap:
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_INT64, count = 1, bitmap = 0)
//! offset 8  : i64 payload (the boxed value)
//! ```
//!
//! Bitmap is `0` (atomic alloc): the payload is a scalar, never a
//! pointer, so Boehm uses `GC_malloc_atomic` and skips scanning the
//! payload entirely. This matches the `TAG_BYTE_ARRAY` / `TAG_INT64`
//! atomic-scan precedent.
//!
//! ## Per-op allocation
//!
//! Every arithmetic/conversion op that returns an `Int64` allocates
//! a fresh record. v1 ships immutable boxed values — no in-place
//! mutation, no record reuse. Callers performing tight loops over
//! large Int64 ranges should expect GC pressure proportional to the
//! op count; v2 may add an arena-class for short-lived intermediate
//! Int64s.
//!
//! ## Saturation on `int64_to_int`
//!
//! Converting `Int64` to Sigil's 63-bit `Int` saturates: values
//! outside `[INT_MIN, INT_MAX]` (where `INT_MIN = -2^62` and
//! `INT_MAX = 2^62 - 1` per `runtime/src/value.rs`) clamp to the
//! nearest endpoint. This mirrors Plan C Task 76's saturating
//! `Clock.now()` precedent (PLAN_C_DEVIATIONS.md Task 76 entry).
//! The conversion is lossy on out-of-range inputs; users wanting
//! full-range round-tripping should keep values boxed.
//!
//! ## Division and modulo
//!
//! `int64_div` and `int64_mod` abort the process on `rhs == 0`,
//! mirroring the existing `arith.rs` checked-arithmetic divide-by-
//! zero policy. v2 may surface as `Raise[ArithError]` once
//! per-op generic effects ship.
//!
//! ## Interior-pointer arithmetic
//!
//! Reads and writes use `obj.add(8)` — literal interior pointers
//! into the GC-allocated object. Boehm's conservative scan tolerates
//! interior pointers (walks back to the object's base); each site
//! below uses the pointer transiently for a single aligned read or
//! write before discarding it.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_INT64};
use sigil_abi::tag::{INT_MAX, INT_MIN};

/// Allocate a fresh boxed Int64 with `payload`. Returns the header
/// pointer.
fn alloc_int64(payload: i64) -> *mut u8 {
    let h = Header::new(TAG_INT64, 1, 0);
    let obj = sigil_alloc(h.raw(), 8);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned i64 store).
    unsafe {
        let p: *mut i64 = obj.add(8).cast();
        p.write(payload);
    }
    counters::incr(CounterId::Int64AllocCount);
    counters::add(CounterId::Int64AllocBytes, 16); // header + payload word
    obj
}

/// Read the payload of a boxed Int64.
///
/// # Safety
///
/// `p` must be a pointer to a valid `TAG_INT64` header.
#[inline]
unsafe fn read_int64(p: *const u8) -> i64 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned i64 read).
    let payload: *const i64 = p.add(8).cast();
    payload.read()
}

/// Construct a boxed `Int64` from a Sigil `Int` (63-bit native value
/// at the codegen layer; widening to i64 is a no-op).
#[no_mangle]
pub extern "C" fn sigil_int64_from_int(v: i64) -> *mut u8 {
    alloc_int64(v)
}

/// Add two boxed Int64s, returning a fresh box. Wraps on overflow
/// (i64 two's-complement wrap; matches Sigil's `Int` Plan A2 wrap
/// semantics).
///
/// # Safety
///
/// `a` and `b` must each be pointers to valid `TAG_INT64` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_add(a: *const u8, b: *const u8) -> *mut u8 {
    let r = read_int64(a).wrapping_add(read_int64(b));
    alloc_int64(r)
}

/// Subtract `b` from `a`. Wraps on overflow.
///
/// # Safety
///
/// As `sigil_int64_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_sub(a: *const u8, b: *const u8) -> *mut u8 {
    let r = read_int64(a).wrapping_sub(read_int64(b));
    alloc_int64(r)
}

/// Multiply two boxed Int64s. Wraps on overflow.
///
/// # Safety
///
/// As `sigil_int64_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_mul(a: *const u8, b: *const u8) -> *mut u8 {
    let r = read_int64(a).wrapping_mul(read_int64(b));
    alloc_int64(r)
}

/// Integer divide `a / b` (truncated toward zero, matching Rust's
/// `/` for signed integers). Aborts on `b == 0`. The single
/// overflow case `i64::MIN / -1` also aborts (mirrors `arith.rs`'s
/// `sigil_checked_div` policy).
///
/// # Safety
///
/// As `sigil_int64_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_div(a: *const u8, b: *const u8) -> *mut u8 {
    let av = read_int64(a);
    let bv = read_int64(b);
    if bv == 0 {
        eprintln!("sigil_int64_div: division by zero");
        std::process::abort();
    }
    if av == i64::MIN && bv == -1 {
        eprintln!("sigil_int64_div: i64::MIN / -1 overflow");
        std::process::abort();
    }
    alloc_int64(av / bv)
}

/// Modulo `a % b` (Rust signed-rem semantics: result has sign of
/// `a`). Aborts on `b == 0`.
///
/// # Safety
///
/// As `sigil_int64_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_mod(a: *const u8, b: *const u8) -> *mut u8 {
    let av = read_int64(a);
    let bv = read_int64(b);
    if bv == 0 {
        eprintln!("sigil_int64_mod: modulo by zero");
        std::process::abort();
    }
    // i64::MIN % -1 is defined in Rust as 0 (no overflow), so we
    // accept it without the extra abort that div has.
    alloc_int64(av.wrapping_rem(bv))
}

/// Negate a boxed Int64. Wraps on `i64::MIN` (value stays
/// `i64::MIN` per two's-complement; matches Rust's
/// `wrapping_neg`).
///
/// # Safety
///
/// `p` must be a pointer to a valid `TAG_INT64` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_neg(p: *const u8) -> *mut u8 {
    alloc_int64(read_int64(p).wrapping_neg())
}

/// Equality comparison. Returns 1 (Sigil `True`) or 0 (`False`).
///
/// # Safety
///
/// `a` and `b` must each be pointers to valid `TAG_INT64` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_eq(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_int64(a) == read_int64(b))
}

/// Less-than comparison.
///
/// # Safety
///
/// As `sigil_int64_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_lt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_int64(a) < read_int64(b))
}

/// Less-than-or-equal comparison.
///
/// # Safety
///
/// As `sigil_int64_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_le(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_int64(a) <= read_int64(b))
}

/// Greater-than comparison.
///
/// # Safety
///
/// As `sigil_int64_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_gt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_int64(a) > read_int64(b))
}

/// Greater-than-or-equal comparison.
///
/// # Safety
///
/// As `sigil_int64_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_ge(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_int64(a) >= read_int64(b))
}

/// Convert a boxed Int64 to Sigil's `Int` (63-bit), saturating
/// out-of-range values to `[INT_MIN, INT_MAX]`. Returns the native
/// i64 the codegen layer expects for `Int`-typed values.
///
/// # Safety
///
/// `p` must be a pointer to a valid `TAG_INT64` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_to_int(p: *const u8) -> i64 {
    read_int64(p).clamp(INT_MIN, INT_MAX)
}

/// Format a boxed Int64 as a decimal string. Returns a freshly-
/// allocated `TAG_STRING`.
///
/// # Safety
///
/// `p` must be a pointer to a valid `TAG_INT64` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_int64_to_string(p: *const u8) -> *mut u8 {
    let v = read_int64(p);
    let s = v.to_string();
    // SAFETY: gc-heap-ptr arithmetic (Rust-owned String buffer; sigil_string_new copies into a fresh GC allocation).
    crate::gc::sigil_string_new(s.as_ptr(), s.len())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    unsafe fn read(p: *const u8) -> i64 {
        read_int64(p)
    }

    #[test]
    fn from_int_round_trips_payload() {
        let _g = gc_test_lock();
        let p = sigil_int64_from_int(42);
        unsafe {
            assert_eq!(read(p), 42);
        }
    }

    #[test]
    fn from_int_negative_round_trips() {
        let _g = gc_test_lock();
        let p = sigil_int64_from_int(-1);
        unsafe {
            assert_eq!(read(p), -1);
        }
    }

    #[test]
    fn add_sums_payloads() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(10);
        let b = sigil_int64_from_int(32);
        unsafe {
            let r = sigil_int64_add(a, b);
            assert_eq!(read(r), 42);
        }
    }

    #[test]
    fn add_wraps_on_overflow() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(i64::MAX);
        let b = sigil_int64_from_int(1);
        unsafe {
            let r = sigil_int64_add(a, b);
            assert_eq!(read(r), i64::MIN); // wrap
        }
    }

    #[test]
    fn sub_subtracts() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(50);
        let b = sigil_int64_from_int(8);
        unsafe {
            let r = sigil_int64_sub(a, b);
            assert_eq!(read(r), 42);
        }
    }

    #[test]
    fn mul_multiplies() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(6);
        let b = sigil_int64_from_int(7);
        unsafe {
            let r = sigil_int64_mul(a, b);
            assert_eq!(read(r), 42);
        }
    }

    #[test]
    fn div_truncates_toward_zero() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(-10);
        let b = sigil_int64_from_int(3);
        unsafe {
            let r = sigil_int64_div(a, b);
            assert_eq!(read(r), -3); // toward zero, not floor
        }
    }

    #[test]
    fn modulo_rust_semantics() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(-10);
        let b = sigil_int64_from_int(3);
        unsafe {
            let r = sigil_int64_mod(a, b);
            // -10 = -3 * 3 + (-1); Rust signed-rem keeps sign of `a`.
            assert_eq!(read(r), -1);
        }
    }

    #[test]
    fn neg_negates() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(42);
        unsafe {
            let r = sigil_int64_neg(a);
            assert_eq!(read(r), -42);
        }
    }

    #[test]
    fn neg_min_wraps_to_self() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(i64::MIN);
        unsafe {
            let r = sigil_int64_neg(a);
            assert_eq!(read(r), i64::MIN); // -i64::MIN wraps to i64::MIN
        }
    }

    #[test]
    fn comparisons_return_correct_bools() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(10);
        let b = sigil_int64_from_int(20);
        unsafe {
            assert_eq!(sigil_int64_eq(a, a), 1);
            assert_eq!(sigil_int64_eq(a, b), 0);
            assert_eq!(sigil_int64_lt(a, b), 1);
            assert_eq!(sigil_int64_lt(b, a), 0);
            assert_eq!(sigil_int64_le(a, a), 1);
            assert_eq!(sigil_int64_gt(b, a), 1);
            assert_eq!(sigil_int64_ge(a, a), 1);
        }
    }

    #[test]
    fn to_int_in_range_passes_through() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(123);
        unsafe {
            assert_eq!(sigil_int64_to_int(a), 123);
        }
    }

    #[test]
    fn to_int_above_max_saturates() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(i64::MAX);
        unsafe {
            assert_eq!(sigil_int64_to_int(a), INT_MAX);
        }
    }

    #[test]
    fn to_int_below_min_saturates() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(i64::MIN);
        unsafe {
            assert_eq!(sigil_int64_to_int(a), INT_MIN);
        }
    }

    #[test]
    fn to_string_formats_decimal() {
        let _g = gc_test_lock();
        let a = sigil_int64_from_int(-12345);
        unsafe {
            let s = sigil_int64_to_string(a);
            // String header is followed by length word at +8 then payload at +16.
            let len = crate::gc::sigil_string_len(s);
            assert_eq!(len, 6); // "-12345"
            let payload: *const u8 = s.add(16);
            let bytes = std::slice::from_raw_parts(payload, len);
            assert_eq!(bytes, b"-12345");
        }
    }
}
