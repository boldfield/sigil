//! Minimal FFI bindings for signal-driven sampling — plan
//! 2026-05-08-sigil-v2-runtime-profile-data Phase 3.
//!
//! The plan rules out adding the `libc` crate ("no new runtime
//! dependencies"), so the runtime declares the small surface it
//! needs (`sigaction`, `setitimer`, plus the platform-specific
//! ucontext-FP offset) directly. Struct layouts are pinned per
//! target and validated at compile time via `static_assertions`-
//! style `const _: () = assert!()` checks plus unit tests.
//!
//! Coverage: linux-x86_64 + macos-aarch64. Other targets compile-
//! error at the `compile_error!` site in `mod.rs`.
//!
//! ## Why `sigaction(2)` over `signal(2)`
//!
//! Original v1 used `signal(2)` for simplicity; the walker captured
//! from its own frame pointer at signal time, which put 2-3
//! trampoline / walker frames at the bottom of every CPU sample.
//! Renderers tolerated this but leaf attribution was off. PR #148
//! review (item #4) flagged this as a future-PR target.
//!
//! This module ships the migration: `sigaction(SIGPROF, ...)` with
//! `SA_SIGINFO + SA_RESTART`, an `extern "C"` handler that receives
//! the `ucontext_t*` of the interrupted thread, and platform-
//! specific `ucontext_fp(ucontext) -> usize` helpers that read the
//! saved frame pointer directly. The walker then starts from THAT
//! fp, so the first captured frame is the interrupted code's
//! caller — no trampoline noise.

#![allow(non_camel_case_types)]

use core::ffi::{c_int, c_void};

pub const SIGPROF: c_int = 27;

/// `ITIMER_PROF`: counts both user + system CPU time (vs ITIMER_VIRTUAL
/// which is user-only and ITIMER_REAL which is wall-clock). Profiling
/// CPU time is what we want.
pub const ITIMER_PROF: c_int = 2;

/// `SA_SIGINFO`: handler receives `siginfo_t*` and `ucontext_t*`.
#[cfg(target_os = "linux")]
pub const SA_SIGINFO: c_int = 0x00000004;
#[cfg(target_os = "macos")]
pub const SA_SIGINFO: c_int = 0x00000040;

/// `SA_RESTART`: automatically restart interrupted syscalls so the
/// signal doesn't surface as EINTR through every libc call in the
/// program.
#[cfg(target_os = "linux")]
pub const SA_RESTART: c_int = 0x10000000;
#[cfg(target_os = "macos")]
pub const SA_RESTART: c_int = 0x00000002;

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

// ===========================================================
// sigaction layouts — per-platform, pinned to glibc / dyld ABI.
// ===========================================================

/// `sigaction(2)` handler signature with `SA_SIGINFO`. The third
/// parameter is opaque (`*mut c_void`) because the `ucontext_t`
/// layout is platform-specific; consumers cast to the local
/// definition via [`ucontext_fp`].
pub type SigactionHandler = extern "C" fn(c_int, *mut c_void, *mut c_void);

/// Linux x86_64 `struct sigaction` (glibc).
///
/// Layout (per `bits/sigaction.h` on x86_64 / aarch64):
///
/// ```text
/// offset 0   :  sa_sigaction      (8 bytes function pointer; union with sa_handler)
/// offset 8   :  sa_mask           (128 bytes — `__sigset_t` is 16 × unsigned long)
/// offset 136 :  sa_flags          (4 bytes)
/// offset 140 :  (pad 4 bytes)
/// offset 144 :  sa_restorer       (8 bytes; legacy, set to NULL)
/// total      :  152 bytes
/// ```
///
/// Linux aarch64 has the same layout (`__sigset_t` is 16 × unsigned
/// long on both x86_64 and aarch64).
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct Sigaction {
    pub sa_sigaction: usize,
    pub sa_mask: [u64; 16],
    pub sa_flags: c_int,
    pub _pad: c_int,
    pub sa_restorer: usize,
}

// Manual `Default` (not derived) because clippy::derivable-impls
// fires on the obvious derive, but the array field `[u64; 16]` cuts
// across const-init paths in the rust 1.95 toolchain in a way that
// the derived `Default` doesn't inline as nicely. Keep it explicit
// so the zero-init bytes are obvious in review.
#[cfg(target_os = "linux")]
#[allow(clippy::derivable_impls)]
impl Default for Sigaction {
    fn default() -> Self {
        Self {
            sa_sigaction: 0,
            sa_mask: [0u64; 16],
            sa_flags: 0,
            _pad: 0,
            sa_restorer: 0,
        }
    }
}

/// macOS aarch64 `struct sigaction`. The Darwin layout omits
/// `sa_restorer` entirely and uses a 32-bit `sigset_t`. Total: 16
/// bytes (8 + 4 + 4).
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
pub struct Sigaction {
    pub sa_sigaction: usize,
    pub sa_mask: u32,
    pub sa_flags: c_int,
}

