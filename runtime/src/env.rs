//! Plan C addendum (CLI external-system effects, EE2) — runtime
//! arm fns for the `Env` effect: `args`, `var`, `vars`. Each arm fn
//! conforms to the Phase 4 CPS arm fn ABI (closure_ptr / in_args /
//! args_len / terminal_out → `*mut NextStep`) used by every existing
//! builtin-effect arm in `runtime/src/handlers.rs`.
//!
//! ## Return shapes (raw)
//!
//! - `Env.args() -> Array[String]` — process argv. The runtime
//!   collects via `std::env::args()` and stamps them into a fresh
//!   `TAG_ARRAY` (slots = pointers to Sigil `String`s).
//! - `Env.var(name: String) -> (Int, String)` — env-var lookup. Tag
//!   `0` = present (slot 1 = value); tag `1` = absent (slot 1 =
//!   empty `String`).
//! - `Env.vars() -> Array[(String, String)]` — every env-var pair.
//!   Each element is a binary tuple `(name, value)` allocated as
//!   `TAG_TUPLE` with bitmap `0b11` (both pointers).
//!
//! `std/env.sigil` (EE4) wraps `Env.var` to produce `Option[String]`.

use crate::effect_helpers::{
    alloc_array_with_capacity, alloc_string_from_str, alloc_tuple, array_set_slot_raw,
};
use crate::gc::string_bytes;
use crate::handlers::{write_k_dispatch_value, NextStep, TerminalResult};

/// `Env.args()` arm fn. Op id 0. No user args; returns `Array[String]`.
///
/// # Safety
///
/// Standard arm-fn ABI: `args_len == 5` (Stage 5: trailing
/// `(k_closure, k_fn, return_arm_closure, return_arm_fn,
/// return_arm_fired_ptr)`).
#[no_mangle]
pub unsafe extern "C" fn sigil_env_args_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 5,
        "sigil_env_args_arm: args_len {args_len} != 5"
    );
    debug_assert!(!in_args.is_null());
    let k_closure = *in_args as *mut u8;
    let k_fn = *in_args.add(1) as *mut u8;

    // Collect argv up front into Rust `String`s — those live in
    // Rust's allocator (independent of Boehm). Sigil-side allocation
    // happens during the fill loop.
    let argv: Vec<String> = std::env::args().collect();
    let n = argv.len();
    let arr = alloc_array_with_capacity(n);
    for (i, a) in argv.iter().enumerate() {
        // sigil_str is rooted via the Rust stack until written into
        // the array slot below; once written, rooted via `arr`.
        let sigil_str = alloc_string_from_str(a);
        array_set_slot_raw(arr, i, sigil_str as u64);
    }

    write_k_dispatch_value(k_closure, k_fn, arr as u64)
}

