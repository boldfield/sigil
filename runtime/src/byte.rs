//! `Byte` runtime primitives — plan A2 task 25.
//!
//! Stage 2 introduces the `Byte` type (unsigned 8-bit, range `[0, 256)`)
//! to the type surface but does not expose any Byte literal syntax.
//! Values are constructed exclusively via these runtime primitives,
//! which the compiler will call at any codegen site that needs a Byte
//! value once the language surface grows call support (plan A2 task 29
//! introduces user function calls; plan A3 sum types land the
//! `Option[Byte]`-returning wrapper).
//!
//! Scope at task 25: the C ABI symbols exist and pass their unit
//! tests. Language-level exposure follows.
//!
//! # The `ByteFromInt` return type
//!
//! `sigil_byte_from_int_checked` has to return both a `u8` payload and
//! a `bool` in-range flag. A `#[repr(C)]` struct is used for the same
//! reason as `CheckedInt` in `arith.rs`: every supported host's
//! `extern "C"` calling convention returns a two-field struct in
//! registers, and Cranelift's default System-V return lowering
//! honours the layout. The in-range `bool` is emitted by the compiler
//! as a normal `Bool` value once Plan A3's sum-type wrapper
//! `Option[Byte]` is in place.

/// Result of `sigil_byte_from_int_checked`. See module doc for ABI
/// rationale.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteFromInt {
    pub value: u8,
    pub in_range: bool,
}

/// Checked `Int → Byte` conversion. Returns `in_range = true` when `n`
/// is representable as a `Byte` (`0 ≤ n < 256`); otherwise
/// `in_range = false` and `value = 0`.
#[no_mangle]
pub extern "C" fn sigil_byte_from_int_checked(n: i64) -> ByteFromInt {
    if (0..256).contains(&n) {
        ByteFromInt {
            value: n as u8,
            in_range: true,
        }
    } else {
        ByteFromInt {
            value: 0,
            in_range: false,
        }
    }
}

/// `Byte → Int` widening. Always succeeds; there is no failure mode
/// because every `Byte` value fits in `[0, 255]` ⊂ `Int`'s 63-bit
/// range.
#[no_mangle]
pub extern "C" fn sigil_byte_to_int(b: u8) -> i64 {
    b as i64
}

/// Wrapping `Byte + Byte`. Overflow wraps modulo 256 per two's
/// complement on `u8`, matching the design's "arithmetic on Byte is
/// wrapping, no overflow reporting" contract.
#[no_mangle]
pub extern "C" fn sigil_byte_add(a: u8, b: u8) -> u8 {
    a.wrapping_add(b)
}

/// Wrapping `Byte - Byte`. See `sigil_byte_add`.
#[no_mangle]
pub extern "C" fn sigil_byte_sub(a: u8, b: u8) -> u8 {
    a.wrapping_sub(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_from_int_in_range() {
        for n in [0i64, 1, 127, 128, 255] {
            let r = sigil_byte_from_int_checked(n);
            assert!(r.in_range, "expected in-range for {n}");
            assert_eq!(r.value, n as u8);
        }
    }

    #[test]
    fn byte_from_int_out_of_range_negative() {
        for n in [-1i64, -256, i64::MIN] {
            let r = sigil_byte_from_int_checked(n);
            assert!(!r.in_range, "expected out-of-range for {n}");
            assert_eq!(r.value, 0);
        }
    }

    #[test]
    fn byte_from_int_out_of_range_above() {
        for n in [256i64, 1000, i64::MAX] {
            let r = sigil_byte_from_int_checked(n);
            assert!(!r.in_range, "expected out-of-range for {n}");
            assert_eq!(r.value, 0);
        }
    }

    #[test]
    fn byte_to_int_round_trips() {
        for b in 0u8..=255 {
            let n = sigil_byte_to_int(b);
            assert_eq!(n, b as i64);
            let back = sigil_byte_from_int_checked(n);
            assert!(back.in_range);
            assert_eq!(back.value, b);
        }
    }

    #[test]
    fn byte_add_wraps() {
        assert_eq!(sigil_byte_add(1, 2), 3);
        assert_eq!(sigil_byte_add(100, 200), 44); // 300 mod 256 = 44
        assert_eq!(sigil_byte_add(255, 1), 0);
        assert_eq!(sigil_byte_add(255, 255), 254);
    }

    #[test]
    fn byte_sub_wraps() {
        assert_eq!(sigil_byte_sub(5, 3), 2);
        assert_eq!(sigil_byte_sub(0, 1), 255);
        assert_eq!(sigil_byte_sub(10, 255), 11); // (10 - 255) mod 256 = 11
    }
}
