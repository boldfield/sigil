//! Algebraic-effects runtime ABI: `NextStep` discriminant + FFI surface.
//!
//! Plan B Task 56 introduces the runtime side of the effect subsystem
//! (`sigil-runtime::handlers`, populated `sigil-runtime::arena`,
//! `sigil_run_loop`). Plan B Task 55 adds the codegen side that builds
//! `NextStep` records and pushes `HandlerFrame`s.
//!
//! The runtime owns the `NextStep` and `HandlerFrame` byte layouts so the
//! compiler does not have to track field offsets. Codegen interacts with
//! both via runtime-provided FFI helpers (`sigil_next_step_done`,
//! `sigil_next_step_call`, `sigil_handler_frame_new`,
//! `sigil_handler_frame_set_arm`, `sigil_handler_frame_set_return`,
//! `sigil_handle_push`, `sigil_handle_pop`, `sigil_perform`,
//! `sigil_run_loop`). The single value the compiler does need to see
//! directly is the `NextStep` discriminant — when the trampoline finishes,
//! a CPS-color caller may want to peek at the tag (e.g. to assert a
//! `Done` result on entry to a handler's return-arm). The two tag values
//! are pinned here.
//!
//! Wire-format invariants the runtime guarantees:
//!
//! - `NextStep` records are arena-allocated and only valid until the next
//!   `sigil_arena_reset` (called at the top of every `sigil_run_loop`
//!   iteration). The trampoline reads the discriminant + payload into
//!   stack locals before performing the reset.
//! - `HandlerFrame` records are Boehm-heap allocated so they survive
//!   arena resets across iterations. The thread-local handler-stack head
//!   is owned by the runtime; codegen pushes / pops frames via the FFI
//!   helpers rather than touching the head directly.

/// `NextStep` discriminant: terminal — the trampoline returns the held
/// value to its caller (the program's entry shim) and exits the loop.
pub const NEXT_STEP_TAG_DONE: u32 = 0;

/// `NextStep` discriminant: the trampoline should invoke the carried
/// closure with the carried argument list, then dispatch on the result.
pub const NEXT_STEP_TAG_CALL: u32 = 1;

/// `NextStep` discriminant: terminal-from-arm — the carrying op arm
/// body's discard-`k` tail emitted this. Trampoline propagates the
/// value identically to `NEXT_STEP_TAG_DONE` (including routing
/// through the outer post_arm_k stack), but writes the
/// "from-arm-discharge" state to the caller-owned
/// `TerminalResult.tag` slot (Plan D Task 111d; previously the
/// `sigil_last_terminal_tag` TLS) so the handle expression's outer
/// codegen logic can skip return arm dispatch — the discharged
/// arm's value is the handle's final value directly per algebraic-
/// effects semantics, not subject to the return clause's wrapper.
///
/// **Why distinct from `DONE`:** Phase 4g shipped uniform return arm
/// dispatch (PR #29 `dd10379`) on the assumption that "the return
/// clause runs over whatever value flows out of the body". That
/// interpretation produces type-unsoundness when the body's type
/// `B` differs from the handle's overall type `R`: the discharged
/// arm value has type `R` but is passed through the return clause
/// expecting type `B`. Symptom: `examples/state.sigil`'s canonical
/// `run_state` shape produces a heap-pointer-shaped value when
/// invoked, because the discharge value (a closure record pointer
/// at type `R`) is passed as `v: B` (which is `Int`) into the
/// return arm, which then computes pointer arithmetic. The fix:
/// distinguish the discharge path so the handle's outer logic
/// skips return arm when the body terminates via discharge.
pub const NEXT_STEP_TAG_DISCHARGED: u32 = 2;

/// Maximum user-arg count a `perform` site can pack into the inline args
/// buffer (plus the implicit `(k_closure_ptr, k_fn_ptr)` pair the runtime
/// appends, so the trampoline-side cap is `MAX_INLINE_ARGS + 2` total
/// dispatched values). Sized to comfortably exceed v1 effect arities
/// (Raise, State, Choose all use 0–2 user args) without growing the
/// stack-resident `args_buf` in `sigil_run_loop`.
///
/// Cross-boundary constant: the compiler's args-buffer packing in
/// `lower_perform_non_io_to_value` (Task 55 Phase 4b) and the runtime's
/// `sigil_perform` / `sigil_next_step_call` / `sigil_run_loop` overflow
/// checks all read from this single source. Bumping it requires
/// auditing both sides.
pub const MAX_INLINE_ARGS: u32 = 32;

