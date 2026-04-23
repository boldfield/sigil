//! Boehm GC integration — plan A1 Stage 1 task 2.
//!
//! The runtime wraps Boehm's `GC_init` + `GC_malloc` in the FFI surface
//! the compiler emits calls to. Header construction happens on the caller
//! side (through `header::Header::new`); `sigil_alloc` writes the header
//! to the first 8 bytes of the Boehm block and returns a pointer to that
//! header.
//!
//! **Allocations always return a pointer to the header**, never to the
//! payload or a field. This is the no-interior-pointers invariant.
//!
//! `sigil_string_new(bytes, len)` is a Stage-1 convenience that allocates
//! a String object, copies the bytes, and returns the tagged heap value.
//! Generalised String construction arrives with the stdlib in later plans.

use std::ffi::c_void;
use std::sync::Once;

use crate::counters::{self, sigil_counter_print_all, CounterId};
use crate::header::{self, Header};

// Direct Boehm FFI — we do not depend on a Rust wrapper crate. These are
// the stable symbols exported by libgc.
#[link(name = "gc")]
extern "C" {
    fn GC_init();
    fn GC_malloc(size: usize) -> *mut c_void;
    // `GC_malloc_atomic` is used for strings (no GC-managed pointers in the
    // payload) so Boehm can skip scanning them during mark phases. Safe on
    // all supported hosts.
    fn GC_malloc_atomic(size: usize) -> *mut c_void;
}

// `atexit` from the C runtime. Used by `sigil --print-runtime-stats` to
// dump counters when the compiled program exits. We avoid depending on
// the `libc` crate (not in the plan's dependency allow-list) and declare
// the signature directly.
extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn counter_atexit_cb() {
    sigil_counter_print_all();
}

static GC_INIT: Once = Once::new();

/// Initialise Boehm GC. Safe to call multiple times from any number of
/// threads; only the first caller runs `GC_init()` and all others wait on
/// `Once` until init completes. The generated `main` shim calls this
/// exactly once before transferring control to user code; tests also call
/// it (serialised by `Once`).
///
/// Also honours `SIGIL_PRINT_STATS=1`: when set on entry, an `atexit`
/// hook is installed that prints every runtime counter to stderr at
/// process exit. `sigil --print-runtime-stats <input>` sets this env
/// var on the child process it spawns.
#[no_mangle]
pub extern "C" fn sigil_gc_init() {
    GC_INIT.call_once(|| {
        // SAFETY: `Once::call_once` guarantees exactly one invocation even
        // under concurrent entry, so Boehm's non-reentrant init runs on a
        // single thread.
        unsafe { GC_init() };

        // Wire the counter dump exactly once — doing it inside the Once
        // guarantees atexit sees exactly one registration per process.
        if std::env::var_os("SIGIL_PRINT_STATS").is_some() {
            // SAFETY: atexit only requires the callback pointer to be
            // valid for the lifetime of the process; `counter_atexit_cb`
            // is a static function with no captured state.
            unsafe { atexit(counter_atexit_cb) };
        }
    });
}

/// Allocate `8 + payload_bytes` from Boehm, write the 8-byte header, and
/// return a pointer to the header (never to the payload). Callers hold a
/// header pointer as their canonical reference to the object.
///
/// `payload_bytes` is the number of bytes after the header; it does not
/// need to be word-aligned, but callers generally size objects to whole
/// words so header fields stay consistent.
///
/// # Safety
///
/// Safe to call from any thread. Does not trap on out-of-memory — Boehm
/// aborts the process via its default oom-handler on OOM, which v1 does
/// not override.
#[no_mangle]
pub extern "C" fn sigil_alloc(header: u64, payload_bytes: usize) -> *mut u8 {
    let total = 8usize.saturating_add(payload_bytes);

    // Bump Boehm counters before the alloc call so a panic inside Boehm
    // (e.g. oom abort) still shows the intent in telemetry.
    counters::incr(CounterId::BoehmAllocCount);
    counters::add(CounterId::BoehmAllocBytes, total as u64);

    let h = Header(header);
    let raw = if h.pointer_bitmap() == 0 {
        // No GC pointers in the payload — Boehm can skip scanning the
        // bytes (saves mark-phase cost). Atomic in Boehm's vocabulary.
        unsafe { GC_malloc_atomic(total) as *mut u8 }
    } else {
        unsafe { GC_malloc(total) as *mut u8 }
    };

    if raw.is_null() {
        // Boehm's default oom-handler aborts before returning null, so
        // reaching here means something has gone wrong that the runtime
        // cannot recover from. Abort cleanly.
        eprintln!("sigil_alloc: Boehm returned null");
        std::process::abort();
    }

    // SAFETY: `raw` points to at least `total` bytes obtained from GC_malloc
    // (or GC_malloc_atomic), and `total >= 8`. Writing the header word is
    // an aligned u64 write at the start of a freshly-returned block. This
    // is not an interior pointer (the header IS the object's header).
    unsafe {
        let hdr_ptr: *mut u64 = raw.cast();
        hdr_ptr.write(header);
    }
    raw
}

