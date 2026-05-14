//! Plan E2 Phase 3 Task 11 — thread registration discriminator.
//!
//! Boehm has no per-thread "precise vs conservative" switch (the
//! Task 10 spike pinned this — see
//! `runtime/docs/boehm-per-thread-roots-spike.md`). The
//! distinction is a runtime-side choice. This module surfaces it
//! as two API entry points + a `GC_set_push_other_roots` callback
//! that walks Sigil threads' stacks via Plan E2 Phase 1's
//! stackmap walker.
//!
//! # API
//!
//! - [`register_sigil_thread_for_precise_roots`] — call from a
//!   Sigil program thread (currently only the main thread). Marks
//!   the thread with a thread-local flag the
//!   `GC_set_push_other_roots` callback uses to decide whether to
//!   walk this thread's stack precisely. Also performs the
//!   standard `GC_register_my_thread(NULL)` enrolment.
//! - [`register_runtime_thread_for_conservative_roots`] — call
//!   from a runtime-internal thread (Plan E1 profile drainer,
//!   etc.). Performs `GC_register_my_thread(NULL)` so Boehm
//!   suspends the thread during STW and scans its stack
//!   conservatively. Does NOT mark the thread for precise walking.
//!
//! # Discriminator state
//!
//! - [`IS_SIGIL_THREAD`] (thread-local `Cell<bool>`): set by
//!   `register_sigil_thread_for_precise_roots`; read by the
//!   `push_other_roots` callback.
//! - [`PRECISE_WALKER_INSTALLED`] (process-wide `Once`): runs
//!   `GC_set_push_other_roots(push_sigil_thread_precise_roots)`
//!   exactly once at process startup. Per `gc_mark.h:309`, the
//!   setter requires external synchronization; the `Once`
//!   discipline is the project's mitigation — install before any
//!   worker thread spawns, never re-install.
//!
//! # Runtime invariant — non-Sigil threads MUST NOT allocate from Boehm
//!
//! `push_sigil_thread_precise_roots` walks the calling thread's
//! stack. Boehm invokes the callback once per mark phase, on
//! whichever thread holds the GC lock — typically the thread
//! whose alloc triggered GC. So if a non-Sigil thread allocates
//! and triggers GC:
//!
//! 1. The callback runs on the non-Sigil thread.
//! 2. `IS_SIGIL_THREAD = false` on that thread → callback
//!    short-circuits, pushes no roots.
//! 3. Post-Task-12 (when conservative scan is disabled on Sigil
//!    threads), Sigil's stack roots are then pushed by NEITHER
//!    Boehm's auto-scan (off for the Sigil thread) nor the
//!    callback (running on the wrong thread).
//! 4. Live Sigil heap objects are silently collected.
//!
//! Today the constraint is naturally satisfied: the only
//! runtime-internal thread is the Plan E1 profile drainer
//! (`runtime/src/profile/cpu.rs`), which never calls
//! `sigil_alloc` — it shuffles `Vec<Sample>` between a Rust
//! SPSC ring and a `Mutex<Vec<Sample>>` using the system
//! allocator only. Any future runtime worker added to the
//! codebase MUST preserve this invariant — or this design
//! needs a multi-Sigil-thread registry walk (see "What this
//! module does NOT do" below) before that worker lands.
//!
//! Surface the constraint at the drainer's registration site
//! (`runtime/src/profile/cpu.rs::drainer_loop`) and at any
//! future runtime worker's spawn site.
//!
//! # What this module does NOT do
//!
//! - **Does not flip Boehm's conservative stack scan off.** That's
//!   Task 12's job. Until then, the precise walker's pushed roots
//!   are *additional* to Boehm's auto-scan, which means Task 11
//!   alone changes nothing observable: all the precise roots are
//!   already being scanned conservatively. The Task 11 deliverable
//!   is structural — install the discriminator + the callback so
//!   Task 12 has a clean hook.
//!
//! - **Does not resolve the "wrap `sigil_alloc` in
//!   `GC_call_with_gc_active`?" question** that the Task 10 spike
//!   doc deferred to Task 11/12 empirical resolution. Task 11
//!   doesn't wrap anything in `GC_do_blocking` either; until Task
//!   12 introduces the wrapping, the question is moot.
//!
//! - **Does not multi-thread the registry.** Sigil today is
//!   single-threaded; the registry is implicitly single-entry
//!   (the main thread). The "callback walks calling thread"
//!   shape works because the runtime invariant above pins GC
//!   to fire from the Sigil thread. A registry walk
//!   (`Mutex<Vec<RegisteredSigilThread>>` iterated under the
//!   callback, walking each Sigil thread's suspended-FP
//!   snapshot) is the structural fix when either (a) Sigil
//!   grows multiple program threads or (b) a runtime worker
//!   needs to call `sigil_alloc`.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Once;