// Compile-time assertions on Sigaction size match the libc shape.
#[cfg(target_os = "linux")]
const _: () = assert!(core::mem::size_of::<Sigaction>() == 152);
#[cfg(target_os = "macos")]
const _: () = assert!(core::mem::size_of::<Sigaction>() == 16);

// ===========================================================
// ucontext FP extraction — platform-specific offsets.
// ===========================================================

/// Return the frame pointer (`%rbp` on x86_64, `x29` on aarch64) of
/// the thread that was interrupted by the signal. Caller supplies the
/// opaque `ucontext_t*` from the SA_SIGINFO handler signature.
///
/// **Linux x86_64.** `ucontext_t` layout (glibc):
///
/// ```text
/// offset 0   :  uc_flags         (8)
/// offset 8   :  uc_link          (8)
/// offset 16  :  uc_stack         (24)  // stack_t = ss_sp(8) + ss_flags(4) + pad(4) + ss_size(8)
/// offset 40  :  uc_mcontext      (256 bytes — gregs[23] × 8 + ...)
/// ...
/// ```
///
/// `gregs[23]` starts at ucontext offset 40. The greg index for RBP
/// is `REG_RBP = 10` (from `sys/ucontext.h`'s anonymous enum:
/// R8,R9,R10,R11,R12,R13,R14,R15,RDI,RSI,RBP,...). So:
///
/// ```text
/// rbp_offset_in_ucontext = 40 + 10 * 8 = 120
/// ```
///
/// **macOS aarch64.** `ucontext_t` is small (~56 bytes) with
/// `uc_mcontext` as a **pointer indirection** to `mcontext_t`. The
/// pointer is at ucontext offset 48 (after uc_onstack=4, uc_sigmask=4,
/// uc_stack=24, uc_link=8, uc_mcsize=8). The `mcontext_t` layout is:
///
/// ```text
/// offset 0   :  __es  (__darwin_arm_exception_state64, 16 bytes: __far + __esr + __exception)
/// offset 16  :  __ss  (__darwin_arm_thread_state64)
///                       __x[29]  : 232 bytes (x0..x28)
///                       __fp     : 8 bytes  <-- offset 16 + 232 = 248
///                       __lr     : 8
///                       __sp     : 8
///                       __pc     : 8
///                       __cpsr   : 4
///                       __pad    : 4
/// ```
///
/// # Safety
///
/// `ucontext` must be a valid pointer to the kernel-supplied
/// `ucontext_t` for the interrupted thread. Reading the saved fp is
/// signal-safe (no allocation, no syscall, single aligned load).
#[inline(always)]
pub unsafe fn ucontext_fp(ucontext: *mut c_void) -> usize {
    if ucontext.is_null() {
        return 0;
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // gregs are int64; REG_RBP is index 10 within gregs.
        const RBP_OFFSET: usize = 40 + 10 * 8;
        let p = (ucontext as *const u8).add(RBP_OFFSET) as *const usize;
        core::ptr::read_volatile(p)
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        // Linux aarch64 ucontext_t.uc_mcontext.regs[29] is the fp.
        // uc_mcontext on aarch64 starts at offset 184 (uc_flags=8 +
        // uc_link=8 + uc_stack=24 + sigset=128 + fault_address=8 +
        // pad=8 puts regs at...) — kernel's `struct sigcontext` is:
        //   fault_address (8) + regs[31] (248) + sp(8) + pc(8) + ...
        // and `mcontext_t` IS sigcontext on aarch64.
        // uc_mcontext begins at ucontext offset 176 (after uc_flags=8,
        // uc_link=8, uc_stack=24, uc_sigmask=128 ≈ 168 + 8 alignment).
        // regs[29] within sigcontext = fault_address(8) + 29*8 = 240.
        // ucontext->regs[29] = 176 + 240 = 416.
        const X29_OFFSET: usize = 176 + 8 + 29 * 8;
        let p = (ucontext as *const u8).add(X29_OFFSET) as *const usize;
        core::ptr::read_volatile(p)
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        // uc_mcontext is a *pointer* at offset 48; deref then read fp.
        const UC_MCONTEXT_PTR_OFFSET: usize = 48;
        const FP_WITHIN_MCONTEXT: usize = 16 + 29 * 8; // __es=16 then __x[29]
        let mctx_ptr_addr = (ucontext as *const u8).add(UC_MCONTEXT_PTR_OFFSET) as *const usize;
        let mctx = core::ptr::read_volatile(mctx_ptr_addr);
        if mctx == 0 {
            return 0;
        }
        let fp_addr = (mctx as *const u8).add(FP_WITHIN_MCONTEXT) as *const usize;
        core::ptr::read_volatile(fp_addr)
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        // macOS x86_64 mirrors aarch64's pointer-indirection design.
        // mcontext_t starts with __darwin_x86_exception_state64 (16),
        // then __darwin_x86_thread_state64 with rax..r15 + rip + rfl.
        // rbp is at __ss offset 5*8 = 40 (rax,rbx,rcx,rdx,rdi,rsi,rbp).
        // Actually __darwin_x86_thread_state64 order:
        //   rax(0), rbx(1), rcx(2), rdx(3), rdi(4), rsi(5), rbp(6),
        //   rsp(7), r8(8)..., rip(16), ...
        // So rbp offset within __ss = 6*8 = 48, plus __es=16 = 64.
        const UC_MCONTEXT_PTR_OFFSET: usize = 48;
        const RBP_WITHIN_MCONTEXT: usize = 16 + 6 * 8;
        let mctx_ptr_addr = (ucontext as *const u8).add(UC_MCONTEXT_PTR_OFFSET) as *const usize;
        let mctx = core::ptr::read_volatile(mctx_ptr_addr);
        if mctx == 0 {
            return 0;
        }
        let fp_addr = (mctx as *const u8).add(RBP_WITHIN_MCONTEXT) as *const usize;
        core::ptr::read_volatile(fp_addr)
    }
}