/// Maximum op-arms a single `HandlerFrame` can carry. Bounded by the
/// 32-bit GC pointer-bitmap on `HandlerFrame`: arm `i`'s closure
/// pointer lives at payload word `5 + 2*i`, so the highest reachable
/// bit is `5 + 2*13 = 31` at `i = 13`. With `MAX_HANDLER_ARMS = 14`
/// (i.e., `i ∈ [0, 13]`) the bitmap is fully utilised; one less and
/// bit 31 stays empty. v1 effects ship with 1–3 ops; the cap is
/// comfortably above realistic v1 needs.
///
/// Cross-boundary constant: the runtime's
/// `sigil_handler_frame_new` overflow check
/// (`runtime/src/handlers.rs`) and the compiler's codegen-walker
/// per-effect arm-count cap (Plan B Task 55 Phase 4f polish round)
/// both read this constant. Promoted to `sigil_abi::effect` from
/// `sigil_runtime::handlers` at the Phase 4f polish-round commit so
/// the walker can reject `MAX_HANDLER_ARMS + 1` arms-per-effect at
/// compile time rather than runtime-aborting in
/// `sigil_handler_frame_new`. Bumping it requires auditing both
/// sides + the GC pointer-bitmap derivation in
/// `runtime/src/handlers.rs::handler_frame_pointer_bitmap`.
pub const MAX_HANDLER_ARMS: u32 = 14;

/// Byte offset of `HandlerFrame::return_fn` within the `#[repr(C)]`
/// struct (`runtime/src/handlers.rs`'s `HandlerFrame`). Pinned here so
/// codegen (Plan B Task 55 Phase 4g) can read the slot directly off
/// the `frame_1_ptr_snapshot` SSA Value at handle exit, rather than
/// going through a runtime FFI accessor — Phase 4g's "no new FFI
/// required" architectural choice (per `[DEVIATION Task 55] Phase 4g`
/// in `PLAN_B_DEVIATIONS.md`).
///
/// Layout context (struct fields in declaration order):
///
/// ```text
///   offset  0: effect_id        (u32, 4 bytes)
///   offset  4: arm_count        (u32, 4 bytes)
///   offset  8: return_fn        (*mut u8, 8 bytes)   ← here
///   offset 16: return_closure   (*mut u8, 8 bytes)
///   offset 24: prev             (*mut HandlerFrame)
///   offset 32: arms[]           (variable-length)
/// ```
///
/// Cross-boundary constant: `runtime/src/handlers.rs`'s
/// `handler_frame_return_offsets_match_abi_constants` test asserts
/// this constant equals `core::mem::offset_of!(HandlerFrame,
/// return_fn)` so a future struct reorder breaks at the test
/// rather than silently miscompiling in codegen.
pub const HANDLER_FRAME_RETURN_FN_OFF: i32 = 8;

/// Byte offset of `HandlerFrame::return_closure` within the
/// `#[repr(C)]` struct. Pinned for the same reason as
/// `HANDLER_FRAME_RETURN_FN_OFF`; see that constant's docs.
pub const HANDLER_FRAME_RETURN_CLOSURE_OFF: i32 = 16;

/// `HandlerFrame` payload size in bytes for a frame carrying
/// `arm_count` op arms. 32 bytes of fixed header
/// (`effect_id` + `arm_count` + `return_fn` + `return_closure` +
/// `prev`) plus 16 bytes per arm (`fn_ptr` + `closure_ptr`).
///
/// Shared between the runtime (which calls this from
/// `sigil_handler_frame_new` to compute its Boehm allocation
/// size) and the compiler (which calls this from the static-
/// descriptor-table pre-pass to register the 14 possible
/// handler-frame shapes at codegen time). Promoting the runtime's
/// helper to a `pub const fn` here closes the duplication risk
/// the static-descriptor-table plan's "Notes for the implementer"
/// section flagged.
#[inline]
pub const fn handler_frame_payload_bytes(arm_count: u32) -> usize {
    32 + 16 * (arm_count as usize)
}

