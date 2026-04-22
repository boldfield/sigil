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

use std::io::Write;

use crate::gc::string_bytes;

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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::gc::{sigil_gc_init, sigil_string_new};

    #[test]
    fn println_does_not_panic() {
        // We can't easily capture stdout from within the test without
        // reopening the file descriptor. This test ensures the happy path
        // runs end-to-end without aborting.
        sigil_gc_init();
        let src = b"hello";
        // SAFETY: not an interior pointer (src is a static byte literal, not a heap object).
        let obj = unsafe { sigil_string_new(src.as_ptr(), src.len()) };
        unsafe { sigil_println(obj) };
    }
}