use crate::stackmap;

// Reuse `GC_allow_register_threads` + `GC_register_my_thread`
// from the parent module's extern block (single source of
// truth — duplicate declarations across modules would let
// signature drift link to one or the other silently).
// `GC_set_push_other_roots` + `GC_push_all_eager` are Phase 3-
// specific; declared here. Tasks 12+ may fold them into the
// parent extern block once production callers exist outside
// this module.
use super::{GC_allow_register_threads, GC_register_my_thread};

#[link(name = "gc")]
extern "C" {
    fn GC_set_push_other_roots(p: GcPushOtherRootsProc);
    fn GC_push_all_eager(bottom: *mut c_void, top: *mut c_void);
}

type GcPushOtherRootsProc = extern "C" fn();

const GC_SUCCESS: i32 = 0;
const GC_DUPLICATE: i32 = 1;

/// `GC_allow_register_threads` must be called before any
/// `GC_register_my_thread` invocation (per `gc.h:1544-1552`).
/// The `Once` runs it on the first registration attempt — works
/// for both the Sigil-thread path (where the main thread doesn't
/// itself call `GC_register_my_thread`, but might later if Sigil
/// goes multi-threaded) and the runtime-thread path.
static ALLOW_REGISTER_THREADS: Once = Once::new();

fn allow_register_threads_once() {
    ALLOW_REGISTER_THREADS.call_once(|| {
        // SAFETY: `Once::call_once` guarantees exactly one
        // invocation per process, satisfying Boehm's
        // "called from the main (or any previously registered)
        // thread between the collector initialization and the
        // first explicit registering of a thread" precondition
        // — assuming `sigil_gc_init` (which runs on the main
        // thread) has already initialised the collector by the
        // time any thread reaches this code path. The Sigil
        // runtime structure guarantees that ordering: the
        // drainer thread spawn is downstream of `sigil_gc_init`.
        unsafe { GC_allow_register_threads() };
    });
}

