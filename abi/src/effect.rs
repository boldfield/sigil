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
        assert_ne!(NEXT_STEP_TAG_DONE, NEXT_STEP_TAG_CALL);
    }
}
