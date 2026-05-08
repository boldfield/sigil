//! Runtime support for sigil-side `panic` / `assert` (Plan C addendum).
//!
//! `sigil_panic(msg_ptr)` writes the Sigil `String` payload to stderr
//! followed by a single newline, then exits the process with status 1.
//! `assert` lowers in the compiler to `if cond { unit } else { panic(msg) }`,
//! so it does not need a runtime entry point of its own.
//!
//! Aborting via `std::process::exit(1)` flushes Rust stdio and runs drop
//! handlers; `libc::abort` would dump core, which is too aggressive for
//! the LLM-first surface this is targeting.
//!
//! See `spec/language.md` "Diagnostics" for user-facing semantics.

use std::io::Write;
use std::process;

use crate::gc::string_bytes;

/// Print the contents of a Sigil `String` to stderr followed by `\n`,
/// then exit the process with status 1.
///
/// # Safety
///
/// `msg_ptr` must point at a valid `TAG_STRING` header (the result of
/// `sigil_string_new` or any other `String`-producing primitive).
#[no_mangle]
pub unsafe extern "C" fn sigil_panic(msg_ptr: *const u8) -> ! {
    let (bytes, len) = string_bytes(msg_ptr);
    let slice = std::slice::from_raw_parts(bytes, len);

    let mut err = std::io::stderr().lock();
    // Best-effort: if write fails (closed fd, etc.) the message is lost
    // but the abort is still observable via the exit status. Don't
    // double-fault by aborting on a stderr write failure during a panic.
    let _ = err.write_all(slice);
    let _ = err.write_all(b"\n");
    let _ = err.flush();

    process::exit(1)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    // `sigil_panic` calls `process::exit(1)`, so it can only be exercised
    // from a subprocess. The runtime crate's existing `cargo test` flow
    // does not spawn subprocesses, so the abort behaviour is covered by
    // the e2e harness in `compiler/tests/e2e.rs` instead (see
    // `panic_aborts_with_message_on_stderr` and friends).
    //
    // What we *can* test in-process is that `sigil_panic` is exported
    // with the expected ABI — the linker would have caught a signature
    // mismatch, but a compile-time reference doubles as a smoke test.
    use super::sigil_panic;

    #[test]
    fn sigil_panic_is_exported_with_expected_signature() {
        let _f: unsafe extern "C" fn(*const u8) -> ! = sigil_panic;
    }
}
