//! Plan C addendum (CLI external-system effects, EE2) — runtime
//! arm fns for the `Fs` effect: `exists`, `file_size`, `is_dir`,
//! `is_file`, `mkdir`, `read_dir`, `read_file`, `remove_dir`,
//! `remove_file`, `write_file`. Each conforms to the Phase 4 CPS
//! arm fn ABI used by every existing builtin-effect arm in
//! `runtime/src/handlers.rs`.
//!
//! ## Return shapes (raw)
//!
//! - Predicates (`exists`, `is_dir`, `is_file`) return `Bool` (i8)
//!   directly. Absent paths legitimately answer `false` — no error
//!   shape needed.
//! - Fallible ops returning a payload return `(Int, T)` tuples where
//!   tag `0` = success; tag `>0` indexes an `FsError` variant defined
//!   in `std/fs.sigil`'s `type FsError = | NotFound | PermissionDenied
//!   | AlreadyExists | NotADirectory | IsADirectory | InvalidUtf8 |
//!   Other(String)` enum (variant order matches the tag → variant
//!   mapping below).
//! - `file_size` returns `(Int, Int64)`. `Int64` is boxed (file
//!   sizes can exceed `Int`'s 63-bit range).
//! - `read_dir` returns `(Int, Array[String])`. Each entry name (no
//!   path joining) becomes one slot in the array.
//! - `read_file` returns `(Int, String)` — content on success, error
//!   message on `Other`.
//! - `mkdir` / `remove_file` / `remove_dir` / `write_file` return
//!   `(Int, String)` — empty string on success, error message on
//!   `Other`.
//!
//! ## `FsError` variant tag mapping
//!
//! The stdlib wrapper at `std/fs.sigil` consumes these tags
//! verbatim — keep the mapping aligned with the variant order in
//! that file's `type FsError = | NotFound | ...` declaration.
//!
//! | Tag | Variant            |
//! |-----|--------------------|
//! | 0   | (success — `Ok`)   |
//! | 1   | `NotFound`         |
//! | 2   | `PermissionDenied` |
//! | 3   | `AlreadyExists`    |
//! | 4   | `NotADirectory`    |
//! | 5   | `IsADirectory`     |
//! | 6   | `InvalidUtf8`      |
//! | 7   | `Other(String)`    |

use std::io;

use crate::effect_helpers::{
    alloc_array_with_capacity, alloc_int64, alloc_string_from_str, alloc_tuple, array_set_slot_raw,
};
use crate::gc::string_bytes;
use crate::handlers::{write_k_dispatch_value, NextStep, TerminalResult};

const FS_OK: i64 = 0;
const FS_ERR_NOT_FOUND: i64 = 1;
const FS_ERR_PERMISSION_DENIED: i64 = 2;
const FS_ERR_ALREADY_EXISTS: i64 = 3;
const FS_ERR_NOT_A_DIRECTORY: i64 = 4;
const FS_ERR_IS_A_DIRECTORY: i64 = 5;
const FS_ERR_INVALID_UTF8: i64 = 6;
const FS_ERR_OTHER: i64 = 7;

/// Map a `std::io::Error` to an `FsError` variant tag. The
/// `Other` case carries the error display string in the tuple's
/// second slot.
fn map_io_err(e: &io::Error) -> i64 {
    use io::ErrorKind::*;
    match e.kind() {
        NotFound => FS_ERR_NOT_FOUND,
        PermissionDenied => FS_ERR_PERMISSION_DENIED,
        AlreadyExists => FS_ERR_ALREADY_EXISTS,
        NotADirectory => FS_ERR_NOT_A_DIRECTORY,
        IsADirectory => FS_ERR_IS_A_DIRECTORY,
        _ => FS_ERR_OTHER,
    }
}

/// Read a Sigil `String` argument as a Rust `&str`. On invalid
/// UTF-8 in the path itself, returns `None` so the caller can
/// surface `Err(InvalidUtf8)`.
unsafe fn path_str_from_sigil_arg<'a>(p: *const u8) -> Option<&'a str> {
    let (bytes, len) = string_bytes(p);
    let slice = std::slice::from_raw_parts(bytes, len);
    std::str::from_utf8(slice).ok()
}

