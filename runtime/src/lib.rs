//! Sigil runtime — static library linked into every compiled program.
//!
//! Vertical-slice surface:
//!
//! - `value` — tagged Value representation (Int, heap, immediate).
//! - `header` — 8-byte object header, single source of truth.
//! - `gc` — Boehm init, `sigil_alloc`, `sigil_string_new`, `sigil_string_len`.
//! - `io` — `sigil_println`.
//! - `arena` — Plan B stub (trampoline arena).
//! - `counters` — runtime instrumentation counters (task 0.10).
//! - `stackmap` — object-file section constants (task 0.11).
//! - `arith` — Plan A2 task 25: `sigil_panic_arith_error`,
//!   `sigil_int_to_string`, checked-overflow arith primitives.
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
//! - `sigil_panic_arith_error` (Plan A2)
//! - `sigil_int_to_string` (Plan A2)
//! - `sigil_checked_add`, `sigil_checked_sub`, `sigil_checked_mul` (Plan A2)
//! - `sigil_byte_from_int_checked`, `sigil_byte_to_int`,
//!   `sigil_byte_add`, `sigil_byte_sub` (Plan A2)
//! - (stubs) `sigil_arena_alloc`, `sigil_arena_reset`

pub mod arena;
pub mod arith;
pub mod byte;
pub mod counters;
pub mod gc;
pub mod header;
pub mod io;
pub mod stackmap;
pub mod value;

#[cfg(test)]
pub(crate) mod test_support;
