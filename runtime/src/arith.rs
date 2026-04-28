//! Arithmetic runtime primitives — plan A2 task 25, Plan B Task 57.
//!
//! Plan A2's `sigil_panic_arith_error` was retired in Plan B Task 57.
//! Codegen no longer emits a branch to a panic-by-stderr-message
//! function on `sdiv` / `srem` sites with a zero divisor; instead,
//! `BinOp::Div` / `BinOp::Mod` elaborate to a perform-bearing form
//! (`if rhs == 0 { perform ArithError.{div,mod}_by_zero() } else
//! { … }`), and the perform routes through `sigil_perform` to the
//! top-level ArithError handler installed by the `main` shim. The
//! default arm fns (`sigil_arith_error_div_by_zero_arm` /
//! `sigil_arith_error_mod_by_zero_arm` in `runtime/src/handlers.rs`)
//! preserve Plan A2's exact stderr banner + exit-2 user-visible
//! behavior; user programs that wrap arithmetic in
//! `handle ... with { ArithError.div_by_zero(k) => ... }` can now
//! recover from div-by-zero rather than aborting. See
//! `[DEVIATION Task 57] BinOp::Div and BinOp::Mod elaborate to
//! perform-bearing form` and `[DEVIATION Task 57] Top-level handler
//! installation in main shim` in
//! `boldfield/designs/PLAN_B_DEVIATIONS.md`.
//!
//! Surface this module still owns:
//!
//! 1. **`sigil_int_to_string`.** Formats an `Int` as a heap-allocated
//!    `String`. Exposed to the language as `int_to_string(n: Int) ->
//!    String`. Unchanged by Task 57.
//!
//! 2. **Checked-overflow primitives.** `sigil_checked_add`,
//!    `sigil_checked_sub`, `sigil_checked_mul` return `(result,
//!    overflowed)` for a future `checked_add(a: Int, b: Int) ->
//!    Option[Int]` wrapper. Unchanged by Task 57.

use crate::gc::sigil_string_new;

/// Format an `Int` as a decimal Sigil `String` and return a heap-header
/// pointer suitable for tagging via `value::from_heap`. The returned
/// pointer is an 8-byte-aligned heap object allocated from Boehm.
///
/// Equivalent to `sigil_string_new(n.to_string().as_bytes(), len)`
/// internally — the formatting buffer is a stack-local Rust `String`
/// that is dropped before the function returns.
#[no_mangle]
pub extern "C" fn sigil_int_to_string(n: i64) -> *mut u8 {
    let formatted = n.to_string();
    let bytes = formatted.as_bytes();
    // SAFETY: not an interior pointer (`formatted` is a Rust `String` on the system allocator, not Boehm-managed; copied into a fresh Boehm allocation before the call returns).
    unsafe { sigil_string_new(bytes.as_ptr(), bytes.len()) }
}

/// Result of a checked-overflow arithmetic primitive. Returned by
/// `sigil_checked_add`/`sub`/`mul`.
///
/// Layout is `#[repr(C)]`: `i64` payload followed by a `bool` flag
/// padded to the struct's natural alignment. Every supported host's
/// `extern "C"` calling convention returns this pair in two registers
/// (x86-64 System-V and AArch64 AAPCS64 both do; WebAssembly is
/// out-of-scope for v1). Cranelift's default System-V return convention
/// honours the same layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedInt {
    pub value: i64,
    pub overflowed: bool,
}

/// Checked `a + b`. On overflow, `overflowed = true` and `value` is the
/// two's-complement wrapped result (matching `i64::wrapping_add`). The
/// language-level `Option[Int]` wrapper arrives with sum types in plan
/// A3.
#[no_mangle]
pub extern "C" fn sigil_checked_add(a: i64, b: i64) -> CheckedInt {
    match a.checked_add(b) {
        Some(value) => CheckedInt {
            value,
            overflowed: false,
        },
        None => CheckedInt {
            value: a.wrapping_add(b),
            overflowed: true,
        },
    }
}

/// Checked `a - b`. See `sigil_checked_add`.
#[no_mangle]
pub extern "C" fn sigil_checked_sub(a: i64, b: i64) -> CheckedInt {
    match a.checked_sub(b) {
        Some(value) => CheckedInt {
            value,
            overflowed: false,
        },
        None => CheckedInt {
            value: a.wrapping_sub(b),
            overflowed: true,
        },
    }
}

