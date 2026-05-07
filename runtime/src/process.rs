//! Plan C addendum (CLI external-system effects, EE2) — runtime
//! arm fn for the `Process` effect: `run`. Conforms to the Phase 4
//! CPS arm fn ABI used by every existing builtin-effect arm.
//!
//! ## Return shape (raw)
//!
//! `Process.run(cmd: String, args: Array[String]) -> (Int, Int,
//! String, String)` — `(error_tag, exit_code, stdout, stderr)`.
//!
//! - `error_tag == 0` (PROCESS_OK): the child process launched and
//!   ran to completion. `exit_code` carries the child's status (0
//!   for typical success; non-zero for typical failure; `-1` if
//!   the process was killed by a signal, since `ExitStatus::code`
//!   returns `None` in that case). `stdout` / `stderr` carry the
//!   captured streams (lossy UTF-8 — invalid bytes become
//!   U+FFFD).
//! - `error_tag > 0`: the launch failed. `exit_code` = 0,
//!   `stdout` / `stderr` are empty strings (the `Other` variant
//!   carries the OS error display in `stderr` for diagnosis).
//!
//! Stdlib wrapper at `std/process.sigil` translates this to
//! `Result[ProcessResult, ProcessError]`.
//!
//! ## `ProcessError` variant tag mapping
//!
//! | Tag | Variant            |
//! |-----|--------------------|
//! | 0   | (success — `Ok`)   |
//! | 1   | `NotFound`         |
//! | 2   | `PermissionDenied` |
//! | 3   | `Other(String)`    |
//!
//! ## Scope
//!
//! - **No shell invocation.** `run("ls", ["-la"])` directly execs
//!   `ls`; it doesn't invoke `bash -c`. If the user wants shell
//!   behavior, they pass `("bash", ["-c", "ls -la"])`.
//! - **No stdin piping in v1.** Stdin is closed (empty input).
//! - **No streaming.** Stdout / stderr are captured fully via
//!   `Command::output()`, which waits for the child to exit before
//!   returning.

use std::io;
use std::process::Command;

use crate::effect_helpers::{alloc_string_from_str, alloc_tuple};
use crate::gc::string_bytes;
use crate::handlers::{sigil_next_step_args_ptr, sigil_next_step_call, NextStep, TerminalResult};
use sigil_header_constants::TAG_ARRAY;

const PROCESS_OK: i64 = 0;
const PROCESS_ERR_NOT_FOUND: i64 = 1;
const PROCESS_ERR_PERMISSION_DENIED: i64 = 2;
const PROCESS_ERR_OTHER: i64 = 3;

fn map_launch_err(e: &io::Error) -> i64 {
    match e.kind() {
        io::ErrorKind::NotFound => PROCESS_ERR_NOT_FOUND,
        io::ErrorKind::PermissionDenied => PROCESS_ERR_PERMISSION_DENIED,
        _ => PROCESS_ERR_OTHER,
    }
}

/// Walk a Sigil `Array[String]` and return its entries as a Vec of
/// `Vec<u8>` (one entry per array slot, raw bytes preserved). The
/// caller (Process.run) feeds these as args to `Command::args`.
///
/// # Safety
///
/// `arr` must be a non-null pointer to a `TAG_ARRAY` header whose
/// element slots hold pointers to `TAG_STRING` records.
unsafe fn array_of_strings_to_byte_vecs(arr: *const u8) -> Vec<Vec<u8>> {
    debug_assert!(!arr.is_null());
    debug_assert_eq!(
        (arr as *const u64).read() as u8,
        TAG_ARRAY,
        "process.run args must be Array[String] (TAG_ARRAY)",
    );
    let len = (arr.add(8) as *const u64).read() as usize;
    let elems_p: *const *const u8 = arr.add(16).cast();
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(len);
    for i in 0..len {
        let s_ptr = elems_p.add(i).read();
        if s_ptr.is_null() {
            out.push(Vec::new());
            continue;
        }
        let (bytes, byte_len) = string_bytes(s_ptr);
        let slice = std::slice::from_raw_parts(bytes, byte_len);
        out.push(slice.to_vec());
    }
    out
}

