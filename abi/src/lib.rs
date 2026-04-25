//! Shared ABI surface that crosses the compiler↔runtime boundary.
//!
//! Plan B Stage 4.5.5 extracted the duplicated constants from
//! `compiler/src/codegen.rs` and `runtime/src/stackmap.rs` (stackmap wire
//! format) and `runtime/src/value.rs` (tagged-pointer bit layout) into
//! this leaf crate so there is one source of truth that both sides
//! consume. Before Stage 4.5.5 the stackmap constants existed in two
//! places kept loosely in sync by parallel unit tests; bumping the
//! version field (Plan B replaces the placeholder with real safepoint
//! data) would have doubled the drift surface if the duplication had
//! persisted.
//!
//! Scope rules:
//! - `#![no_std]` and zero dependencies. Both `sigil-compiler` and
//!   `sigil-runtime` (the latter is a `staticlib`) consume from this
//!   crate; pulling `std` or other crates would impose those costs on
//!   every linked Sigil binary.
//! - Pure data declarations only. Helpers that operate on these
//!   constants (e.g. `from_int` / `as_int` for tagged Values, or
//!   `parse_section` for stackmap bytes) stay in the consumer crate
//!   that needs them. The split keeps `sigil-abi` testable as a leaf
//!   without having to import IO or allocation surface from std.
//! - The `sigil-header-constants` crate continues to own the 8-byte
//!   object-header bit layout. That crate predates `sigil-abi` (PR #7,
//!   Plan A2) and has the same shape as this one; merging them is a
//!   future refactor that is not required by Plan B's deviation budget.

#![no_std]

pub mod stackmap;
pub mod tag;
