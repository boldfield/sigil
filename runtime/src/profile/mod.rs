//! Runtime profile-data emission surface — plan 2026-05-08-sigil-v2-
//! runtime-profile-data.
//!
//! Six phases:
//!
//! - Phase 1 (compile-time): `--emit-symbol-table` → `prog.symtab`
//!   sidecar (see `compiler/src/symtab.rs`).
//! - Phase 2: frame-pointer stack walker (this module's [`unwind`]).
//! - Phase 3: SIGPROF-driven CPU sampler.
//! - Phase 4: sampled allocation profiler.
//! - Phase 5: pprof + folded-stacks writers.
//! - Phase 6: spec + end-to-end validation.
//!
//! The whole surface is **zero-overhead when disabled**: the env-var
//! gates short-circuit before any allocation or syscall, and the
//! sampling hooks compile down to a TLS load + branch on the cold
//! path. Phase 3 / Phase 4 own those gates; this module's
//! [`unwind::capture_stack`] is a leaf primitive with no global state.

pub mod unwind;
