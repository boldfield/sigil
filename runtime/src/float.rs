//! Boxed `Float` runtime primitives (IEEE 754 f64).
//!
//! Layout on the heap:
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_FLOAT, count = 1, bitmap = 0)
//! offset 8  : f64 payload (stored as 8 raw bytes)
//! ```
//!
//! Bitmap is `0` (atomic alloc): the payload is a scalar, never a
//! pointer, so Boehm uses `GC_malloc_atomic` and skips scanning.
//! Follows the `TAG_INT64` boxed-scalar precedent exactly.
//!
//! ## Per-op allocation
//!
//! Every arithmetic/math/conversion op that returns a `Float`
//! allocates a fresh record. No in-place mutation, no record reuse.
//!
//! ## Division
//!
//! `float_div` follows IEEE 754 semantics: division by zero yields
//! ±Inf (not an abort), unlike `int64_div` which aborts.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_FLOAT};

fn alloc_float(payload: f64) -> *mut u8 {
    let h = Header::new(TAG_FLOAT, 1, 0);
    let obj = sigil_alloc(h.raw(), 8);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned f64 store).
    unsafe {
        let p: *mut f64 = obj.add(8).cast();
        p.write(payload);
    }
    counters::incr(CounterId::FloatAllocCount);
    counters::add(CounterId::FloatAllocBytes, 16);
    obj
}

#[inline]
unsafe fn read_float(p: *const u8) -> f64 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned f64 read).
    let payload: *const f64 = p.add(8).cast();
    payload.read()
}

// ── Boxing / unboxing ──────────────────────────────────────────────

/// Box an f64 from its bit pattern (passed as i64 to stay in the
/// integer register class, matching codegen's `iconst` + call pattern).
#[no_mangle]
pub extern "C" fn sigil_float_box(bits: i64) -> *mut u8 {
    alloc_float(f64::from_bits(bits as u64))
}

/// # Safety
///
/// `p` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_unbox(p: *const u8) -> f64 {
    read_float(p)
}

// ── Arithmetic ─────────────────────────────────────────────────────

/// # Safety
///
/// `a` and `b` must each point at valid `TAG_FLOAT` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_add(a: *const u8, b: *const u8) -> *mut u8 {
    alloc_float(read_float(a) + read_float(b))
}

/// # Safety
///
/// As `sigil_float_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_sub(a: *const u8, b: *const u8) -> *mut u8 {
    alloc_float(read_float(a) - read_float(b))
}

/// # Safety
///
/// As `sigil_float_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_mul(a: *const u8, b: *const u8) -> *mut u8 {
    alloc_float(read_float(a) * read_float(b))
}

/// IEEE 754 division: div-by-zero yields ±Inf, 0/0 yields NaN.
///
/// # Safety
///
/// As `sigil_float_add`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_div(a: *const u8, b: *const u8) -> *mut u8 {
    alloc_float(read_float(a) / read_float(b))
}

/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_neg(a: *const u8) -> *mut u8 {
    alloc_float(-read_float(a))
}

// ── Comparison ─────────────────────────────────────────────────────

/// # Safety
///
/// `a` and `b` must each point at valid `TAG_FLOAT` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_eq(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_float(a) == read_float(b))
}

/// # Safety
///
/// As `sigil_float_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_lt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_float(a) < read_float(b))
}

/// # Safety
///
/// As `sigil_float_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_le(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_float(a) <= read_float(b))
}

/// # Safety
///
/// As `sigil_float_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_gt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_float(a) > read_float(b))
}

/// # Safety
///
/// As `sigil_float_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_ge(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_float(a) >= read_float(b))
}

// ── Math ───────────────────────────────────────────────────────────

/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_abs(a: *const u8) -> *mut u8 {
    alloc_float(read_float(a).abs())
}

/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_floor(a: *const u8) -> *mut u8 {
    alloc_float(read_float(a).floor())
}

/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_ceil(a: *const u8) -> *mut u8 {
    alloc_float(read_float(a).ceil())
}

/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_sqrt(a: *const u8) -> *mut u8 {
    alloc_float(read_float(a).sqrt())
}

// ── Conversions ────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn sigil_float_from_int(n: i64) -> *mut u8 {
    alloc_float(n as f64)
}

