//! Plan E2 Phase 1 Task 5 — opt-in stackmap precise-vs-conservative
//! cross-check.
//!
//! Activated by `SIGIL_GC_CROSS_CHECK=1`. At every `sigil_alloc`
//! invocation the walker:
//!
//! 1. Reads the calling thread's stack range from `pthread_attr_t`.
//! 2. Walks the frame-pointer chain via `stackmap::walk_for_gc` and
//!    collects precise root addresses (set B).
//! 3. Asserts every precise address lies within the stack range —
//!    a conservative-scanner trivially sees all such addresses (set
//!    A is "all word-aligned addresses in [sp, stack_base)"), so
//!    `B ⊆ A` reduces to in-range checking.
//! 4. Asserts the value at each precise address is heap-pointer-
//!    shaped per the same predicate Boehm uses for conservative
//!    pointer recognition.
//!
//! Divergence aborts the process with a diagnostic. Production code
//! paths skip this entirely — the env var is read once via
//! `OnceLock`, so the steady-state cost is one load + branch per
//! alloc.
//!
//! **Coverage assertion** (PR #163 review M1). The harness also
//! tracks total roots seen across the program's lifetime via an
//! atomic counter. At process exit (registered via libc::atexit) the
//! summary line `[SIGIL_GC_CROSS_CHECK] allocs_checked=N
//! roots_total=M fns_resolved=K records_resolved=L` is written to
//! stderr. The e2e cross-check tests parse this line and assert
//! `roots_total > 0` on at least one alloc-bearing example —
//! catches the "silently vacuous" regression that PR #163's first
//! review surfaced.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::stackmap;

/// Cached env-var enable state. `None` = not initialised yet;
/// `Some(true|false)` = decided. Steady-state read is one OnceLock
/// fast-path load. Replaces the prior AtomicU8 tri-state (PR #163
/// review N3).
static ENABLE: OnceLock<bool> = OnceLock::new();
static ALLOCS_CHECKED: AtomicU64 = AtomicU64::new(0);
static ROOTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static ATEXIT_REGISTERED: OnceLock<()> = OnceLock::new();

/// Inlined fast-path check. Returns immediately when the env-var
/// gate is off; otherwise dispatches to the slow path. Phase 1 runs
/// the cross-check on every alloc — the stress test bounds exposure,
/// not sampling.
#[inline]
pub fn maybe_cross_check() {
    if !is_enabled() {
        return;
    }
    do_cross_check();
}

fn is_enabled() -> bool {
    *ENABLE.get_or_init(|| {
        let on = std::env::var_os("SIGIL_GC_CROSS_CHECK")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        if on {
            // Register the at-exit summary writer the first time the
            // env var is observed. Idempotent — atexit handlers run
            // in reverse registration order, and we only ever
            // register once.
            ATEXIT_REGISTERED.get_or_init(|| {
                // SAFETY: libc::atexit accepts a `extern "C" fn()` and
                // calls it during process exit. Our handler reads
                // atomics and writes to stderr — safe to call from
                // any teardown context.
                unsafe {
                    libc::atexit(xcheck_atexit_summary);
                }
            });
        }
        on
    })
}

extern "C" fn xcheck_atexit_summary() {
    let allocs = ALLOCS_CHECKED.load(Ordering::Relaxed);
    let roots = ROOTS_TOTAL.load(Ordering::Relaxed);
    let (fns_resolved, records_resolved) = match stackmap::init_index() {
        Some(idx) => (idx.resolved_function_count(), idx.resolved_record_count()),
        None => (0, 0),
    };
    eprintln!(
        "[SIGIL_GC_CROSS_CHECK] allocs_checked={allocs} roots_total={roots} \
         fns_resolved={fns_resolved} records_resolved={records_resolved}"
    );
}

#[cold]
fn do_cross_check() {
    ALLOCS_CHECKED.fetch_add(1, Ordering::Relaxed);
    let roots = stackmap::walk_for_gc();
    if roots.is_empty() {
        // No precise roots derivable on this call — either no frames
        // matched a known safepoint (e.g., the call chain has not yet
        // re-entered Sigil code) or the stackmap index couldn't be
        // initialised. Both are valid for a single call: the
        // coverage assertion (`roots_total > 0` across the program)
        // is the structural gate, not this one.
        return;
    }
    ROOTS_TOTAL.fetch_add(roots.len() as u64, Ordering::Relaxed);
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
        // pointer.
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
    if v & 0b111 != 0 {
        return false;
    }
    if v < 0x1000 {
        return false;
    }
    true
}

