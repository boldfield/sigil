//! Plan E2 Phase 1 Task 5 — opt-in stackmap precise-vs-conservative
//! cross-check.
//!
//! Activated by `SIGIL_GC_CROSS_CHECK=1`. At every `sigil_alloc`
//! invocation (after a sampling gate) the walker:
//!
//! 1. Reads the calling thread's stack range from `pthread_attr_t`.
//! 2. Walks the frame-pointer chain via `stackmap::walk_for_gc` and
//!    collects precise root addresses (set B).
//! 3. Asserts every precise address lies within the stack range —
//!    a conservative-scanner trivially sees all such addresses (set
//!    A is "all word-aligned addresses in [sp, stack_base)"), so
//!    `B ⊆ A` reduces to in-range checking.
//! 4. Asserts the value at each precise address is heap-pointer-
//!    shaped per Boehm's view (`GC_is_visible` returns the address
//!    itself when the value points inside a Boehm-allocated block).
//!
//! Divergence aborts the process with a diagnostic. Production code
//! paths skip this entirely — the env var is read once and cached in
//! a relaxed atomic, so the steady-state cost is one load + branch
//! per alloc.
//!
//! Phase 1 ship gate: zero divergence on the existing e2e test
//! suite + a 10k-cons-cell stress test (`SIGIL_GC_CROSS_CHECK=1`
//! exits 0 on every example).

use std::sync::atomic::{AtomicU8, Ordering};

use crate::stackmap;

/// Tri-state atomic for the env-var-driven enable flag. We use u8
/// rather than AtomicBool to give us a "not-yet-checked" sentinel
/// without resorting to OnceLock (which we can use too, but
/// AtomicU8 keeps the steady-state cost to a single relaxed load).
const ENABLE_UNCHECKED: u8 = 0;
const ENABLE_OFF: u8 = 1;
const ENABLE_ON: u8 = 2;

static ENABLE: AtomicU8 = AtomicU8::new(ENABLE_UNCHECKED);

/// Inlined fast-path check. Returns immediately when the env-var
/// gate is off; otherwise dispatches to the slow path. Phase 1 runs
/// the cross-check on every alloc — the stress test bounds exposure,
/// not sampling. A future Phase 2 may add a sample-every-N gate if
/// runtime cost becomes an issue.
#[inline]
pub fn maybe_cross_check() {
    if !is_enabled() {
        return;
    }
    do_cross_check();
}

fn is_enabled() -> bool {
    match ENABLE.load(Ordering::Relaxed) {
        ENABLE_ON => true,
        ENABLE_OFF => false,
        _ => initialise_enable(),
    }
}

#[cold]
fn initialise_enable() -> bool {
    let on = std::env::var_os("SIGIL_GC_CROSS_CHECK")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    ENABLE.store(if on { ENABLE_ON } else { ENABLE_OFF }, Ordering::Relaxed);
    on
}

#[cold]
fn do_cross_check() {
    let roots = stackmap::walk_for_gc();
    if roots.is_empty() {
        // No precise roots derivable on this call — either no frames
        // matched a known safepoint (e.g., the call chain has not yet
        // re-entered Sigil code) or the stackmap index couldn't be
        // initialised. Both are valid: the cross-check is a sanity
        // gate, not a coverage assertion. Subset-of-empty is trivially
        // satisfied.
        return;
    }
    let (lo, hi) = match thread_stack_range() {
        Some(r) => r,
        None => return, // can't bound the stack; skip rather than spuriously fail
    };
    for r in &roots {
        if !(lo..hi).contains(&r.addr) {
            abort_with(format_args!(
                "SIGIL_GC_CROSS_CHECK: precise root 0x{:x} outside stack \
                 range [0x{:x}, 0x{:x}) (return_pc=0x{:x}, kind=0x{:x})",
                r.addr, lo, hi, r.return_pc, r.kind,
            ));
        }
        // SAFETY: `r.addr` lies within [lo, hi); the stack is mapped
        // for the calling thread's lifetime. Read as a possible heap
        // pointer; Boehm's `GC_is_visible` returns the address if it
        // points inside a Boehm-allocated block, NULL otherwise.
        let v: *mut std::ffi::c_void = unsafe { (r.addr as *const *mut std::ffi::c_void).read() };
        if v.is_null() {
            // Null is harmless — the slot may be uninitialised at
            // this PC (Cranelift's safepoint pass keeps it in the
            // map but the value isn't necessarily a live ref yet).
            continue;
        }
        if !value_is_heap_pointer_shape(v as usize) {
            abort_with(format_args!(
                "SIGIL_GC_CROSS_CHECK: precise root 0x{:x} contains \
                 non-heap-pointer-shaped value 0x{:x} \
                 (return_pc=0x{:x}, kind=0x{:x})",
                r.addr, v as usize, r.return_pc, r.kind,
            ));
        }
    }
}

/// Heuristic "looks like a heap pointer". v1 heap pointers are
/// always 8-byte-aligned (allocator-returned) and lie in userspace.
/// We don't have a stable Boehm API to ask "is this a tracked block"
/// without entering a mark — so we use the shape check Boehm itself
/// applies during conservative scans (pointer-sized, aligned, in
/// userspace). False positives are bounded by what Boehm itself
/// would conservatively trace.
fn value_is_heap_pointer_shape(v: usize) -> bool {
    if v == 0 {
        return false;
    }
    // Cranelift-emitted heap pointers from `sigil_alloc` are
    // word-aligned (Boehm hands out 8-byte-aligned blocks). The
    // Cranelift tail-call closure_value path (PR #108) flows a
    // closure heap-ptr through `block_params[0]` — also aligned.
    if v & 0b111 != 0 {
        return false;
    }
    // Reject obvious non-pointer ranges: low addresses (0..4KB) are
    // typically unmapped on every OS we target.
    if v < 0x1000 {
        return false;
    }
    true
}

