//! Runtime profile-data emission surface — plan 2026-05-08-sigil-v2-
//! runtime-profile-data.
//!
//! Six phases:
//!
//! - Phase 1 (compile-time): `--emit-symbol-table` → `prog.symtab`
//!   sidecar (see `compiler/src/symtab.rs`).
//! - Phase 2: frame-pointer stack walker ([`unwind`]).
//! - Phase 3: SIGPROF-driven CPU sampler ([`cpu`]).
//! - Phase 4: sampled allocation profiler ([`alloc`]).
//! - Phase 5: pprof ([`pprof`]) + folded-stacks ([`folded`]) writers
//!   behind the [`output`] dispatcher.
//! - Phase 6: spec + end-to-end validation.
//!
//! The whole surface is **zero-overhead when disabled**: the env-var
//! gates short-circuit before any allocation or syscall, and the
//! sampling hooks compile down to a TLS load + branch on the cold
//! path. The signal handler and drainer thread are only installed
//! when `SIGIL_CPU_PROFILE` or `SIGIL_ALLOC_PROFILE` is set; in the
//! unset case [`cpu::maybe_init`] / [`alloc::maybe_init`] return
//! `false` after a single `env::var_os` lookup.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod alloc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod cpu;
pub mod folded;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod output;
pub mod pprof;
pub mod resolve;
pub mod ring;
pub mod sample;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod sys;
pub mod unwind;

/// Top-level init invoked by `sigil_gc_init` (inside its `Once`).
/// Walks every profile module's `maybe_init`. Zero-overhead when
/// neither env var is set — each `maybe_init` returns after a single
/// `env::var_os` lookup.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn maybe_init() {
    let _ = cpu::maybe_init();
    let _ = alloc::maybe_init();
}

/// No-op on non-Linux / non-macOS hosts. The signal-based sampler
/// surface assumes ITIMER_PROF and SIGPROF; other platforms fall
/// through silently.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn maybe_init() {}