// ===========================================================
// extern bindings.
// ===========================================================

extern "C" {
    pub fn sigaction(signum: c_int, act: *const Sigaction, oldact: *mut Sigaction) -> c_int;
    pub fn setitimer(which: c_int, new_value: *const Itimerval, old_value: *mut Itimerval)
        -> c_int;
}

// ===========================================================
// dladdr — PC → symbol-name lookup for dyld-loaded libraries.
// ===========================================================

/// `Dl_info` as returned by `dladdr(3)`. Layout is identical on Linux
/// and macOS — both define it as four pointer-width fields with no
/// padding (see `<dlfcn.h>` on each platform).
///
/// Field order matches the POSIX-ish definition Linux and macOS share:
/// `dli_fname` (image path), `dli_fbase` (image load address),
/// `dli_sname` (nearest symbol name, may be NULL when no symbol is
/// close enough — Apple stripped dylibs hit this case), `dli_saddr`
/// (address of that symbol).
#[repr(C)]
pub struct DlInfo {
    pub dli_fname: *const core::ffi::c_char,
    pub dli_fbase: *mut c_void,
    pub dli_sname: *const core::ffi::c_char,
    pub dli_saddr: *mut c_void,
}

extern "C" {
    /// Resolve `addr` (a runtime PC) to the nearest symbol in any
    /// loaded image. Returns non-zero on success; on failure the `info`
    /// pointer is left untouched. On macOS, `dli_sname` is the closest
    /// **exported** symbol; on Linux (glibc), it's the closest
    /// symbol-table entry whose address is ≤ `addr`. In both cases
    /// stripped images yield `dli_sname == NULL`, which the caller
    /// must check before reading the name.
    ///
    /// `dladdr` is documented as thread-safe on both platforms but is
    /// **not** signal-safe (it allocates / locks internally on the
    /// macOS implementation). We only call it from flush-time writer
    /// code, which runs on the drainer thread or at atexit — outside
    /// the SIGPROF handler.
    pub fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn timeval_size_matches_platform_abi() {
        // Linux x86_64: timeval = i64 + i64 = 16
        // macOS aarch64: timeval = i64 + i32 + pad = 16
        assert_eq!(core::mem::size_of::<Timeval>(), 16);
        assert_eq!(core::mem::size_of::<Itimerval>(), 32);
    }

    #[test]
    fn sigaction_size_matches_platform_abi() {
        #[cfg(target_os = "linux")]
        assert_eq!(core::mem::size_of::<Sigaction>(), 152);
        #[cfg(target_os = "macos")]
        assert_eq!(core::mem::size_of::<Sigaction>(), 16);
    }

    #[test]
    fn ucontext_fp_returns_zero_for_null() {
        // SAFETY: explicit null is a documented short-circuit; the
        // function returns 0 before any read.
        let fp = unsafe { ucontext_fp(core::ptr::null_mut()) };
        assert_eq!(fp, 0);
    }

    #[test]
    fn dladdr_resolves_libc_function() {
        // Pass a libc function pointer through dladdr and assert we
        // get a non-NULL `dli_sname`. Uses `setitimer` (already FFI-
        // declared in this module) as the probe symbol so the test
        // doesn't depend on any further bindings.
        let probe = setitimer as *const core::ffi::c_void;
        let mut info = DlInfo {
            dli_fname: core::ptr::null(),
            dli_fbase: core::ptr::null_mut(),
            dli_sname: core::ptr::null(),
            dli_saddr: core::ptr::null_mut(),
        };
        // SAFETY: probe is a valid function pointer; info is a stack
        // value we own.
        let rc = unsafe { dladdr(probe, &mut info as *mut _) };
        assert!(rc != 0, "dladdr should succeed for a libc function ptr");
        assert!(
            !info.dli_sname.is_null(),
            "dli_sname should be non-NULL for a libc function ptr"
        );
        // SAFETY: dli_sname is non-NULL on success; libc owns the
        // string; reading is safe as long as we don't outlive the
        // process. The string is short and has a NUL terminator.
        let name = unsafe { core::ffi::CStr::from_ptr(info.dli_sname) }
            .to_string_lossy()
            .into_owned();
        assert!(
            name.contains("setitimer"),
            "expected `setitimer` in resolved name, got {name:?}"
        );
    }
}
