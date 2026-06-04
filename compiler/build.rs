//! Build script for `sigil-compiler` — Plan A2 task 1.5.5.
//!
//! Emits `cargo:rerun-if-changed` hints so sigil-compiler is rebuilt when
//! the runtime's source changes. The runtime staticlib
//! (`libsigil_runtime.a`) is materialised *at test-run time* by the e2e
//! test itself — see `compiler/tests/e2e.rs::ensure_runtime_staticlib`.
//!
//! This script used to invoke `cargo build -p sigil-runtime` inline,
//! which caused a deadlock under `cargo test --workspace` on a cold
//! target directory (the outer cargo holds build-unit locks that the
//! nested cargo would need). The e2e-runtime approach does the rebuild
//! *after* the outer cargo has finished its build phase and released
//! those locks. See PLAN_A2_DEVIATIONS.md ([Task 1.5.5]) for the full
//! rationale.

fn main() {
    println!("cargo:rerun-if-changed=../runtime/src");
    println!("cargo:rerun-if-changed=../runtime/Cargo.toml");
    println!("cargo:rerun-if-changed=../runtime/build.rs");
    // Embedded via include_dir! in src/stdlib_embed.rs — rebuild the
    // compiler (and re-embed) whenever any stdlib source changes.
    println!("cargo:rerun-if-changed=../std");
}