/// Checked `a * b`. See `sigil_checked_add`.
#[no_mangle]
pub extern "C" fn sigil_checked_mul(a: i64, b: i64) -> CheckedInt {
    match a.checked_mul(b) {
        Some(value) => CheckedInt {
            value,
            overflowed: false,
        },
        None => CheckedInt {
            value: a.wrapping_mul(b),
            overflowed: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{sigil_gc_init, sigil_string_len};

    /// Read the contents of a Sigil string object into an owned `Vec<u8>`
    /// while keeping `obj` alive through a `black_box` sink. Boehm is a
    /// non-moving *but* conservative collector: if the optimiser drops
    /// `obj` after forming the interior pointer, a concurrent test
    /// thread's alloc can trigger a mark phase that collects the very
    /// object we are reading from, and another allocation can then
    /// reuse the slot before we finish the read. The `black_box(obj)`
    /// call after `to_vec()` pins the pointer on the stack until the
    /// copy is complete, dodging the interior-pointer race entirely.
    ///
    /// This helper is the safe pattern every arith/gc test that reads
    /// a string payload back out should use under parallel `cargo test`.
    fn read_string_bytes(obj: *mut u8) -> Vec<u8> {
        // SAFETY: callers pass a live `sigil_string_new` result; the
        // helper is the only interior-pointer site and it immediately
        // copies out.
        let (ptr, len) = unsafe { crate::gc::string_bytes(obj) };
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let owned = slice.to_vec();
        // Pin `obj` on the stack until the copy is complete. Without
        // this, the optimiser is free to drop the stack slot after
        // `ptr` is computed, making the slot's memory eligible for GC
        // reuse by a concurrent test thread.
        std::hint::black_box(obj);
        owned
    }

    #[test]
    fn int_to_string_positive() {
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let obj = sigil_int_to_string(42);
        assert!(!obj.is_null());
        let len = unsafe { sigil_string_len(obj) };
        assert_eq!(len, 2);
        let bytes = read_string_bytes(obj);
        assert_eq!(bytes, b"42");
    }

    #[test]
    fn int_to_string_negative() {
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        let obj = sigil_int_to_string(-7);
        let len = unsafe { sigil_string_len(obj) };
        assert_eq!(len, 2);
        let bytes = read_string_bytes(obj);
        assert_eq!(bytes, b"-7");
    }

    #[test]
    fn int_to_string_zero_and_extremes() {
        let _guard = crate::test_support::gc_test_lock();
        sigil_gc_init();
        for (n, expected) in [
            (0i64, "0"),
            (i64::MAX, "9223372036854775807"),
            (i64::MIN, "-9223372036854775808"),
        ] {
            let obj = sigil_int_to_string(n);
            let len = unsafe { sigil_string_len(obj) };
            assert_eq!(len, expected.len(), "length for {n}");
            let bytes = read_string_bytes(obj);
            assert_eq!(bytes, expected.as_bytes(), "bytes for {n}");
        }
    }

    #[test]
    fn checked_add_no_overflow() {
        assert_eq!(
            sigil_checked_add(2, 3),
            CheckedInt {
                value: 5,
                overflowed: false
            }
        );
        assert_eq!(
            sigil_checked_add(-10, 4),
            CheckedInt {
                value: -6,
                overflowed: false
            }
        );
    }

    #[test]
    fn checked_add_overflow_wraps() {
        let r = sigil_checked_add(i64::MAX, 1);
        assert!(r.overflowed);
        assert_eq!(r.value, i64::MAX.wrapping_add(1));
    }

    #[test]
    fn checked_sub_overflow_wraps() {
        let r = sigil_checked_sub(i64::MIN, 1);
        assert!(r.overflowed);
        assert_eq!(r.value, i64::MIN.wrapping_sub(1));
    }

    #[test]
    fn checked_mul_overflow_wraps() {
        let r = sigil_checked_mul(i64::MAX, 2);
        assert!(r.overflowed);
        assert_eq!(r.value, i64::MAX.wrapping_mul(2));
    }

    #[test]
    fn checked_mul_no_overflow() {
        assert_eq!(
            sigil_checked_mul(6, 7),
            CheckedInt {
                value: 42,
                overflowed: false
            }
        );
    }

    // Plan A2's `sigil_panic_arith_error` was retired in Plan B
    // Task 57; the e2e tests `div_by_zero.sigil` and the inline
    // `mod_by_zero_traps` test now exercise the runtime-side arm
    // fns `sigil_arith_error_div_by_zero_arm` and `sigil_arith_-
    // error_mod_by_zero_arm` (defined in `runtime/src/handlers.rs`),
    // which preserve the Plan A2 stderr banner + exit-2 behaviour.
    // Direct unit tests for the arm fns would still abort the test
    // runner; the e2e suite is the authoritative cross-check.
}
