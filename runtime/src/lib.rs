//! Sigil runtime — static library linked into every compiled program.
//!
//! Stage 1 vertical slice surface:
//!
//! - `value` — tagged Value representation (Int, heap, immediate).
//! - `header` — 8-byte object header, single source of truth.
//! - `gc` — Boehm init, `sigil_alloc`, `sigil_string_new`, `sigil_string_len`.
//! - `io` — `sigil_println`.
//! - `arena` — Plan B stub (trampoline arena).
//! - `counters` — runtime instrumentation counters (task 0.10).
//! - `stackmap` — object-file section constants (task 0.11).
//!
//! FFI exports for the compiler:
//!
//! - `sigil_gc_init`
//! - `sigil_alloc`
//! - `sigil_string_new`, `sigil_string_len`
//! - `sigil_println`
//! - `sigil_counter_read`, `sigil_counter_print_all`
//! - (stubs) `sigil_arena_alloc`, `sigil_arena_reset`

pub mod arena;
pub mod counters;
pub mod gc;
pub mod header;
pub mod io;
pub mod stackmap;
pub mod value;
