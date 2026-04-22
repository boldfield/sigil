//! Tagged value representation — plan A1 Stage 1 task 2.
//!
//! Sigil `Value` is a single `u64`, tagged by its low bits:
//!
//! - `...xxx0` — 63-bit signed integer (`Int`), range `[-2^62, 2^62)`.
//!   Overflow wraps two's-complement in the 63-bit range. The raw bit
//!   representation is `(signed_value as u64) << 1` so the low bit is 0.
//! - `...xx01` — heap pointer. The pointer itself is 8-byte aligned (Boehm
//!   returns at least 8-byte-aligned blocks), so the low 3 bits of the
//!   raw pointer are zero. We set bits `0..2 = 01` to tag heap values.
//! - `...xx11` — immediate (reserved for v1's `Bool`, `Unit`, small `Char`).
//!
//! Plan A1's vertical slice uses only `Int` (for `main`'s return value)
//! and heap pointers (for `String`). Immediates are declared but unused.
//!
//! The layout is defined *here* and nowhere else. Any code that wants to
//! interpret a `Value` must go through these constants and accessors.

/// Tagged 64-bit value.
pub type Value = u64;

/// Bitmask isolating the low tag bit (Int vs non-Int).
pub const TAG_INT_MASK: u64 = 0b1;

/// Bitmask isolating the low two tag bits (heap vs immediate when non-Int).
pub const TAG_WIDE_MASK: u64 = 0b11;

/// Tag bits for a heap pointer. `(ptr & !TAG_WIDE_MASK)` recovers the raw
/// pointer; `raw_ptr | TAG_HEAP` tags it.
pub const TAG_HEAP: u64 = 0b01;

/// Tag bits for immediates.
pub const TAG_IMMEDIATE: u64 = 0b11;

/// Lower / upper inclusive range of Sigil's 63-bit `Int` type.
pub const INT_MIN: i64 = -(1 << 62);
pub const INT_MAX: i64 = (1 << 62) - 1;

#[inline]
pub fn is_int(v: Value) -> bool {
    (v & TAG_INT_MASK) == 0
}

#[inline]
pub fn is_heap(v: Value) -> bool {
    (v & TAG_WIDE_MASK) == TAG_HEAP
}

#[inline]
pub fn is_immediate(v: Value) -> bool {
    (v & TAG_WIDE_MASK) == TAG_IMMEDIATE
}

/// Construct an `Int` value. Input outside the 63-bit range wraps per the
/// design's two's-complement semantics.
#[inline]
pub fn from_int(n: i64) -> Value {
    // Canonical encoding: shift left by one, low bit becomes the Int tag (0).
    // `wrapping_shl` gives two's-complement wraparound at 64 bits, matching
    // the design's overflow-wraps semantics at the 63-bit boundary.
    (n as u64).wrapping_shl(1)
}

/// Sign-extend the high 63 bits of the tagged int back to a full `i64`.
/// Callers must check `is_int` first; passing a non-Int is undefined in
/// the sense that it will return an arbitrary integer reinterpretation.
#[inline]
pub fn as_int(v: Value) -> i64 {
    (v as i64) >> 1
}

/// Tag a raw pointer as a heap Value. The pointer must be ≥8-byte aligned
/// (guaranteed for all `sigil_alloc` returns).
#[inline]
pub fn from_heap(ptr: *mut u8) -> Value {
    (ptr as u64) | TAG_HEAP
}

/// Recover the raw pointer from a heap Value. Callers must check `is_heap`.
#[inline]
pub fn as_heap_ptr(v: Value) -> *mut u8 {
    (v & !TAG_WIDE_MASK) as *mut u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_round_trip_zero() {
        assert_eq!(as_int(from_int(0)), 0);
        assert!(is_int(from_int(0)));
        assert!(!is_heap(from_int(0)));
    }

    #[test]
    fn int_round_trip_positive() {
        for n in [1i64, 2, 42, 1 << 60, INT_MAX] {
            let v = from_int(n);
            assert!(is_int(v), "expected Int for {n}");
            assert_eq!(as_int(v), n, "round trip failed for {n}");
        }
    }

    #[test]
    fn int_round_trip_negative() {
        for n in [-1i64, -2, -42, -(1 << 60), INT_MIN] {
            let v = from_int(n);
            assert!(is_int(v), "expected Int for {n}");
            assert_eq!(as_int(v), n, "round trip failed for {n}");
        }
    }

    #[test]
    fn heap_tag_round_trip() {
        // Use a fake 8-byte-aligned pointer value so the test doesn't need
        // a real allocation.
        let fake: *mut u8 = 0x1000 as *mut u8;
        let v = from_heap(fake);
        assert!(is_heap(v));
        assert!(!is_int(v));
        assert_eq!(as_heap_ptr(v), fake);
    }

    #[test]
    fn heap_and_int_are_disjoint() {
        let iv = from_int(7);
        let hv = from_heap(0x1000 as *mut u8);
        assert!(is_int(iv) && !is_heap(iv));
        assert!(is_heap(hv) && !is_int(hv));
    }
}