thread_local! {
    /// Per-thread marker the `push_other_roots` callback reads to
    /// decide whether to walk the calling thread's stack
    /// precisely. Default `false` (runtime-internal / unregistered
    /// threads). Set to `true` by
    /// `register_sigil_thread_for_precise_roots`.
    static IS_SIGIL_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Install `GC_set_push_other_roots(push_sigil_thread_precise_roots)`
/// exactly once per process. Per `gc_mark.h:309`, the setter
/// requires external synchronization; we install eagerly at the
/// first thread registration (which runs before any worker thread
/// spawns by the Sigil runtime's structure: `sigil_gc_init` runs
/// on the main thread before the profile drainer ever spawns).
static PRECISE_WALKER_INSTALLED: Once = Once::new();

fn install_push_other_roots_once() {
    PRECISE_WALKER_INSTALLED.call_once(|| {
        // SAFETY: `Once::call_once` guarantees exactly one
        // installation per process, satisfying `gc_mark.h:309`'s
        // external-sync requirement. The proc has `'static`
        // lifetime; Boehm holds it for the process's life.
        unsafe { GC_set_push_other_roots(push_sigil_thread_precise_roots) };
    });
}

/// Register the calling thread as a Sigil program thread. Sets
/// the thread-local discriminator + ensures the precise-walker
/// callback is installed + ensures `GC_allow_register_threads`
/// has fired (so subsequent runtime-thread registrations on
/// other threads can call `GC_register_my_thread` safely).
///
/// **No `GC_register_my_thread` call.** Per `gc.h:1561-1562`,
/// "This should never be called from the main thread, where it
/// is always done implicitly." Sigil today runs the user
/// program on the main thread, so this entry point doesn't
/// register with Boehm — the implicit registration covers it.
/// (When Sigil goes multi-threaded post-Plan-E2, this entry
/// point will need to branch on "main vs not" and call
/// `GC_register_my_thread` for non-main Sigil threads.)
///
/// **Why `GC_allow_register_threads` fires here, not in the
/// runtime-thread entry point:** per `gc.h:1547-1552`, the
/// allow call must be made "from the main (or any previously
/// registered) thread between the collector initialization and
/// the first explicit registering of a thread (it should be
/// called as late as possible)." Sigil's runtime structure
/// guarantees `sigil_gc_init` (which calls this registration)
/// runs on the main thread before any worker thread spawns,
/// so the allow call lands on the main thread. Calling it
/// later from the drainer would technically violate the docs,
/// even though libgc 8.x tolerates it in practice.
///
/// Idempotent: calling more than once is a no-op (the TLS flag
/// is already set; the `Once`s already fired).
pub fn register_sigil_thread_for_precise_roots() {
    install_push_other_roots_once();
    allow_register_threads_once();
    // Pre-warm every lazy initialiser the stackmap module owns
    // BEFORE any GC can fire. The push_other_roots callback
    // (`push_sigil_thread_precise_roots`) calls
    // `stackmap::walk_for_gc_with_callback` from inside Boehm's
    // STW mark phase; any libc malloc invoked from there can
    // deadlock against a suspended thread holding malloc's
    // internal lock. The `prewarm_for_stw` helper triggers
    // every OnceLock the walk path traverses (StackmapIndex
    // BTreeMap; SIGIL_GC_XCHECK_TRACE env var). Idempotent.
    crate::stackmap::prewarm_for_stw();
    IS_SIGIL_THREAD.with(|f| f.set(true));
}

/// Register the calling thread as a runtime-internal thread
/// (Plan E1 profile drainer; any future runtime worker). Calls
/// `GC_allow_register_threads` (Once-gated) + `GC_register_my_thread`
/// so Boehm suspends the thread during STW + its stack is scanned
/// conservatively. Does NOT set the precise-walker flag.
///
/// Idempotent (`GC_register_my_thread` returns `GC_DUPLICATE` on
/// re-registration).
///
/// **Why explicit conservative-registration even though Boehm's
/// default scan IS conservative.** Without explicit
/// `GC_register_my_thread`, Boehm doesn't know the thread exists
/// — STW won't suspend it, and any heap pointers on its stack
/// won't be scanned at all. The current Plan E1 drainer doesn't
/// hold heap pointers (it shuffles `Vec<Sample>` between a Rust
/// SPSC ring and a `Mutex<Vec<Sample>>`), so unregistered would
/// also work today. Explicit registration is forward-proofing
/// for a future runtime thread that DOES touch heap memory.
pub fn register_runtime_thread_for_conservative_roots() {
    install_push_other_roots_once();
    allow_register_threads_once();
    // SAFETY: NULL stack base = Boehm auto-detects via
    // `GC_get_stack_base` — the documented pattern for threads
    // not created via `GC_pthread_create`.
    let rc = unsafe { GC_register_my_thread(std::ptr::null()) };
    debug_assert!(
        rc == GC_SUCCESS || rc == GC_DUPLICATE,
        "GC_register_my_thread(NULL) returned rc={rc} \
         (expected GC_SUCCESS=0 or GC_DUPLICATE=1)"
    );
    // IS_SIGIL_THREAD stays at its default `false`.
}

/// `GC_set_push_other_roots` callback. Boehm invokes this once
/// per mark phase, from whatever thread holds the GC lock
/// (typically the thread whose alloc triggered GC).
///
/// For Sigil threads: walk the calling thread's stack via Plan
/// E2 Phase 1's stackmap walker and push each precise root
/// location as an 8-byte range via `GC_push_all_eager`. Uses
/// the [`stackmap::walk_for_gc_with_callback`] closure variant,
/// NOT the `Vec`-returning `walk_for_gc`: this callback runs
/// inside Boehm's STW with all enrolled threads suspended; if
/// `Vec::push` triggered a libc `malloc` and a suspended thread
/// happened to hold libc's internal allocator lock, we'd
/// deadlock. The closure variant streams roots without
/// allocating.
///
/// For non-Sigil threads (runtime drainer; cargo-test workers
/// in our integration-test process): no-op. The stackmap walker
/// would either find no matching safepoint records (because the
/// thread isn't running Sigil-emitted code) or produce garbage
/// (if a runtime function's PC happened to coincide with a
/// safepoint range — vanishingly unlikely but the gate is the
/// principled mitigation).
///
/// **Single-Sigil-thread limitation.** This callback only walks
/// the *calling* thread's stack. With Sigil today as
/// single-Sigil-threaded + the runtime invariant that only
/// Sigil threads call `sigil_alloc` (see module doc), GC always
/// fires from the Sigil thread, which is the calling thread.
/// If a future runtime worker calls `sigil_alloc` (and triggers
/// GC), the callback runs on the worker — `IS_SIGIL_THREAD =
/// false` — and Sigil's roots never get pushed. After Task 12
/// disables conservative scan on the Sigil thread, that would
/// silently collect live Sigil objects. The constraint
/// "non-Sigil threads MUST NOT allocate from Boehm" is the
/// runtime-side mitigation; see module doc.
extern "C" fn push_sigil_thread_precise_roots() {
    let is_sigil = IS_SIGIL_THREAD.with(Cell::get);
    if !is_sigil {
        return;
    }
    stackmap::walk_for_gc_with_callback(|r| {
        // SAFETY: the stack range [r.addr, r.addr + 8) lives on
        // the calling thread's stack, which is suspended by
        // Boehm's STW for the duration of the mark phase. Boehm
        // owns the read. `GC_push_all_eager` does not allocate
        // (it appends to Boehm's internal mark stack via lock-
        // free updates).
        let bottom = r.addr as *mut c_void;
        let top = (r.addr.wrapping_add(8)) as *mut c_void;
        unsafe { GC_push_all_eager(bottom, top) };
    });
}

#[cfg(test)]
pub(crate) fn is_sigil_thread() -> bool {
    IS_SIGIL_THREAD.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn precise_walker_was_installed() -> bool {
    PRECISE_WALKER_INSTALLED.is_completed()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Subprocess-mode env var. Tests that touch Boehm's
    /// per-thread registration state run in a dedicated
    /// subprocess so the inevitable thread-tear-down at test
    /// end doesn't leave a stale Boehm record that crashes the
    /// next parallel test (same problem class the Phase 2 spike
    /// + handler stress tests work around).
    const STRESS_INNER_VAR: &str = "SIGIL_GC_STRESS_INNER";

    fn in_stress_subprocess() -> bool {
        std::env::var(STRESS_INNER_VAR).is_ok()
    }

    fn run_in_subprocess(test_name: &str) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("run_in_subprocess: current_exe failed: {e}");
                std::process::abort();
            }
        };
        let full_name = format!("gc::threads::tests::{test_name}");
        let status = match std::process::Command::new(&exe)
            .args(["--exact", &full_name, "--nocapture"])
            .env(STRESS_INNER_VAR, "1")
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("run_in_subprocess: spawn `{full_name}` failed: {e}");
                std::process::abort();
            }
        };
        assert!(
            status.success(),
            "subprocess for `{full_name}` failed: {status}"
        );
    }

    #[test]
    fn fresh_thread_starts_with_is_sigil_thread_false() {
        // The thread-local default is `false`. This test runs on
        // a fresh cargo-test thread (cargo spawns one per #[test]
        // even under --test-threads=1); without an explicit
        // register call, `IS_SIGIL_THREAD` should remain `false`.
        // No Boehm interaction → no subprocess needed.
        //
        // **Assumption**: every test in this module that calls
        // `register_sigil_thread_for_precise_roots` runs in
        // subprocess mode, so the flag never persists into a
        // reused cargo-test worker thread. If a future test
        // sets the flag without subprocess isolation, this test
        // would non-deterministically fail when scheduled on
        // the same worker. Future test additions: keep the
        // discipline (subprocess-wrap any flag-setting test) OR
        // run THIS test in a subprocess too.
        assert!(!is_sigil_thread());
    }

    #[test]
    fn register_runtime_thread_does_not_set_sigil_flag() {
        // Calls `GC_register_my_thread`, which touches Boehm's
        // per-thread record table — under parallel cargo-test
        // execution that races with other GC-using tests'
        // thread tear-downs and SIGSEGVs. Subprocess isolation
        // matches the Phase 2 false-retention reproducer's
        // discipline (`runtime/src/gc.rs::tests::*` use the
        // same `SIGIL_GC_STRESS_INNER` env-var pattern).
        if !in_stress_subprocess() {
            run_in_subprocess("register_runtime_thread_does_not_set_sigil_flag");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        // Boehm requires GC_init before GC_allow_register_threads;
        // sigil_gc_init runs the init via Once.
        crate::gc::sigil_gc_init();

        register_runtime_thread_for_conservative_roots();
        // Conservative registration must NOT turn this thread
        // into a "Sigil" thread for the precise-walker callback's
        // purposes.
        assert!(!is_sigil_thread());
        // The walker must be installed regardless — the install
        // is `Once`-gated and the runtime-thread entry path
        // also goes through it (so the install happens whether
        // the runtime sets up Sigil threads first or runtime
        // threads first).
        assert!(precise_walker_was_installed());
    }

    #[test]
    fn register_sigil_thread_sets_sigil_flag() {
        // Doesn't itself call `GC_register_my_thread` (per the
        // module doc, the main thread is implicitly registered
        // by Boehm), but installs the `push_other_roots` callback
        // via `Once`. Subprocess isolation defends against the
        // sequencing that a parallel cargo-test run could
        // produce: another test installs a different callback,
        // then asserts on `precise_walker_was_installed()` here
        // races with that install.
        if !in_stress_subprocess() {
            run_in_subprocess("register_sigil_thread_sets_sigil_flag");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        crate::gc::sigil_gc_init();

        register_sigil_thread_for_precise_roots();
        assert!(is_sigil_thread());
        assert!(precise_walker_was_installed());
    }
}