/// Sigil pointer bitmap for a `HandlerFrame` carrying `arm_count`
/// op arms. Bit `k` is `1` iff payload word `k` holds a Sigil
/// (Boehm-managed) heap pointer.
///
/// Layout (payload words, indexed from 0):
/// - 0: `(effect_id, arm_count)` — non-pointer.
/// - 1: `return_fn` — function pointer (not GC-tracked).
/// - 2: `return_closure` — GC pointer (bit 2).
/// - 3: `prev` — GC pointer (bit 3).
/// - 4 + 2*i: arm `i` `fn_ptr` — function pointer (skip).
/// - 5 + 2*i: arm `i` `closure_ptr` — GC pointer (bit 5 + 2*i).
///
/// The `MAX_HANDLER_ARMS = 14` cap keeps the highest set bit
/// at `5 + 2*13 = 31`, fitting in the 32-bit bitmap. Callers
/// that pass `arm_count > MAX_HANDLER_ARMS` get UB at the shift
/// (shifting by 32+ on a `u32`); the runtime entry point checks
/// this before calling, and the compiler's pre-pass iterates
/// `arm_count ∈ [1, MAX_HANDLER_ARMS]` explicitly.
///
/// Shared between the runtime and compiler for the same reason
/// as `handler_frame_payload_bytes` above.
#[inline]
pub const fn handler_frame_pointer_bitmap(arm_count: u32) -> u32 {
    let mut bitmap: u32 = (1u32 << 2) | (1u32 << 3);
    let mut i: u32 = 0;
    while i < arm_count {
        let closure_word_idx = 5 + 2 * i;
        bitmap |= 1u32 << closure_word_idx;
        i += 1;
    }
    bitmap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_tags_are_distinct_and_pinned() {
        // Pinning the literal values: the compiler-side will compare the
        // discriminant against these constants when the codegen wiring
        // (Task 55) reads a `Done` result, so renumbering them is an ABI
        // break that needs an audit on both sides.
        assert_eq!(NEXT_STEP_TAG_DONE, 0);
        assert_eq!(NEXT_STEP_TAG_CALL, 1);
        assert_eq!(NEXT_STEP_TAG_DISCHARGED, 2);
        assert_ne!(NEXT_STEP_TAG_DONE, NEXT_STEP_TAG_CALL);
        assert_ne!(NEXT_STEP_TAG_DONE, NEXT_STEP_TAG_DISCHARGED);
        assert_ne!(NEXT_STEP_TAG_CALL, NEXT_STEP_TAG_DISCHARGED);
    }

    #[test]
    fn max_inline_args_pinned_at_32() {
        // Pinning the literal: codegen and runtime overflow checks
        // both read this constant, so a bump requires auditing both
        // sides + the GC test in `runtime/src/handlers.rs`.
        assert_eq!(MAX_INLINE_ARGS, 32);
    }

    #[test]
    fn max_handler_arms_pinned_at_14() {
        // Pinning the literal: the cap is structurally derived from
        // the 32-bit GC pointer-bitmap on `HandlerFrame` (highest
        // reachable bit = 5 + 2*13 = 31). Bumping requires growing
        // the bitmap word size in `runtime/src/handlers.rs`.
        assert_eq!(MAX_HANDLER_ARMS, 14);
    }

    #[test]
    fn handler_frame_helpers_match_known_layout() {
        // Pinning the helpers against hand-derived values for the
        // edge cases the runtime and codegen both rely on. A drift
        // here would silently miscompute the descriptor index lookup
        // in the static-descriptor-table pre-pass (compiler) and
        // the Boehm typed-malloc routing (runtime).
        assert_eq!(handler_frame_payload_bytes(0), 32);
        assert_eq!(handler_frame_payload_bytes(1), 48);
        assert_eq!(
            handler_frame_payload_bytes(MAX_HANDLER_ARMS),
            32 + 16 * MAX_HANDLER_ARMS as usize
        );
        assert_eq!(handler_frame_pointer_bitmap(0), 0b0000_1100);
        assert_eq!(handler_frame_pointer_bitmap(1), 0b0010_1100);
        // arm_count = MAX exercises the highest bit (closure slot at
        // payload word 31).
        let max_bitmap = handler_frame_pointer_bitmap(MAX_HANDLER_ARMS);
        assert!(max_bitmap & (1u32 << 31) != 0);
    }

    #[test]
    fn handler_frame_helpers_are_const_eval_callable() {
        // The compiler's static-descriptor-table pre-pass calls these
        // helpers in const context (or close enough — at codegen
        // initialization, before any per-fn lowering). Pinning the
        // const-fn property prevents an accidental promotion of the
        // helpers to non-const (e.g., adding a `debug_assert!` with a
        // non-const formatter argument) from silently breaking the
        // shape table population.
        const ZERO_ARMS_BYTES: usize = handler_frame_payload_bytes(0);
        const MAX_ARMS_BITMAP: u32 = handler_frame_pointer_bitmap(MAX_HANDLER_ARMS);
        assert_eq!(ZERO_ARMS_BYTES, 32);
        assert!(MAX_ARMS_BITMAP & (1u32 << 31) != 0);
    }

    #[test]
    fn handler_frame_return_offsets_pinned() {
        // Pinning the literal byte offsets: codegen Phase 4g reads
        // `return_fn` and `return_closure` directly off the
        // `frame_1_ptr_snapshot` Value at handle exit. The
        // `handler_frame_return_offsets_match_abi_constants` test in
        // `runtime/src/handlers.rs` pairs with this one to assert the
        // constants match `offset_of!(HandlerFrame, ...)`.
        assert_eq!(HANDLER_FRAME_RETURN_FN_OFF, 8);
        assert_eq!(HANDLER_FRAME_RETURN_CLOSURE_OFF, 16);
    }
}