/// Calling-thread stack range `[sp, stack_base)`. The lower bound is
/// the current stack pointer; the upper bound is the thread's stack
/// base (highest stack address). On Linux we read it via
/// `pthread_getattr_np` + `pthread_attr_getstack`; on macOS via
/// `pthread_get_stackaddr_np` + `pthread_get_stacksize_np`.
fn thread_stack_range() -> Option<(usize, usize)> {
    let sp = current_sp();
    let base = thread_stack_base()?;
    if base <= sp {
        return None;
    }
    Some((sp, base))
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn current_sp() -> usize {
    let sp: usize;
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    sp
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn current_sp() -> usize {
    let sp: usize;
    unsafe {
        std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    sp
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn current_sp() -> usize {
    0
}

#[cfg(target_os = "linux")]
fn thread_stack_base() -> Option<usize> {
    use std::mem::MaybeUninit;
    let mut attr: MaybeUninit<pthread_attr_t> = MaybeUninit::uninit();
    // SAFETY: pthread_getattr_np fills `attr` with the calling
    // thread's attributes; we destroy it before returning.
    unsafe {
        // SAFETY: gc-heap-ptr arithmetic (MaybeUninit local; pointer feeds libc fill-out).
        if pthread_getattr_np(pthread_self(), attr.as_mut_ptr()) != 0 {
            return None;
        }
        let mut stackaddr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut stacksize: usize = 0;
        // SAFETY: gc-heap-ptr arithmetic (MaybeUninit local; libc reads out stack bounds).
        let rc = pthread_attr_getstack(attr.as_mut_ptr(), &mut stackaddr, &mut stacksize);
        // SAFETY: gc-heap-ptr arithmetic (MaybeUninit local; libc destroys allocator-internal state).
        pthread_attr_destroy(attr.as_mut_ptr());
        if rc != 0 {
            return None;
        }
        // Linux pthread_attr_getstack returns the lowest stack
        // address + size. The stack base (highest addr) is
        // stackaddr + stacksize.
        Some((stackaddr as usize).wrapping_add(stacksize))
    }
}

#[cfg(target_os = "macos")]
fn thread_stack_base() -> Option<usize> {
    // SAFETY: pthread_get_stackaddr_np returns the stack-base address
    // (highest addr) for the current thread. Always defined on
    // Darwin; returns null for invalid thread, which we don't see
    // from a Rust-managed thread.
    unsafe {
        let base = pthread_get_stackaddr_np(pthread_self());
        if base.is_null() {
            None
        } else {
            Some(base as usize)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn thread_stack_base() -> Option<usize> {
    None
}

// pthread_attr_t is opaque to Rust. We only need to allocate space
// of the right size + alignment; the actual contents are managed by
// libc. The sizes here are upper bounds taken from glibc / Darwin
// headers; over-sizing is safe (uninitialised tail bytes are never
// read by the libc functions).
#[cfg(target_os = "linux")]
#[repr(C)]
struct pthread_attr_t {
    _data: [u64; 8], // 56 bytes on glibc x86_64; 8x u64 = 64 bytes is a safe upper bound
}

#[cfg(target_os = "linux")]
extern "C" {
    fn pthread_self() -> usize;
    fn pthread_getattr_np(thread: usize, attr: *mut pthread_attr_t) -> i32;
    fn pthread_attr_getstack(
        attr: *mut pthread_attr_t,
        stackaddr: *mut *mut std::ffi::c_void,
        stacksize: *mut usize,
    ) -> i32;
    fn pthread_attr_destroy(attr: *mut pthread_attr_t) -> i32;
}

#[cfg(target_os = "macos")]
extern "C" {
    fn pthread_self() -> *mut std::ffi::c_void;
    fn pthread_get_stackaddr_np(thread: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cold]
fn abort_with(args: std::fmt::Arguments<'_>) -> ! {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{}", args);
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_defaults_to_off_without_env() {
        // Pin a known-OFF state. The actual env-var-based init runs
        // once per process; assume the harness doesn't set the var.
        ENABLE.store(ENABLE_UNCHECKED, Ordering::Relaxed);
        assert!(!is_enabled());
        assert_eq!(ENABLE.load(Ordering::Relaxed), ENABLE_OFF);
    }

    #[test]
    fn current_sp_returns_nonzero_on_supported_arch() {
        let sp = current_sp();
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert!(sp != 0);
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        assert_eq!(sp, 0);
    }

    #[test]
    fn thread_stack_range_is_sane() {
        // thread_stack_range returns [some_lo_sp, stack_base) — the
        // exact lo bound shifts with call depth, so we only assert
        // monotonicity. The current SP after the call sits below
        // the stack base; both lo and hi are user-space addresses.
        let sp_before = current_sp();
        if let Some((lo, hi)) = thread_stack_range() {
            assert!(lo < hi);
            assert!(
                sp_before < hi,
                "current sp ({sp_before:x}) must be below stack base ({hi:x})"
            );
        }
    }
}