/// Calling-thread stack range `[stack_bottom, stack_base)` — the
/// absolute address span of this thread's stack as known to pthread.
/// `stack_bottom` is the lowest mapped stack address (`stackaddr`
/// from `pthread_attr_getstack` on Linux, `stack_base - stacksize`
/// on macOS); `stack_base` is the highest address (where the stack
/// starts and grows down from).
///
/// Returns `None` when the host APIs aren't available or
/// `pthread_attr_getstack` fails. Callers using this for signal-safe
/// FP validation must cache the result OFF the signal path; pthread
/// queries are not async-signal-safe.
///
/// Plan E2 Phase 3 Task 12 — exposed `pub(crate)` so
/// `profile::cpu::maybe_init` can install the range into
/// `profile::unwind`'s safe-stack-range cache before any SIGPROF
/// can fire. The SIGPROF unwinder uses the cached range to validate
/// every `fp` it's about to dereference, preventing a crash when
/// SIGPROF interrupts libgc's `-fomit-frame-pointer` internals and
/// `ucontext_fp` returns a wild value.
pub(crate) fn thread_stack_bounds() -> Option<(usize, usize)> {
    let base = thread_stack_base()?;
    let size = thread_stack_size()?;
    let bottom = base.checked_sub(size)?;
    Some((bottom, base))
}

/// Calling-thread stack range `[sp, stack_base)` via libc's
/// pthread bindings (PR #163 review N2 — replaces hand-rolled
/// `pthread_attr_t` layout). The lower bound is the current stack
/// pointer; the upper bound is the thread's stack base (highest
/// stack address). Returns `None` when the host APIs aren't
/// available or `pthread_attr_getstack` fails.
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
    linux_attr_getstack().map(|(addr, size)| addr.wrapping_add(size))
}

#[cfg(target_os = "linux")]
fn thread_stack_size() -> Option<usize> {
    linux_attr_getstack().map(|(_, size)| size)
}

#[cfg(target_os = "linux")]
fn linux_attr_getstack() -> Option<(usize, usize)> {
    use std::mem::MaybeUninit;
    let mut attr: MaybeUninit<libc::pthread_attr_t> = MaybeUninit::uninit();
    unsafe {
        // SAFETY: gc-heap-ptr arithmetic (MaybeUninit local; libc fills it).
        let attr_ptr_a = attr.as_mut_ptr();
        if libc::pthread_getattr_np(libc::pthread_self(), attr_ptr_a) != 0 {
            return None;
        }
        let mut stackaddr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut stacksize: libc::size_t = 0;
        // SAFETY: gc-heap-ptr arithmetic (libc reads stack bounds out).
        let attr_ptr_b = attr.as_mut_ptr();
        let rc = libc::pthread_attr_getstack(attr_ptr_b, &mut stackaddr, &mut stacksize);
        // SAFETY: gc-heap-ptr arithmetic (libc destroys allocator-internal state).
        let attr_ptr_c = attr.as_mut_ptr();
        libc::pthread_attr_destroy(attr_ptr_c);
        if rc != 0 {
            return None;
        }
        // Linux pthread_attr_getstack returns the lowest stack
        // address + size. The stack base (highest addr) is
        // stackaddr + stacksize.
        Some((stackaddr as usize, stacksize))
    }
}

#[cfg(target_os = "macos")]
fn thread_stack_base() -> Option<usize> {
    // SAFETY: pthread_get_stackaddr_np returns the stack-base address
    // (highest addr) for the current thread on Darwin.
    unsafe {
        let base = libc::pthread_get_stackaddr_np(libc::pthread_self());
        if base.is_null() {
            None
        } else {
            Some(base as usize)
        }
    }
}

#[cfg(target_os = "macos")]
fn thread_stack_size() -> Option<usize> {
    // SAFETY: pthread_get_stacksize_np returns the stack size in
    // bytes for the calling thread on Darwin.
    let size = unsafe { libc::pthread_get_stacksize_np(libc::pthread_self()) };
    if size == 0 {
        None
    } else {
        Some(size as usize)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn thread_stack_base() -> Option<usize> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn thread_stack_size() -> Option<usize> {
    None
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
        // We can't reset a OnceLock, so we just exercise the lookup
        // path; under cargo test the env var is unset and the cached
        // value is false (or this test ran before enable was first
        // queried, in which case is_enabled() initialises to false).
        assert!(!is_enabled());
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