/// Truncate toward zero, clamp to `[i64::MIN, i64::MAX]`, NaN → 0.
///
/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_to_int(a: *const u8) -> i64 {
    let f = read_float(a);
    if f.is_nan() {
        return 0;
    }
    if f >= i64::MAX as f64 {
        return i64::MAX;
    }
    if f <= i64::MIN as f64 {
        return i64::MIN;
    }
    f as i64
}

/// Format as decimal string. Returns a freshly-allocated `TAG_STRING`.
///
/// # Safety
///
/// `a` must point at a valid `TAG_FLOAT` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_float_to_string(a: *const u8) -> *mut u8 {
    let f = read_float(a);
    let mut s = format!("{}", f);
    if !s.contains('.')
        && !s.contains('e')
        && !s.contains('E')
        && s != "inf"
        && s != "-inf"
        && s != "NaN"
    {
        s.push_str(".0");
    }
    crate::gc::sigil_string_new(s.as_ptr(), s.len())
}

/// Validate whether `s` parses as f64. Returns 0 = valid, 1 = invalid.
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_to_float_validate(s: *const u8) -> i64 {
    let (bytes, len) = crate::gc::string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let text = match std::str::from_utf8(slice) {
        Ok(t) => t,
        Err(_) => return 1,
    };
    if text.is_empty() {
        return 1;
    }
    match text.parse::<f64>() {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// Parse `s` as f64 and box. Caller must validate first.
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header AND have passed
/// `sigil_string_to_float_validate`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_to_float_parse(s: *const u8) -> *mut u8 {
    let (bytes, len) = crate::gc::string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let text = match std::str::from_utf8(slice) {
        Ok(t) => t,
        Err(_) => {
            eprintln!(
                "sigil_string_to_float_parse: input is not valid UTF-8; \
                 caller must invoke sigil_string_to_float_validate first"
            );
            std::process::abort();
        }
    };
    match text.parse::<f64>() {
        Ok(v) => alloc_float(v),
        Err(e) => {
            eprintln!(
                "sigil_string_to_float_parse: failed to parse `{text}` ({e}); \
                 caller must invoke sigil_string_to_float_validate first"
            );
            std::process::abort();
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    unsafe fn read(p: *const u8) -> f64 {
        read_float(p)
    }

    fn boxf(f: f64) -> *mut u8 {
        sigil_float_box(f.to_bits() as i64)
    }

    #[test]
    fn box_unbox_round_trip() {
        let _g = gc_test_lock();
        let p = boxf(3.14);
        unsafe {
            assert!((sigil_float_unbox(p) - 3.14).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn box_negative_round_trips() {
        let _g = gc_test_lock();
        let p = boxf(-2.5);
        unsafe {
            assert!((read(p) - (-2.5)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn add_sums() {
        let _g = gc_test_lock();
        let a = boxf(1.5);
        let b = boxf(2.5);
        unsafe {
            let r = sigil_float_add(a, b);
            assert!((read(r) - 4.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn sub_subtracts() {
        let _g = gc_test_lock();
        let a = boxf(5.0);
        let b = boxf(2.0);
        unsafe {
            assert!((read(sigil_float_sub(a, b)) - 3.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn mul_multiplies() {
        let _g = gc_test_lock();
        let a = boxf(3.0);
        let b = boxf(4.0);
        unsafe {
            assert!((read(sigil_float_mul(a, b)) - 12.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn div_divides() {
        let _g = gc_test_lock();
        let a = boxf(10.0);
        let b = boxf(4.0);
        unsafe {
            assert!((read(sigil_float_div(a, b)) - 2.5).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn div_by_zero_yields_inf() {
        let _g = gc_test_lock();
        let a = boxf(1.0);
        let b = boxf(0.0);
        unsafe {
            let r = read(sigil_float_div(a, b));
            assert!(r.is_infinite() && r.is_sign_positive());
        }
    }

    #[test]
    fn neg_negates() {
        let _g = gc_test_lock();
        let a = boxf(3.5);
        unsafe {
            assert!((read(sigil_float_neg(a)) - (-3.5)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn comparisons() {
        let _g = gc_test_lock();
        let a = boxf(1.0);
        let b = boxf(2.0);
        unsafe {
            assert_eq!(sigil_float_eq(a, a), 1);
            assert_eq!(sigil_float_eq(a, b), 0);
            assert_eq!(sigil_float_lt(a, b), 1);
            assert_eq!(sigil_float_lt(b, a), 0);
            assert_eq!(sigil_float_le(a, a), 1);
            assert_eq!(sigil_float_gt(b, a), 1);
            assert_eq!(sigil_float_ge(a, a), 1);
        }
    }

    #[test]
    fn nan_not_equal_to_self() {
        let _g = gc_test_lock();
        let nan = boxf(f64::NAN);
        unsafe {
            assert_eq!(sigil_float_eq(nan, nan), 0);
            assert_eq!(sigil_float_lt(nan, nan), 0);
        }
    }

    #[test]
    fn abs_floor_ceil_sqrt() {
        let _g = gc_test_lock();
        unsafe {
            let neg = boxf(-3.7);
            assert!((read(sigil_float_abs(neg)) - 3.7).abs() < f64::EPSILON);
            assert!((read(sigil_float_floor(neg)) - (-4.0)).abs() < f64::EPSILON);
            assert!((read(sigil_float_ceil(neg)) - (-3.0)).abs() < f64::EPSILON);

            let four = boxf(4.0);
            assert!((read(sigil_float_sqrt(four)) - 2.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn from_int_and_to_int() {
        let _g = gc_test_lock();
        let p = sigil_float_from_int(42);
        unsafe {
            assert!((read(p) - 42.0).abs() < f64::EPSILON);
            assert_eq!(sigil_float_to_int(p), 42);
        }
    }

    #[test]
    fn to_int_clamps_overflow() {
        let _g = gc_test_lock();
        let big = boxf(1e30);
        let small = boxf(-1e30);
        unsafe {
            assert_eq!(sigil_float_to_int(big), i64::MAX);
            assert_eq!(sigil_float_to_int(small), i64::MIN);
        }
    }

    #[test]
    fn to_int_nan_yields_zero() {
        let _g = gc_test_lock();
        let nan = boxf(f64::NAN);
        unsafe {
            assert_eq!(sigil_float_to_int(nan), 0);
        }
    }

    #[test]
    fn to_string_formats() {
        let _g = gc_test_lock();
        let p = boxf(3.14);
        unsafe {
            let s = sigil_float_to_string(p);
            let len = crate::gc::sigil_string_len(s);
            let payload: *const u8 = s.add(16);
            let bytes = std::slice::from_raw_parts(payload, len);
            assert_eq!(bytes, b"3.14");

            let whole = sigil_float_to_string(boxf(4.0));
            let wlen = crate::gc::sigil_string_len(whole);
            let wbytes = std::slice::from_raw_parts(whole.add(16), wlen);
            assert_eq!(wbytes, b"4.0");

            let inf_p = sigil_float_to_string(boxf(f64::INFINITY));
            let ilen = crate::gc::sigil_string_len(inf_p);
            let ibytes = std::slice::from_raw_parts(inf_p.add(16), ilen);
            assert_eq!(ibytes, b"inf");

            let nan_p = sigil_float_to_string(boxf(f64::NAN));
            let nlen = crate::gc::sigil_string_len(nan_p);
            let nbytes = std::slice::from_raw_parts(nan_p.add(16), nlen);
            assert_eq!(nbytes, b"NaN");
        }
    }

    #[test]
    fn validate_and_parse_round_trip() {
        let _g = gc_test_lock();
        unsafe {
            let s = crate::gc::sigil_string_new(b"2.718".as_ptr(), 5);
            assert_eq!(sigil_string_to_float_validate(s), 0);
            let p = sigil_string_to_float_parse(s);
            assert!((read(p) - 2.718).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn validate_rejects_non_numeric() {
        let _g = gc_test_lock();
        unsafe {
            let s = crate::gc::sigil_string_new(b"abc".as_ptr(), 3);
            assert_ne!(sigil_string_to_float_validate(s), 0);
        }
    }

    #[test]
    fn validate_rejects_empty() {
        let _g = gc_test_lock();
        unsafe {
            let s = crate::gc::sigil_string_new(b"".as_ptr(), 0);
            assert_ne!(sigil_string_to_float_validate(s), 0);
        }
    }

    #[test]
    fn counter_increments_on_alloc() {
        let _g = gc_test_lock();
        let before = counters::read(CounterId::FloatAllocCount);
        let _ = boxf(1.0);
        let after = counters::read(CounterId::FloatAllocCount);
        assert!(after > before);
    }
}
