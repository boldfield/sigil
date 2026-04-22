//! Embedded Sigil standard library — Plan A1 Stage 1 task 3.
//!
//! The workspace's `std/` tree is embedded via `include_dir!` at compile
//! time. Import resolution reads only from the embedded tree; the
//! filesystem is never consulted for `std.*` modules. This yields a
//! single-binary distribution.

use include_dir::{include_dir, Dir};

/// Embedded read-only view of `std/`.
pub static STD: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../std");

/// Return the source text of an embedded stdlib file, or `None` if the
/// path doesn't exist inside the embedded tree. `path` uses forward
/// slashes (e.g. `io.sigil`).
pub fn get(path: &str) -> Option<&'static str> {
    STD.get_file(path).and_then(|f| f.contents_utf8())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn std_io_is_embedded() {
        let s = get("io.sigil").expect("std/io.sigil missing from embed");
        assert!(s.contains("effect IO"));
    }
}
