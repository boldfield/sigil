//! Runtime IO shim — plan A1 Stage 1 task 2.
//!
//! Stage 1 only implements `sigil_println`, the runtime-side handler for
//! `perform IO.println(s)`. It takes a heap-allocated Sigil `String`
//! pointer (the header-pointer form from `sigil_string_new`) and writes
//! the UTF-8 payload followed by a single newline to stdout.
//!
//! The signature differs slightly from the plan's literal
//! `sigil_println(ptr, len)` because every Sigil `String` is a heap object
//! (see DEVIATIONS); codegen passes the heap header pointer. The runtime
//! is the only place that materialises the transient byte pointer needed
//! to drive `write(2)`.

use std::io::{BufRead, Write};

use crate::gc::{sigil_string_new, string_bytes};

/// Write the byte contents of a Sigil `String` to stdout, followed by a
/// newline. Returns nothing; IO errors abort the process (Sigil has no
/// `Result`-in-FFI convention yet).
///
/// # Safety
///
/// `obj` must be a non-null pointer returned by `sigil_string_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_println(obj: *const u8) {
    let (bytes, len) = string_bytes(obj);
    let slice = std::slice::from_raw_parts(bytes, len);

    // Acquire stdout once per call; flushing before newline is unnecessary
    // since stdout is line-buffered in the compiled program at exit.
    let mut out = std::io::stdout().lock();
    if out.write_all(slice).is_err() {
        std::process::abort();
    }
    if out.write_all(b"\n").is_err() {
        std::process::abort();
    }
}

/// Plan C Task 70 — write the byte contents of a Sigil `String` to
/// stdout, *without* a trailing newline. Companion to
/// [`sigil_println`].
///
/// # Safety
///
/// Same as [`sigil_println`].
#[no_mangle]
pub unsafe extern "C" fn sigil_print(obj: *const u8) {
    let (bytes, len) = string_bytes(obj);
    let slice = std::slice::from_raw_parts(bytes, len);
    let mut out = std::io::stdout().lock();
    if out.write_all(slice).is_err() {
        std::process::abort();
    }
}

/// Plan C Task 70 — read a single line from stdin, returning a fresh
/// Sigil `String`. The trailing `\n` (and `\r\n`) is stripped. EOF
/// without bytes returns the empty string. IO errors abort the
/// process.
///
/// # Safety
///
/// Safe to call from any thread; stdin lock is acquired internally.
#[no_mangle]
pub unsafe extern "C" fn sigil_read_line() -> *mut u8 {
    let mut buf = String::new();
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    if handle.read_line(&mut buf).is_err() {
        std::process::abort();
    }
    // Strip exactly one line terminator: a trailing `\n` (and a
    // preceding `\r` if present, so `\r\n` round-trips through the
    // `text\n` convention). Multiple trailing newlines are
    // preserved (only the input-line-terminating newline is
    // consumed); `read_line` itself returns at most one line so in
    // practice `buf` ends in 0 or 1 `\n`.
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    // SAFETY: gc-heap-ptr arithmetic (buf is a stack-local String, not a heap object — false-positive).
    sigil_string_new(buf.as_bytes().as_ptr(), buf.len())
}

// Plan C addendum (CLI external-system effects, EE1) — `sigil_read_file`
// and `sigil_write_file` (which aborted on IO error) were removed
// alongside their `IO.read_file` / `IO.write_file` arm fns. Their
// error-aware replacements live in `runtime/src/fs.rs` (EE2) and
// surface through the `Fs` effect's raw-shape ops + `std/fs.sigil`'s
// `read_file` / `write_file` wrappers returning
// `Result[String, FsError]` / `Result[Unit, FsError]`.

#[cfg(test)]
mod tests {
    use super::*;

    use crate::gc::{sigil_gc_init, sigil_string_new};

    #[test]
    fn println_does_not_panic() {
        let _guard = crate::test_support::gc_test_lock();
        // We can't easily capture stdout from within the test without
        // reopening the file descriptor. This test ensures the happy path
        // runs end-to-end without aborting.
        sigil_gc_init();
        let src = b"hello";
        // SAFETY: gc-heap-ptr arithmetic (src is a static byte literal, not a heap object).
        let obj = unsafe { sigil_string_new(src.as_ptr(), src.len()) };
        unsafe { sigil_println(obj) };
    }
}