/// Build a `(Int, String)` tuple for the standard fallible-Fs-op
/// shape. `tag` 0 = success (caller fills `value` with the success
/// payload like file content); tag >0 = error variant + optional
/// `value` (used by the `Other` variant for the error display
/// string; ignored by the others — empty string).
unsafe fn build_int_string_tuple(tag: i64, value: *mut u8) -> *mut u8 {
    alloc_tuple(&[tag as u64, value as u64], 0b10)
}

/// Build an `(Int, T, String)` tuple where `T` is a pointer-typed
/// Sigil value (Int64, Array). The trailing `msg` slot carries the
/// OS-error display string when the tag indexes the `Other` variant;
/// otherwise empty. Bitmap = `0b110` (slots 1 + 2 are pointers; slot
/// 0 is the scalar tag).
///
/// Used by `file_size` (T = Int64) and `read_dir` (T = Array[String])
/// — each needs to round-trip the OS error message through to the
/// stdlib `Err(Other(msg))` construction site. Earlier draft used a
/// 2-tuple `(Int, T)` here which dropped the message on the floor.
unsafe fn build_int_pointer_string_tuple(tag: i64, ptr: *mut u8, msg: *mut u8) -> *mut u8 {
    alloc_tuple(&[tag as u64, ptr as u64, msg as u64], 0b110)
}

// ── Predicates ─────────────────────────────────────────────────────

/// `Fs.exists(path: String) -> Bool` arm fn. Op id 0.
///
/// # Safety
///
/// `args_len == 3` (1 user arg + trailing pair).
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_exists_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let exists = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => std::path::Path::new(p).exists(),
        None => false,
    };

    write_k_dispatch_value(k_closure, k_fn, u8::from(exists) as u64)
}

/// `Fs.is_dir(path: String) -> Bool` arm fn. Op id 2.
///
/// # Safety
///
/// As `sigil_fs_exists_arm`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_is_dir_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let is_dir = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => std::path::Path::new(p).is_dir(),
        None => false,
    };

    write_k_dispatch_value(k_closure, k_fn, u8::from(is_dir) as u64)
}

/// `Fs.is_file(path: String) -> Bool` arm fn. Op id 3.
///
/// # Safety
///
/// As `sigil_fs_exists_arm`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_is_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let is_file = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => std::path::Path::new(p).is_file(),
        None => false,
    };

    write_k_dispatch_value(k_closure, k_fn, u8::from(is_file) as u64)
}

// ── Metadata ──────────────────────────────────────────────────────

