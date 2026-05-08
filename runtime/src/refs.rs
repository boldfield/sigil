//! `Ref[T]` heap cells — single-slot mutable storage backing
//! `std/state.sigil`'s runtime-cell-backed State implementation.
//!
//! # Why this exists
//!
//! Sigil v1's prior State implementation used Plotkin's lambda-encoding
//! (each State arm body returned `fn(s: S) -> (A, S)` — a closure
//! capturing the continuation `k`). The encoding could not compose with
//! discharging effects like `Raise`: the lifted state-fn lambda is Sync
//! per `compute_user_fn_abi`, and the Sync ABI cannot carry a
//! `DISCHARGED` tag through fn returns. A foreign discharge from inside
//! `k(s)` would silently lose the tag, leaving `state-fn(initial)` to
//! return the discharged value typed-as-tuple → SIGSEGV at the caller's
//! destructure.
//!
//! Replacing the encoding with mutable cells avoids the ABI gap
//! entirely: arm bodies become `k(deref(cell))` / `{ ref_set(cell, v); k(()) }`,
//! the existing `lower_k_pair_call` infrastructure handles foreign
//! discharge correctly, and the State surface (`perform State.get/set`,
//! `run_state`) is unchanged.
//!
//! # Layout
//!
//! ```text
//! offset 0 : 8-byte header (tag = TAG_REF, count = 1, bitmap = 0b1)
//! offset 8 : u64 value slot
//! ```
//!
//! Bitmap bit 0 is set: Boehm's conservative scan follows the value slot
//! as a possible pointer. This is essential when the cell holds a heap
//! pointer (e.g., `Ref[String]` storing a string pointer). For tagged-Int
//! values (low bit = 1) and Unit (i64 0), the conservative scan tolerates
//! the non-pointer bit pattern by simply not following it.
//!
//! # v1 access discipline
//!
//! These three ops are gated by the typechecker to calls from inside
//! `std/state.sigil` only. User code cannot allocate `Ref[T]` directly.
//! `Ref[T]` is therefore not exposed as a v1 user-facing type — it's an
//! internal representation detail of `run_state`'s cell-backed encoding.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_REF};

/// Cell payload size: one u64 value slot.
const REF_PAYLOAD_BYTES: usize = 8;

/// Allocate a fresh `Ref[T]` cell holding `initial`. Returns the cell
/// pointer (a `TAG_REF`-headered, GC-managed allocation).
///
/// # ABI
///
/// Standard Sync calling convention: no closure_ptr, no `terminal_out`,
/// returns the cell pointer directly. Compiler lowers calls to
/// `sigil_ref_alloc(initial)` as a direct extern-C call to this symbol
/// with the value widened to `u64`.
///
/// # Safety
///
/// Safe to call. The returned pointer is GC-rooted only by whoever holds
/// it after this returns; once dropped, the cell becomes unreachable and
/// is collected on the next GC cycle.
#[no_mangle]
pub extern "C" fn sigil_ref_alloc(initial: u64) -> *mut u8 {
    let h = Header::new(TAG_REF, 1, 0b1);
    let obj = sigil_alloc(h.raw(), REF_PAYLOAD_BYTES);
    // SAFETY: gc-heap-ptr arithmetic — `obj` is a freshly-allocated
    // 16-byte block (8-byte header + 8-byte value slot). Writing the
    // value slot at offset 8 is an aligned u64 store within the
    // allocation.
    unsafe {
        let value_slot: *mut u64 = obj.add(8).cast();
        value_slot.write(initial);
    }
    counters::incr(CounterId::BoehmAllocCount);
    obj
}

/// Read the current value stored in `cell`.
///
/// # Safety
///
/// `cell` must be a non-null pointer previously returned by
/// `sigil_ref_alloc`. The cell must still be live (i.e., reachable from
/// some GC root), which is the caller's responsibility.
#[no_mangle]
pub unsafe extern "C" fn sigil_ref_deref(cell: *mut u8) -> u64 {
    debug_assert!(!cell.is_null(), "sigil_ref_deref: null cell");
    // SAFETY: gc-heap-ptr arithmetic — cell is a `TAG_REF` allocation,
    // value slot at offset 8 is an aligned u64.
    let value_slot: *const u64 = cell.add(8).cast();
    value_slot.read()
}

/// Overwrite `cell`'s value slot with `value`. Returns Unit (`i64 0`).
///
/// # Safety
///
/// `cell` must be a non-null pointer previously returned by
/// `sigil_ref_alloc`. The cell must still be live.
#[no_mangle]
pub unsafe extern "C" fn sigil_ref_set(cell: *mut u8, value: u64) -> u64 {
    debug_assert!(!cell.is_null(), "sigil_ref_set: null cell");
    // SAFETY: same argument as sigil_ref_deref.
    let value_slot: *mut u64 = cell.add(8).cast();
    value_slot.write(value);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    #[test]
    fn cell_round_trip_int_value() {
        let _g = gc_test_lock();
        let cell = sigil_ref_alloc(42);
        assert!(!cell.is_null());
        // SAFETY: cell is just-allocated, still live.
        let v = unsafe { sigil_ref_deref(cell) };
        assert_eq!(v, 42);

        // SAFETY: same.
        let unit = unsafe { sigil_ref_set(cell, 99) };
        assert_eq!(unit, 0, "sigil_ref_set returns Unit (0)");

        // SAFETY: same.
        let v2 = unsafe { sigil_ref_deref(cell) };
        assert_eq!(v2, 99);
    }

    #[test]
    fn cell_holds_pointer_value() {
        // Round-trip a heap-string pointer through the cell. The
        // bitmap=0b1 setting means Boehm's conservative scan follows
        // the value slot — exercised more thoroughly by stdlib /
        // closure tests that already rely on conservative scanning of
        // pointer-bearing payloads. Here we only verify the load/store
        // mechanics on a pointer-shaped value.
        use crate::gc::sigil_string_new;

        let _g = gc_test_lock();
        unsafe {
            // SAFETY: `s` is a valid static byte slice; sigil_string_new copies.
            let str_ptr = sigil_string_new(b"hello".as_ptr(), 5);
            let cell = sigil_ref_alloc(str_ptr as u64);

            // SAFETY: cell is alive (held in `cell` local); deref returns
            // the original string pointer.
            let recovered = sigil_ref_deref(cell) as *const u8;
            assert_eq!(recovered, str_ptr);

            // String content is intact: read length via the runtime helper.
            let len = crate::gc::sigil_string_len(recovered);
            assert_eq!(len, 5);
        }
    }

    #[test]
    fn cell_holds_unit_value() {
        // Unit is encoded as i64 0 (low bit clear, not a pointer).
        // Conservative scan tolerates the zero pattern.
        let _g = gc_test_lock();
        let cell = sigil_ref_alloc(0);
        let v = unsafe { sigil_ref_deref(cell) };
        assert_eq!(v, 0);
    }

    #[test]
    fn distinct_cells_are_independent() {
        let _g = gc_test_lock();
        let cell_a = sigil_ref_alloc(1);
        let cell_b = sigil_ref_alloc(2);
        assert_ne!(cell_a, cell_b);

        // SAFETY: both cells just-allocated.
        unsafe { sigil_ref_set(cell_a, 100) };
        assert_eq!(unsafe { sigil_ref_deref(cell_a) }, 100);
        assert_eq!(unsafe { sigil_ref_deref(cell_b) }, 2);
    }
}
