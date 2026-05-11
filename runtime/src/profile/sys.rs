//! Minimal FFI bindings for signal-driven sampling — plan
//! 2026-05-08-sigil-v2-runtime-profile-data Phase 3.
//!
//! The plan rules out adding the `libc` crate ("no new runtime
//! dependencies"), so the runtime declares the small surface it
//! needs (signal, setitimer, clock_gettime) directly. Struct
//! layouts are pinned per target.
//!
//! Coverage: linux-x86_64 + macos-aarch64. Other targets compile-
//! error at the `compile_error!` site in `mod.rs`.

#![allow(non_camel_case_types)]

use core::ffi::c_int;

pub const SIGPROF: c_int = 27;

/// `ITIMER_PROF`: counts both user + system CPU time (vs ITIMER_VIRTUAL
/// which is user-only and ITIMER_REAL which is wall-clock). Profiling
/// CPU time is what we want.
pub const ITIMER_PROF: c_int = 2;

/// Sentinel `SIG_ERR` return value from `signal(2)`. Treat any
/// `*mut c_void` whose `as usize == usize::MAX` as failure.
pub const SIG_ERR: usize = usize::MAX;

/// Linux x86_64 layout: `time_t` and `suseconds_t` are both 64-bit
/// signed integers, so the struct is 16 bytes with no padding.
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// macOS aarch64 layout: `time_t` is 64-bit, `suseconds_t` is 32-bit
/// (`__darwin_suseconds_t = __int32_t`). The struct ends after the
/// 4-byte field; the C ABI doesn't pad-out trailing fields, but two
/// of these inside an `itimerval` are aligned individually, so we
/// model the struct as exactly the layout C uses.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i32,
    /// Padding to align the next field in `Itimerval` to 8 bytes.
    /// C compilers insert this implicitly; we make it explicit.
    pub _pad: i32,
}

#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Itimerval {
    pub it_interval: Timeval,
    pub it_value: Timeval,
}

/// `signal(2)` handler type. Used in lieu of `sigaction(2)` for
/// simplicity — v1 doesn't read `siginfo_t` or `ucontext_t`. The
/// walker captures from its own frame pointer instead, accepting
/// that the first few captured frames are signal-trampoline /
/// runtime-internal frames that downstream renderers display as
/// `??` or as runtime symbol names.
pub type SignalHandler = extern "C" fn(c_int);

// `signal(2)`: install a simple handler. Returns the previous
// handler (or `SIG_ERR` cast through `*mut c_void`).
extern "C" {
    pub fn signal(signum: c_int, handler: SignalHandler) -> *mut core::ffi::c_void;
    pub fn setitimer(which: c_int, new_value: *const Itimerval, old_value: *mut Itimerval)
        -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeval_size_matches_platform_abi() {
        // Linux x86_64: timeval = i64 + i64 = 16
        // macOS aarch64: timeval = i64 + i32 + pad = 16
        assert_eq!(core::mem::size_of::<Timeval>(), 16);
        assert_eq!(core::mem::size_of::<Itimerval>(), 32);
    }
}
