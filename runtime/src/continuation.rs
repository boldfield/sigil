//! Plan D Task 117 (b) Phase 4 — Continuation value object.
//!
//! Boxes the captured-continuation pair AND the originating handle's
//! return-arm pair so that a `Continuation[op_ret, ret]` value flows
//! through the Sync user-fn ABI as a single pointer. The return-arm
//! pair is necessary because invoking the continuation outside the
//! originating arm body (e.g., from a recursive helper like
//! `fold_choices`) must still wrap the body-returned value via the
//! handle's `return(v) => ...` arm — the trampoline can't infer this
//! wrap from anywhere else once the captured continuation reaches a
//! foreign call frame.
//!
//! Layout (40 bytes):
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_CONTINUATION, count=4,
//!                            bitmap=0b0101)
//!                              ^^^^^^^^^^^^
//!                              bit 0: k_closure_ptr      (GC managed)
//!                              bit 1: k_fn_ptr           (code addr)
//!                              bit 2: return_closure_ptr (GC managed)
//!                              bit 3: return_fn_ptr      (code addr)
//! offset 8  : k_closure_ptr      (resume closure record)
//! offset 16 : k_fn_ptr           (resume code address)
//! offset 24 : return_closure_ptr (handle's return-arm closure;
//!                                 null when no captures / no arm)
//! offset 32 : return_fn_ptr      (handle's return-arm fn; null
//!                                 falls back to identity at invoke)
//! ```
//!
//! Allocated by [`sigil_continuation_alloc`] at the call site that
//! flows a continuation into a fn-parameter. Inside the receiving fn,
//! `k(arg)` derefs offsets 8/16/24/32, builds `NextStep::Call(
//! k_closure, k_fn, [arg, return_closure, return_fn])`, and drives
//! `sigil_run_loop` to the wrapped terminal value.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_CONTINUATION};

/// Allocate a fresh Continuation value object holding the given
/// `(k_closure, k_fn, return_closure, return_fn)` quadruple. Returns
/// the header pointer (a single GC-managed pointer the caller treats
/// as the Sigil-level `Continuation` value).
///
/// `return_closure` / `return_fn` may be null — when null, the
/// invoke side substitutes `sigil_continuation_identity` as the
/// trailing-pair fn (mirrors the pre-Phase-4 behavior of identity
/// returning `Done(arg)` for handles without a return arm).
///
/// # Safety
///
/// All four pointers must be valid for their respective slots.
/// `k_closure` / `return_closure` must be GC-allocated closure-record
/// headers (or null); `k_fn` / `return_fn` must be valid function
/// pointers to Cps arm-fn-ABI code (or null). The four pointers as a
/// group must logically belong to a single live handler arm — if the
/// caller flows a stale set into this allocator, the resulting
/// Continuation value dispatches to a dead frame at invoke time and
/// trips the runtime ScopeId check.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_alloc(
    k_closure: *mut u8,
    k_fn: *mut u8,
    return_closure: *mut u8,
    return_fn: *mut u8,
) -> *mut u8 {
    // bitmap = 0b0101 — bits 0 and 2 cover fields 0 and 2 (closure
    // pointers, GC-managed); bits 1 and 3 (fn pointers) are code
    // addresses, not heap pointers.
    let h = Header::new(TAG_CONTINUATION, 4, 0b0101);
    let obj = sigil_alloc(h.raw(), 32);
    // SAFETY: gc-heap-ptr arithmetic (transient base for four aligned
    // pointer stores).
    let p_k_closure: *mut *mut u8 = obj.add(8).cast();
    p_k_closure.write(k_closure);
    let p_k_fn: *mut *mut u8 = obj.add(16).cast();
    p_k_fn.write(k_fn);
    let p_ret_closure: *mut *mut u8 = obj.add(24).cast();
    p_ret_closure.write(return_closure);
    let p_ret_fn: *mut *mut u8 = obj.add(32).cast();
    p_ret_fn.write(return_fn);
    counters::incr(CounterId::ContinuationAllocCount);
    counters::add(CounterId::ContinuationAllocBytes, 40);
    obj
}

/// Read the `k_closure_ptr` field of a Continuation value.
///
/// # Safety
///
/// `cont` must be a pointer to a valid `TAG_CONTINUATION` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_load_closure(cont: *const u8) -> *mut u8 {
    let p: *const *mut u8 = cont.add(8).cast();
    p.read()
}

/// Read the `k_fn_ptr` field of a Continuation value.
///
/// # Safety
///
/// `cont` must be a pointer to a valid `TAG_CONTINUATION` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_load_fn(cont: *const u8) -> *mut u8 {
    let p: *const *mut u8 = cont.add(16).cast();
    p.read()
}

/// Read the `return_closure_ptr` field of a Continuation value.
///
/// # Safety
///
/// `cont` must be a pointer to a valid `TAG_CONTINUATION` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_load_return_closure(cont: *const u8) -> *mut u8 {
    let p: *const *mut u8 = cont.add(24).cast();
    p.read()
}

/// Read the `return_fn_ptr` field of a Continuation value.
///
/// # Safety
///
/// `cont` must be a pointer to a valid `TAG_CONTINUATION` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_load_return_fn(cont: *const u8) -> *mut u8 {
    let p: *const *mut u8 = cont.add(32).cast();
    p.read()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    #[test]
    fn alloc_round_trips_quadruple() {
        let _g = gc_test_lock();
        let k_closure = 0xDEAD_BEEF_DEAD_BEEFu64 as *mut u8;
        let k_fn = 0xCAFE_BABE_CAFE_BABEu64 as *mut u8;
        let ret_closure = 0xBABE_FACE_BABE_FACEu64 as *mut u8;
        let ret_fn = 0xFEED_F00D_FEED_F00Du64 as *mut u8;
        unsafe {
            let cont = sigil_continuation_alloc(k_closure, k_fn, ret_closure, ret_fn);
            assert!(!cont.is_null(), "alloc returned null");
            assert_eq!(sigil_continuation_load_closure(cont), k_closure);
            assert_eq!(sigil_continuation_load_fn(cont), k_fn);
            assert_eq!(sigil_continuation_load_return_closure(cont), ret_closure);
            assert_eq!(sigil_continuation_load_return_fn(cont), ret_fn);
        }
    }

    #[test]
    fn header_layout_matches_constants() {
        let _g = gc_test_lock();
        unsafe {
            let cont = sigil_continuation_alloc(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            let header_word: *const u64 = cont.cast();
            let h = Header(header_word.read());
            assert_eq!(h.type_tag(), TAG_CONTINUATION, "tag must be TAG_CONTINUATION");
            assert_eq!(h.payload_count(), 4, "count must be 4");
            assert_eq!(
                h.pointer_bitmap(),
                0b0101,
                "bitmap: bits 0+2 set (k_closure / return_closure GC), bits 1+3 clear (k_fn / return_fn code)"
            );
        }
    }

    #[test]
    fn null_quadruple_round_trips() {
        let _g = gc_test_lock();
        unsafe {
            let cont = sigil_continuation_alloc(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert!(sigil_continuation_load_closure(cont).is_null());
            assert!(sigil_continuation_load_fn(cont).is_null());
            assert!(sigil_continuation_load_return_closure(cont).is_null());
            assert!(sigil_continuation_load_return_fn(cont).is_null());
        }
    }
}