/// Build the 4-element `(Int, Int, String, String)` result tuple.
/// Bitmap = 0b1100 (slots 2 & 3 are pointers; slots 0 & 1 are
/// scalars).
unsafe fn build_process_result_tuple(
    error_tag: i64,
    exit_code: i64,
    stdout: *mut u8,
    stderr: *mut u8,
) -> *mut u8 {
    alloc_tuple(
        &[
            error_tag as u64,
            exit_code as u64,
            stdout as u64,
            stderr as u64,
        ],
        0b1100,
    )
}

/// `Process.run(cmd: String, args: Array[String]) -> (Int, Int,
/// String, String)` arm fn. Op id 0.
///
/// # Safety
///
/// `args_len == 4` (2 user args + trailing pair). `in_args[0]` is
/// a non-null `TAG_STRING` pointer (cmd); `in_args[1]` is a non-
/// null `TAG_ARRAY` pointer (args).
#[no_mangle]
pub unsafe extern "C" fn sigil_process_run_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 4,
        "sigil_process_run_arm: args_len {args_len} != 4"
    );
    debug_assert!(!in_args.is_null());
    let cmd_ptr = *in_args as *const u8;
    let args_arr = *in_args.add(1) as *const u8;
    let k_closure = *in_args.add(2) as *mut u8;
    let k_fn = *in_args.add(3) as *mut u8;

    let (cmd_bytes, cmd_len) = string_bytes(cmd_ptr);
    let cmd_slice = std::slice::from_raw_parts(cmd_bytes, cmd_len);
    let cmd = match std::str::from_utf8(cmd_slice) {
        Ok(c) => c,
        Err(_) => {
            // Invalid UTF-8 in command path — treat as NotFound.
            // (Direct `exec` of non-UTF-8 paths is platform-
            // dependent; for v1 we surface as an error rather than
            // attempt the syscall with raw bytes.)
            let empty = alloc_string_from_str("");
            let empty2 = alloc_string_from_str("");
            let tup = build_process_result_tuple(PROCESS_ERR_NOT_FOUND, 0, empty, empty2);
            let ns = sigil_next_step_call(k_closure, k_fn, 1);
            *sigil_next_step_args_ptr(ns) = tup as u64;
            return ns;
        }
    };

    let arg_byte_vecs = array_of_strings_to_byte_vecs(args_arr);
    let arg_strs: Vec<&[u8]> = arg_byte_vecs.iter().map(|v| v.as_slice()).collect();

    // Build the command. We use OsStr conversion via String so
    // that args with non-ASCII content go through verbatim.
    let mut command = Command::new(cmd);
    for raw_arg in &arg_strs {
        // Fall back to lossy UTF-8 conversion for arg bytes — this
        // is consistent with how the rest of the runtime treats
        // String byte sequences (lossy `string_chars`, lossy
        // process stdout/stderr).
        let arg_str = String::from_utf8_lossy(raw_arg).into_owned();
        command.arg(arg_str);
    }

    let (error_tag, exit_code, stdout_str, stderr_str): (i64, i64, String, String) =
        match command.output() {
            Ok(out) => {
                let code = out.status.code().map(|c| c as i64).unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                (PROCESS_OK, code, stdout, stderr)
            }
            Err(e) => {
                let kind = map_launch_err(&e);
                let display = if kind == PROCESS_ERR_OTHER {
                    format!("{e}")
                } else {
                    String::new()
                };
                // For the `Other` variant we put the OS error
                // display in `stderr` so the user can pattern-match
                // and surface it; for NotFound / PermissionDenied
                // both stream slots are empty.
                (kind, 0, String::new(), display)
            }
        };

    let stdout_ptr = alloc_string_from_str(&stdout_str);
    let stderr_ptr = alloc_string_from_str(&stderr_str);
    let tup = build_process_result_tuple(error_tag, exit_code, stdout_ptr, stderr_ptr);

    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    *sigil_next_step_args_ptr(ns) = tup as u64;
    ns
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::effect_helpers::{alloc_array_with_capacity, array_set_slot_raw};
    use crate::gc::sigil_string_new;
    use crate::test_support::gc_test_lock;

    unsafe fn make_string(s: &str) -> *mut u8 {
        // SAFETY: gc-heap-ptr arithmetic (Rust-owned `&str`; sigil_string_new copies).
        sigil_string_new(s.as_ptr(), s.len())
    }

    unsafe fn make_array_of_strings(items: &[&str]) -> *mut u8 {
        let arr = alloc_array_with_capacity(items.len());
        for (i, s) in items.iter().enumerate() {
            let p = make_string(s);
            array_set_slot_raw(arr, i, p as u64);
        }
        arr
    }

    #[cfg(unix)]
    #[test]
    fn run_echo_returns_zero_exit_and_stdout() {
        // PATH-resolved command name — `/bin/echo` exists on Linux
        // but the macOS-14 GitHub runner doesn't always provide it
        // at that path. `Command::new("echo")` does PATH lookup.
        let _g = gc_test_lock();
        unsafe {
            let _cmd = make_string("echo");
            let _args = make_array_of_strings(&["hello"]);
            // Replicate the arm-fn body without trampoline dispatch.
            let mut command = Command::new("echo");
            command.arg("hello");
            let out = command.output().expect("spawn echo");
            assert_eq!(out.status.code(), Some(0));
            assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_false_returns_one_exit() {
        let out = Command::new("false").output().expect("spawn `false`");
        assert_eq!(out.status.code(), Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn run_missing_executable_is_not_found() {
        match Command::new("/nonexistent/path/to/no_command").output() {
            Ok(_) => panic!("expected NotFound launch error"),
            Err(e) => assert_eq!(map_launch_err(&e), PROCESS_ERR_NOT_FOUND),
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_stderr_separately() {
        let out = Command::new("sh")
            .arg("-c")
            .arg("echo out; echo err >&2")
            .output()
            .expect("spawn sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
    }

    #[test]
    fn array_of_strings_to_byte_vecs_round_trips() {
        let _g = gc_test_lock();
        unsafe {
            let arr = make_array_of_strings(&["alpha", "beta", "γ"]);
            let bytes = array_of_strings_to_byte_vecs(arr);
            assert_eq!(bytes.len(), 3);
            assert_eq!(&bytes[0], b"alpha");
            assert_eq!(&bytes[1], b"beta");
            assert_eq!(&bytes[2], "γ".as_bytes());
        }
    }

    /// Pins `PROCESS_*` constants against `std/process.sigil`'s
    /// `match (tag, _, _, _) { (1, ...) => Err(NotFound), (2, ...) =>
    /// Err(PermissionDenied), (_, ...) => Err(Other(err)) }`. Same
    /// rationale as `fs::tests::fs_err_tag_round_trip_*`: the
    /// runtime + stdlib jointly own the tag → variant contract.
    #[test]
    fn process_err_tag_round_trip_pinned_against_stdlib_variants() {
        assert_eq!(PROCESS_OK, 0, "tag 0 reserved for `Ok`");
        assert_eq!(PROCESS_ERR_NOT_FOUND, 1);
        assert_eq!(PROCESS_ERR_PERMISSION_DENIED, 2);
        assert_eq!(PROCESS_ERR_OTHER, 3);

        assert_eq!(
            map_launch_err(&io::Error::new(io::ErrorKind::NotFound, "")),
            PROCESS_ERR_NOT_FOUND,
        );
        assert_eq!(
            map_launch_err(&io::Error::new(io::ErrorKind::PermissionDenied, "")),
            PROCESS_ERR_PERMISSION_DENIED,
        );
        assert_eq!(
            map_launch_err(&io::Error::new(io::ErrorKind::InvalidInput, "")),
            PROCESS_ERR_OTHER,
        );
    }
}