/// Allocate and populate a Sigil `String` object from a byte slice.
///
/// Layout of a `String` object on the heap:
///
/// ```text
/// offset 0  : 8-byte header (tag TAG_STRING, count = ceil(8 + len) / 8, bitmap 0)
/// offset 8  : u64 length (in bytes)
/// offset 16 : UTF-8 bytes (length bytes, then zero-pad to word alignment)
/// ```
///
/// Bytes are read from `src` and copied verbatim into the payload. Callers
/// are responsible for ensuring `src` points to `len` readable bytes that
/// form valid UTF-8 (v1 does not validate — Stage 1 only emits literals
/// that are known-valid at compile time).
///
/// Returns a raw pointer to the header. Tagging as a Sigil `Value` is the
/// caller's responsibility (typically done via `value::from_heap`).
///
/// # Safety
///
/// `src` must be non-null and point to at least `len` readable bytes, or
/// `src` may be null when `len == 0`. Any other combination is UB.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_new(src: *const u8, len: usize) -> *mut u8 {
    // The payload is: one length word (8 bytes) + len data bytes, padded up
    // to a multiple of 8 so the object's payload-word count is a whole
    // number. For Stage 1's hello-world all strings are ≤63 words.
    let payload_bytes = 8 + round_up_to_word(len);
    let payload_words = (payload_bytes / 8) as u8;

    let h = Header::new(header::TAG_STRING, payload_words, 0);
    let obj = sigil_alloc(h.raw(), payload_bytes);

    // Write the length word at offset 8.
    //
    // SAFETY: not an interior pointer (pointer arithmetic is to local stack
    // variables inside runtime, computed only to drive a single aligned
    // store, not stored or passed). obj+8 is still inside the object but
    // the write and the read below are transient.
    let len_ptr: *mut u64 = obj.add(8).cast();
    len_ptr.write(len as u64);

    // Copy the byte payload at offset 16.
    //
    // SAFETY: not an interior pointer (temporary pointers used only for a
    // single byte-range copy, never returned to caller).
    if len > 0 && !src.is_null() {
        let dst = obj.add(16);
        std::ptr::copy_nonoverlapping(src, dst, len);
    }

    obj
}

/// Read the length (in bytes) of a heap `String` object. Caller passes the
/// header-pointer form; interior pointers are never produced.
///
/// # Safety
///
/// `obj` must be a pointer to a valid `String` header previously returned
/// by `sigil_string_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_len(obj: *const u8) -> usize {
    // SAFETY: not an interior pointer (used transiently for a single read).
    let len_ptr: *const u64 = obj.add(8).cast();
    len_ptr.read() as usize
}

/// Borrow the raw UTF-8 byte slice out of a heap `String` for the duration
/// of a syscall. The pointer is transient — callers must not store it.
///
/// # Safety
///
/// Same contract as `sigil_string_len`. The returned pointer is valid for
/// `sigil_string_len(obj)` bytes for as long as `obj` is live (which
/// Boehm ensures for the duration of the call chain).
pub(crate) unsafe fn string_bytes(obj: *const u8) -> (*const u8, usize) {
    let len = sigil_string_len(obj);
    // SAFETY: not an interior pointer (immediately consumed by the caller
    // for a single write syscall; never stored or passed back across
    // FFI or module boundaries).
    let bytes = obj.add(16);
    (bytes, len)
}

#[inline]
fn round_up_to_word(n: usize) -> usize {
    (n + 7) & !7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_up_handles_boundaries() {
        assert_eq!(round_up_to_word(0), 0);
        assert_eq!(round_up_to_word(1), 8);
        assert_eq!(round_up_to_word(7), 8);
        assert_eq!(round_up_to_word(8), 8);
        assert_eq!(round_up_to_word(9), 16);
    }

    #[test]
    fn alloc_and_read_string() {
        sigil_gc_init();
        let before_count = counters::read(CounterId::BoehmAllocCount);
        let src = b"hi";
        // SAFETY: not an interior pointer (src is a static byte literal, not a heap object).
        let obj = unsafe { sigil_string_new(src.as_ptr(), src.len()) };
        assert!(!obj.is_null());
        let after_count = counters::read(CounterId::BoehmAllocCount);
        assert!(
            after_count > before_count,
            "Boehm alloc counter must bump on String allocation"
        );

        let len = unsafe { sigil_string_len(obj) };
        assert_eq!(len, 2);

        let (bytes, len2) = unsafe { string_bytes(obj) };
        assert_eq!(len2, 2);
        let slice = unsafe { std::slice::from_raw_parts(bytes, len2) };
        assert_eq!(slice, b"hi");
    }

    #[test]
    fn alloc_empty_string() {
        sigil_gc_init();
        let obj = unsafe { sigil_string_new(std::ptr::null(), 0) };
        assert!(!obj.is_null());
        assert_eq!(unsafe { sigil_string_len(obj) }, 0);
    }
}