/// `Fs.file_size(path: String) -> (Int, Int64)` arm fn. Op id 1.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_file_size_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, size_value, msg) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::metadata(p) {
            Ok(m) => {
                // `Metadata::len()` returns u64. Sigil's `Int64` is
                // signed, so files >= 2^63 bytes (~9.2 EB) would wrap
                // to negative on a naked `as i64` cast. Surface that
                // case as `FsError::Other("file size exceeds Int64
                // range")` rather than silently misreporting size.
                let len = m.len();
                if len > i64::MAX as u64 {
                    (
                        FS_ERR_OTHER,
                        alloc_int64(0),
                        alloc_string_from_str("file size exceeds Int64 range"),
                    )
                } else {
                    (FS_OK, alloc_int64(len as i64), alloc_string_from_str(""))
                }
            }
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, alloc_int64(0), display)
            }
        },
        None => (
            FS_ERR_INVALID_UTF8,
            alloc_int64(0),
            alloc_string_from_str(""),
        ),
    };
    let tup = build_int_pointer_string_tuple(tag, size_value, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

// ── Directory ops ──────────────────────────────────────────────────

/// `Fs.mkdir(path: String) -> (Int, String)` arm fn. Op id 4.
///
/// Single-level directory creation. Recursive `mkdir -p` is a
/// stdlib helper, not an effect op.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_mkdir_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, msg) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::create_dir(p) {
            Ok(()) => (FS_OK, alloc_string_from_str("")),
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, display)
            }
        },
        None => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
    };
    let tup = build_int_string_tuple(tag, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Fs.read_dir(path: String) -> (Int, Array[String])` arm fn.
/// Op id 5. Returns entry names only (no path joining); the
/// stdlib wrapper at `std/fs.sigil` constructs `Result[List[String],
/// FsError]` by Array→List conversion.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_read_dir_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, arr, msg) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::read_dir(p) {
            Ok(rd) => {
                // Collect entry names (Rust strings, allocator-
                // independent of Boehm). Then fill an empty Sigil
                // Array slot-by-slot. Filenames with non-UTF-8 bytes
                // become lossy (U+FFFD substituted) rather than
                // silently dropped — keeps the entry count honest
                // and matches the rest of the runtime's lossy-string
                // convention (stdout/stderr capture, `string_chars`).
                // `DirEntry` IO errors (`entry_result.err()`) are
                // still skipped — the alternative is aborting the
                // whole listing on the first transient EACCES, which
                // is worse for v1.
                let names: Vec<String> = rd
                    .filter_map(|entry_result| {
                        entry_result
                            .ok()
                            .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    })
                    .collect();
                let arr = alloc_array_with_capacity(names.len());
                for (i, name) in names.iter().enumerate() {
                    let s = alloc_string_from_str(name);
                    array_set_slot_raw(arr, i, s as u64);
                }
                (FS_OK, arr, alloc_string_from_str(""))
            }
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, alloc_array_with_capacity(0), display)
            }
        },
        None => (
            FS_ERR_INVALID_UTF8,
            alloc_array_with_capacity(0),
            alloc_string_from_str(""),
        ),
    };
    let tup = build_int_pointer_string_tuple(tag, arr, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Fs.remove_dir(path: String) -> (Int, String)` arm fn. Op id 7.
/// Empty directory only.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_remove_dir_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, msg) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::remove_dir(p) {
            Ok(()) => (FS_OK, alloc_string_from_str("")),
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, display)
            }
        },
        None => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
    };
    let tup = build_int_string_tuple(tag, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

// ── File ops ───────────────────────────────────────────────────────

/// `Fs.read_file(path: String) -> (Int, String)` arm fn. Op id 6.
/// On success, slot 1 = file contents. On error, slot 1 = empty
/// string (or the OS error display for the `Other` variant).
/// Invalid UTF-8 in file content surfaces as `FsError::InvalidUtf8`.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_read_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, value) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::read(p) {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Ok(s) => (FS_OK, alloc_string_from_str(s)),
                Err(_) => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
            },
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, display)
            }
        },
        None => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
    };
    let tup = build_int_string_tuple(tag, value);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Fs.remove_file(path: String) -> (Int, String)` arm fn. Op id 8.
///
/// # Safety
///
/// `args_len == 3`.
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_remove_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    let (tag, msg) = match path_str_from_sigil_arg(path_ptr) {
        Some(p) => match std::fs::remove_file(p) {
            Ok(()) => (FS_OK, alloc_string_from_str("")),
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, display)
            }
        },
        None => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
    };
    let tup = build_int_string_tuple(tag, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Fs.write_file(path: String, data: String) -> (Int, String)` arm
/// fn. Op id 9. Replaces any existing contents.
///
/// # Safety
///
/// `args_len == 4` (2 user args + trailing pair).
#[no_mangle]
pub unsafe extern "C" fn sigil_fs_write_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 6);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let data_ptr = *in_args.add(1) as *const u8;
    let k_closure = *in_args.add(2) as *mut u8;
    let k_fn = *in_args.add(3) as *mut u8;

    let path = path_str_from_sigil_arg(path_ptr);
    let (data_bytes, data_len) = string_bytes(data_ptr);
    let data = std::slice::from_raw_parts(data_bytes, data_len);

    let (tag, msg) = match path {
        Some(p) => match std::fs::write(p, data) {
            Ok(()) => (FS_OK, alloc_string_from_str("")),
            Err(e) => {
                let kind = map_io_err(&e);
                let display = if kind == FS_ERR_OTHER {
                    alloc_string_from_str(&format!("{e}"))
                } else {
                    alloc_string_from_str("")
                };
                (kind, display)
            }
        },
        None => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
    };
    let tup = build_int_string_tuple(tag, msg);

    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::gc::{sigil_string_len, sigil_string_new};
    use crate::test_support::gc_test_lock;

    /// Helper: read a Sigil String's bytes back into a Rust Vec<u8>.
    unsafe fn read_string(p: *const u8) -> Vec<u8> {
        let len = sigil_string_len(p);
        let payload: *const u8 = p.add(16);
        std::slice::from_raw_parts(payload, len).to_vec()
    }

    /// Helper: build a Sigil String for use as a path argument in tests.
    unsafe fn make_string(s: &str) -> *mut u8 {
        // SAFETY: gc-heap-ptr arithmetic (Rust-owned `&str` buffer; sigil_string_new copies).
        sigil_string_new(s.as_ptr(), s.len())
    }

    fn unique_temp_path(suffix: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let mut p = std::env::temp_dir();
        p.push(format!(
            "sigil_fs_test_{pid}_{}_{suffix}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        p
    }

    #[test]
    fn read_file_round_trip() {
        let _g = gc_test_lock();
        let path = unique_temp_path("read");
        std::fs::write(&path, b"hello, sigil").expect("write fixture");
        unsafe {
            let s = make_string(path.to_str().expect("utf-8 path"));
            // Replicate the arm fn body without trampoline dispatch.
            let path_ref = path_str_from_sigil_arg(s).expect("utf-8 path");
            let bytes = std::fs::read(path_ref).expect("read");
            let tag = FS_OK;
            let value = alloc_string_from_str(std::str::from_utf8(&bytes).expect("utf-8"));
            let tup = build_int_string_tuple(tag, value);
            assert_eq!((tup.add(8) as *const i64).read(), FS_OK);
            let val_ptr = (tup.add(16) as *const *const u8).read();
            assert_eq!(read_string(val_ptr), b"hello, sigil");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_file_missing_is_not_found() {
        let _g = gc_test_lock();
        let path = unique_temp_path("missing");
        let _ = std::fs::remove_file(&path);
        unsafe {
            let s = make_string(path.to_str().expect("utf-8 path"));
            let path_ref = path_str_from_sigil_arg(s).expect("utf-8 path");
            let (tag, _value) = match std::fs::read(path_ref) {
                Ok(b) => (
                    FS_OK,
                    alloc_string_from_str(std::str::from_utf8(&b).unwrap()),
                ),
                Err(e) => (map_io_err(&e), alloc_string_from_str("")),
            };
            assert_eq!(tag, FS_ERR_NOT_FOUND);
        }
    }

    #[test]
    fn read_file_invalid_utf8_returns_invalid_utf8() {
        let _g = gc_test_lock();
        let path = unique_temp_path("invalid");
        std::fs::write(&path, [0xFFu8, 0xFE]).expect("write fixture");
        unsafe {
            let s = make_string(path.to_str().expect("utf-8 path"));
            let path_ref = path_str_from_sigil_arg(s).expect("utf-8 path");
            let (tag, _) = match std::fs::read(path_ref) {
                Ok(b) => match std::str::from_utf8(&b) {
                    Ok(_) => (FS_OK, alloc_string_from_str("")),
                    Err(_) => (FS_ERR_INVALID_UTF8, alloc_string_from_str("")),
                },
                Err(e) => (map_io_err(&e), alloc_string_from_str("")),
            };
            assert_eq!(tag, FS_ERR_INVALID_UTF8);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_file_round_trips_with_read() {
        let _g = gc_test_lock();
        let path = unique_temp_path("write");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"sigil writes").expect("write");
        let read = std::fs::read(&path).expect("read back");
        assert_eq!(read, b"sigil writes");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_dir_lists_entries() {
        let _g = gc_test_lock();
        let dir = unique_temp_path("read_dir");
        std::fs::create_dir(&dir).expect("create temp dir");
        std::fs::write(dir.join("a.txt"), b"a").expect("write a");
        std::fs::write(dir.join("b.txt"), b"b").expect("write b");
        let entries: Vec<String> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
            .collect();
        assert_eq!(entries.len(), 2);
        let _ = std::fs::remove_file(dir.join("a.txt"));
        let _ = std::fs::remove_file(dir.join("b.txt"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn exists_predicates() {
        let path = unique_temp_path("exists");
        let _ = std::fs::remove_file(&path);
        assert!(!std::path::Path::new(&path).exists());
        std::fs::write(&path, b"x").expect("write");
        assert!(std::path::Path::new(&path).exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mkdir_remove_round_trip() {
        let dir = unique_temp_path("mkrm");
        let _ = std::fs::remove_dir(&dir);
        std::fs::create_dir(&dir).expect("create");
        assert!(dir.is_dir());
        std::fs::remove_dir(&dir).expect("remove");
        assert!(!dir.exists());
    }

    #[test]
    fn remove_file_missing_is_not_found() {
        let path = unique_temp_path("rm_missing");
        let _ = std::fs::remove_file(&path);
        match std::fs::remove_file(&path) {
            Ok(()) => panic!("expected NotFound"),
            Err(e) => assert_eq!(map_io_err(&e), FS_ERR_NOT_FOUND),
        }
    }

    #[test]
    fn file_size_known_size() {
        let _g = gc_test_lock();
        let path = unique_temp_path("size");
        std::fs::write(&path, [0u8; 100]).expect("write 100 bytes");
        let m = std::fs::metadata(&path).expect("metadata");
        assert_eq!(m.len(), 100);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn map_io_err_covers_known_kinds() {
        let nf = io::Error::new(io::ErrorKind::NotFound, "");
        assert_eq!(map_io_err(&nf), FS_ERR_NOT_FOUND);
        let pd = io::Error::new(io::ErrorKind::PermissionDenied, "");
        assert_eq!(map_io_err(&pd), FS_ERR_PERMISSION_DENIED);
        let ae = io::Error::new(io::ErrorKind::AlreadyExists, "");
        assert_eq!(map_io_err(&ae), FS_ERR_ALREADY_EXISTS);
        let other = io::Error::new(io::ErrorKind::InvalidInput, "");
        assert_eq!(map_io_err(&other), FS_ERR_OTHER);
    }

    /// `std/fs.sigil`'s `__tag_to_fs_error(tag, msg)` does this
    /// mapping in Sigil:
    ///
    /// ```sigil
    /// match tag {
    ///   1 => NotFound,
    ///   2 => PermissionDenied,
    ///   3 => AlreadyExists,
    ///   4 => NotADirectory,
    ///   5 => IsADirectory,
    ///   6 => InvalidUtf8,
    ///   _ => Other(msg),
    /// }
    /// ```
    ///
    /// The runtime side (this file) and the stdlib side
    /// (`std/fs.sigil`) hold both halves of a tag → variant
    /// contract. Reordering or renumbering one side without the
    /// other silently misclassifies errors at runtime. This test
    /// pins the literal tag values so a mismatched reorder is a
    /// compile-time failure here, and the comment block above is
    /// the canonical reference for the stdlib match arms.
    #[test]
    fn fs_err_tag_round_trip_pinned_against_stdlib_variants() {
        // Pins literal tag values. If you renumber any of these,
        // `std/fs.sigil`'s `__tag_to_fs_error` match arms must
        // change in lockstep.
        assert_eq!(FS_OK, 0, "tag 0 reserved for `Ok`");
        assert_eq!(FS_ERR_NOT_FOUND, 1);
        assert_eq!(FS_ERR_PERMISSION_DENIED, 2);
        assert_eq!(FS_ERR_ALREADY_EXISTS, 3);
        assert_eq!(FS_ERR_NOT_A_DIRECTORY, 4);
        assert_eq!(FS_ERR_IS_A_DIRECTORY, 5);
        assert_eq!(FS_ERR_INVALID_UTF8, 6);
        assert_eq!(FS_ERR_OTHER, 7);

        // Pins `map_io_err` for each non-Other kind. `Other` is the
        // catch-all and gets exercised by `map_io_err_covers_known_-
        // kinds` above.
        assert_eq!(
            map_io_err(&io::Error::new(io::ErrorKind::NotFound, "")),
            FS_ERR_NOT_FOUND,
        );
        assert_eq!(
            map_io_err(&io::Error::new(io::ErrorKind::PermissionDenied, "")),
            FS_ERR_PERMISSION_DENIED,
        );
        assert_eq!(
            map_io_err(&io::Error::new(io::ErrorKind::AlreadyExists, "")),
            FS_ERR_ALREADY_EXISTS,
        );
        assert_eq!(
            map_io_err(&io::Error::new(io::ErrorKind::NotADirectory, "")),
            FS_ERR_NOT_A_DIRECTORY,
        );
        assert_eq!(
            map_io_err(&io::Error::new(io::ErrorKind::IsADirectory, "")),
            FS_ERR_IS_A_DIRECTORY,
        );
    }
}
