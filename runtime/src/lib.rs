//! Sigil runtime — static library linked into every compiled program.
//!
//! Vertical-slice surface:
//!
//! - `value` — tagged Value representation (Int, heap, immediate).
//! - `header` — 8-byte object header, single source of truth.
//! - `gc` — Boehm init, `sigil_alloc`, `sigil_string_new`, `sigil_string_len`.
//! - `io` — `sigil_println`.
//! - `arena` — Plan B Task 56: bump allocator for the CPS trampoline,
//!   `sigil_arena_alloc` / `sigil_arena_reset` / `sigil_arena_promote`.
//! - `handlers` — Plan B Task 56: HandlerFrame, thread-local handler
//!   stack, `sigil_perform`, `sigil_run_loop`, NextStep helpers.
//! - `counters` — runtime instrumentation counters (task 0.10).
//! - `stackmap` — object-file section constants (task 0.11).
//! - `arith` — Plan A2 task 25 / Plan B Task 57: `sigil_int_to_-
//!   string`, checked-overflow arith primitives. Plan A2's
//!   `sigil_panic_arith_error` retired in Task 57; replaced by the
//!   ArithError handler-arm fns in `handlers` (see below).
//! - `byte` — Plan A2 task 25: `Byte` conversion and wrapping arith
//!   primitives.
//!
//! FFI exports for the compiler:
//!
//! - `sigil_gc_init`
//! - `sigil_alloc`
//! - `sigil_string_new`, `sigil_string_len`
//! - `sigil_println`
//! - `sigil_counter_read`, `sigil_counter_print_all`
//! - `sigil_int_to_string` (Plan A2)
//! - `sigil_checked_add`, `sigil_checked_sub`, `sigil_checked_mul` (Plan A2)
//! - `sigil_byte_from_int_checked`, `sigil_byte_to_int`,
//!   `sigil_byte_add`, `sigil_byte_sub` (Plan A2)
//! - `sigil_arena_alloc`, `sigil_arena_reset`, `sigil_arena_promote` (Plan B)
//! - `sigil_handler_frame_new`, `sigil_handler_frame_set_arm`,
//!   `sigil_handler_frame_set_return`, `sigil_handle_push`,
//!   `sigil_handle_pop`, `sigil_perform`, `sigil_run_loop`,
//!   `sigil_next_step_done`, `sigil_next_step_call`,
//!   `sigil_next_step_args_ptr`, `sigil_continuation_identity` (Plan B)
//! - `sigil_next_step_discharged` (Stage 6.8-followup Bug 2 fix —
//!   distinguish op-arm-discharge from body-normal completion so
//!   handle expression skips return arm dispatch on discharge per
//!   algebraic-effects semantics). Plan D Task 111 replaced the
//!   companion TLS-out-channel (`sigil_last_terminal_tag` /
//!   `sigil_last_terminal_value` and their reset helpers) with
//!   `sigil_run_loop`'s packed multi-return.
//! - `sigil_io_println_arm`, `sigil_arith_error_div_by_zero_arm`,
//!   `sigil_arith_error_mod_by_zero_arm` (Plan B Task 57 — runtime-
//!   side default arm fns installed by the `main` shim's top-level
//!   handler frames)

pub mod arena;
pub mod arith;
pub mod array;
pub mod byte;
pub mod byte_array;
pub mod clock;
pub mod counters;
pub mod gc;
pub mod handlers;
pub mod header;
pub mod int64;
pub mod io;
pub mod mem;
pub mod random;
pub mod stackmap;
pub mod string;
pub mod string_builder;
pub mod value;

#[cfg(test)]
pub(crate) mod test_support;
