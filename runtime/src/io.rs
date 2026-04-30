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

/// Plan C Task 70 — read the contents of `path` as a byte sequence
/// and return it as a fresh Sigil `String`. Aborts on IO error
/// (missing file, permission denied) or invalid UTF-8.
///
/// # Safety
///
/// `path_obj` must be a non-null pointer returned by
/// `sigil_string_new` whose payload is a valid filesystem path.
#[no_mangle]
pub unsafe extern "C" fn sigil_read_file(path_obj: *const u8) -> *mut u8 {
    let (bytes, len) = string_bytes(path_obj);
    let slice = std::slice::from_raw_parts(bytes, len);
    let path = match std::str::from_utf8(slice) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("sigil_read_file: path is not valid UTF-8");
            std::process::abort();
        }
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sigil_read_file: failed to read `{path}`: {e}");
            std::process::abort();
        }
    };
    // SAFETY: gc-heap-ptr arithmetic (contents is a stack-local String, not a heap object — false-positive).
    sigil_string_new(contents.as_bytes().as_ptr(), contents.len())
}

/// Plan C Task 70 — write the contents of `data_obj` to the file at
/// `path_obj`, replacing any existing contents. Aborts on IO error.
///
/// # Safety
///
/// Both arguments must be non-null pointers returned by
/// `sigil_string_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_write_file(path_obj: *const u8, data_obj: *const u8) {
    let (path_bytes, path_len) = string_bytes(path_obj);
    let path_slice = std::slice::from_raw_parts(path_bytes, path_len);
    let path = match std::str::from_utf8(path_slice) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("sigil_write_file: path is not valid UTF-8");
            std::process::abort();
        }
    };
    let (data_bytes, data_len) = string_bytes(data_obj);
    let data_slice = std::slice::from_raw_parts(data_bytes, data_len);
    if let Err(e) = std::fs::write(path, data_slice) {
        eprintln!("sigil_write_file: failed to write `{path}`: {e}");
        std::process::abort();
    }
}

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
