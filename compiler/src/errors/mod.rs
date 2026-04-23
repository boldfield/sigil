pub mod catalog;
pub mod diagnostic;

pub use catalog::{lookup, ErrorCode, ErrorEntry, CATALOG};
pub use diagnostic::{CompilerError, DiagnosticEmitter, ErrorFormat, Severity, Span};

/// Resolve a catalog code string to an `ErrorCode`.
///
/// Absence of a catalog entry for a compiler-owned string is a build-time
/// invariant (the catalog seed is checked into the tree alongside every
/// call site that references it; the `CATALOG` unit test asserts all codes
/// are unique and non-empty). Consolidating lookup here reduces the
/// compiler's `panic!` / `unreachable!` surface to a single call site,
/// matched by the `disallowed-macros` rule in `clippy.toml`.
pub fn code(code: &str) -> ErrorCode {
    match ErrorCode::new(code) {
        Some(c) => c,
        None => unreachable!("catalog missing {code}: build-time invariant"),
    }
}
