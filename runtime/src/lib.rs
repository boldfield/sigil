//! Sigil runtime — static library linked into every compiled program.
//!
//! v1 provides: a Boehm-backed GC, a String/IO runtime shim, stable object
//! header construction, stub trampoline arena (populated in Plan B),
//! instrumentation counters, and object-file stackmap section names.
//!
//! Submodules arrive across Stage 1 tasks. Task 0.10 adds `counters`; task
//! 0.11 adds `stackmap`.

pub mod counters;
pub mod stackmap;