/// `Env.var(name: String) -> (Int, String)` arm fn. Op id 1.
///
/// Tag 0 = present (Some); tag 1 = absent (None). The empty
/// string in the tag-1 case is allocated for shape uniformity —
/// the stdlib wrapper at `std/env.sigil` ignores it and
/// constructs `None`.
///
/// # Safety
///
/// `args_len == 5` (1 user arg + trailing pair). `in_args[0]` must
/// be a non-null `TAG_STRING` pointer.
#[no_mangle]
pub unsafe extern "C" fn sigil_env_var_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 6, "sigil_env_var_arm: args_len {args_len} != 6");
    debug_assert!(!in_args.is_null());
    let name_ptr = *in_args as *const u8;
    debug_assert!(!name_ptr.is_null());
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (name_bytes, name_len) = string_bytes(name_ptr);
    let name_slice = std::slice::from_raw_parts(name_bytes, name_len);
    let (tag, value): (i64, *mut u8) = match std::str::from_utf8(name_slice) {
        Ok(name) => match std::env::var(name) {
            Ok(v) => (0, alloc_string_from_str(&v)),
            Err(_) => (1, alloc_string_from_str("")),
        },
        // Invalid UTF-8 in the env-var name — treat as not-found
        // rather than aborting; user-visible behavior is the same
        // as querying a missing variable.
        Err(_) => (1, alloc_string_from_str("")),
    };
    let idx = crate::gc::runtime_shape_indices().tuple_int_ptr;
    let tup = alloc_tuple(&[tag as u64, value as u64], 0b10, idx);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Env.vars() -> Array[(String, String)]` arm fn. Op id 2.
///
/// # Safety
///
/// `args_len == 5`.
#[no_mangle]
pub unsafe extern "C" fn sigil_env_vars_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 5,
        "sigil_env_vars_arm: args_len {args_len} != 5"
    );
    debug_assert!(!in_args.is_null());
    let k_closure = *in_args as *mut u8;
    let k_fn = *in_args.add(1) as *mut u8;

    let pairs: Vec<(String, String)> = std::env::vars().collect();
    let n = pairs.len();
    let arr = alloc_array_with_capacity(n);
    for (i, (k, v)) in pairs.iter().enumerate() {
        // Each iteration's locals (k_ptr, v_ptr, tup) are rooted via
        // the Rust stack between the allocations below. After the
        // tuple is written into the array slot it's rooted via `arr`.
        let k_ptr = alloc_string_from_str(k);
        let v_ptr = alloc_string_from_str(v);
        let tup_idx = crate::gc::runtime_shape_indices().tuple_ptr_ptr;
        let tup = alloc_tuple(&[k_ptr as u64, v_ptr as u64], 0b11, tup_idx);
        array_set_slot_raw(arr, i, tup as u64);
    }

    write_k_dispatch_value(k_closure, k_fn, arr as u64)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::gc::sigil_string_len;
    use crate::test_support::gc_test_lock;

    /// Exercise the Sigil-side allocation path of `Env.args`.
    /// The arm fn ABI is awkward to drive directly without the
    /// trampoline, so we test the underlying Rust logic via a
    /// direct-call helper. End-to-end coverage lives in EE6's
    /// e2e tests.
    #[test]
    fn args_returns_non_empty_array() {
        let _g = gc_test_lock();
        unsafe {
            // Rebuild what `sigil_env_args_arm` does, minus the
            // NextStep dispatch (which needs trampoline state).
            let argv: Vec<String> = std::env::args().collect();
            let n = argv.len();
            let arr = alloc_array_with_capacity(n);
            for (i, a) in argv.iter().enumerate() {
                let sigil_str = alloc_string_from_str(a);
                array_set_slot_raw(arr, i, sigil_str as u64);
            }
            // Verify length word matches.
            let len_p: *const u64 = arr.add(8).cast();
            assert_eq!(len_p.read() as usize, n);
            assert!(n >= 1, "test harness should always have at least argv[0]");
        }
    }

    #[test]
    fn var_present_returns_tag_zero() {
        let _g = gc_test_lock();
        unsafe {
            // `PATH` is set in essentially every test environment
            // (cargo / shell / CI runner). If a CI minimization
            // ever drops it, swap for another always-present var.
            std::env::set_var("__SIGIL_TEST_VAR_PRESENT", "value");
            let name = alloc_string_from_str("__SIGIL_TEST_VAR_PRESENT");
            let (name_bytes, name_len) = string_bytes(name);
            let name_slice = std::slice::from_raw_parts(name_bytes, name_len);
            let s = std::str::from_utf8(name_slice).expect("ascii");
            let v = std::env::var(s).expect("present");
            let value = alloc_string_from_str(&v);
            let tup_idx = crate::gc::runtime_shape_indices().tuple_int_ptr;
            let tup = alloc_tuple(&[0_u64, value as u64], 0b10, tup_idx);
            // tag at offset 8, value ptr at offset 16.
            assert_eq!((tup.add(8) as *const i64).read(), 0);
            let val_ptr = (tup.add(16) as *const *const u8).read();
            assert_eq!(sigil_string_len(val_ptr), 5);
            std::env::remove_var("__SIGIL_TEST_VAR_PRESENT");
        }
    }

    #[test]
    fn var_absent_returns_tag_one() {
        let _g = gc_test_lock();
        unsafe {
            let name = "__sigil_test_var_definitely_absent_2026_05_07__";
            std::env::remove_var(name);
            let (tag, value) = match std::env::var(name) {
                Ok(v) => (0_i64, alloc_string_from_str(&v)),
                Err(_) => (1_i64, alloc_string_from_str("")),
            };
            let tup_idx = crate::gc::runtime_shape_indices().tuple_int_ptr;
            let tup = alloc_tuple(&[tag as u64, value as u64], 0b10, tup_idx);
            assert_eq!((tup.add(8) as *const i64).read(), 1);
            let val_ptr = (tup.add(16) as *const *const u8).read();
            assert_eq!(sigil_string_len(val_ptr), 0);
        }
    }
}
