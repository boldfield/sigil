//! Test-only utilities for the runtime crate.
//!
//! # Why a shared lock exists
//!
//! Boehm GC is configured with POSIX thread support on our target hosts
//! (`libgc-dev` on Ubuntu and `bdw-gc` on macOS both default to
//! `--enable-threads=posix`), but Rust test threads are **not**
//! auto-registered with Boehm. `pthread_create` interception only
//! engages for threads created via `GC_pthread_create`, which is not
//! what `std::thread::spawn` calls. As a result Boehm's mark phase
//! does not scan the stacks of test threads; any heap object
//! reachable only through such a thread can be prematurely reclaimed,
//! and the freed slot can be reused by a subsequent allocation.
//!
//! Under parallel `cargo test` this races: one test holds an `obj`
//! pointer on its stack, a sibling test triggers a collection, Boehm
//! misses the pointer during stack scan, the object is swept, the
//! sibling reuses the slot, and the first test's reads return stale
//! or cross-test bytes.
//!
//! Fixing this "properly" means registering every Rust test thread
//! with Boehm (`GC_allow_register_threads` + `GC_register_my_thread`
//! with a captured stack base). That's a larger surface change than
//! Task 24 + 25 should carry, so for now we **serialise every
//! GC-touching test** in the runtime crate via a single static
//! `Mutex`. Each runtime test that allocates or reads GC objects
//! holds the guard for its duration; parallel `cargo test` still
//! spawns many test threads, they just take turns entering the GC
//! section.
//!
//! Cost: effectively single-threaded runtime test execution. Runtime
//! tests are <1ms each, so the wall-clock impact is tiny. Plan B's
//! precise-GC rewrite will remove this by registering threads
//! explicitly (or switching off Boehm entirely).
//!
//! # Why the lock is exposed at crate level
//!
//! Both `gc.rs` and `arith.rs` (and any future module whose tests
//! touch the GC) need the same mutex. `pub(crate)` on the mutex
//! getter keeps it internal to the crate while allowing every `mod
//! tests` to reach it through `crate::test_support::gc_test_lock`.

use std::sync::{Mutex, MutexGuard, OnceLock};

static GC_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Acquire the shared GC-test mutex. Hold the returned guard for the
/// entire duration of any test that allocates or reads a Boehm-backed
/// heap object. See the module doc for rationale.
///
/// The guard should be bound to a `_guard` variable so Rust keeps it
/// alive for the test's full scope; dropping it early releases the
/// mutex and re-exposes the test to the cross-thread race.
///
/// The mutex is poisoned if a test panics while holding it. We unwrap
/// the poisoned result rather than re-throw: a panic in one test
/// should not cascade all remaining tests into poison-unwrap failures.
pub(crate) fn gc_test_lock() -> MutexGuard<'static, ()> {
    let m = GC_TEST_LOCK.get_or_init(|| Mutex::new(()));
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One-shot global for `GC_allow_register_threads`. Plan B Task 56's
/// GC stress tests need to call `GC_gcollect` from Rust test threads;
/// Boehm aborts that with "Collecting from unknown thread" unless the
/// thread is registered, and registration requires
/// `GC_allow_register_threads` to have been called once on the program
/// at large.
static ALLOW_REGISTER_ONCE: std::sync::Once = std::sync::Once::new();

/// RAII guard: enrols the calling Rust thread with Boehm GC for the
/// guard's lifetime, allowing `GC_gcollect` and other thread-aware
/// Boehm calls to succeed without aborting; ALSO registers this
/// thread's `HANDLER_STACK` and `ARENA` TLS ranges as Boehm roots so
/// the GC stress tests' allocations are reachable through the same
/// rooting paths the production runtime uses. Drop symmetrically
/// removes the roots and unregisters the thread, preventing stale
/// ranges from accumulating across `--test-threads=N` test thread
/// teardowns.
///
/// Tests that force collection must hold this guard for the duration.
/// Tests that only allocate (no explicit `GC_gcollect`) do not need
/// it — the existing `gc_test_lock` is sufficient.
pub(crate) struct GcThreadEnrolment {
    handler_stack_root: (*mut std::ffi::c_void, *mut std::ffi::c_void),
    arena_root: (*mut std::ffi::c_void, *mut std::ffi::c_void),
}

impl GcThreadEnrolment {
    pub(crate) fn acquire() -> Self {
        // Idempotent global enable. Must precede any
        // GC_register_my_thread call.
        ALLOW_REGISTER_ONCE.call_once(|| {
            // SAFETY: `Once::call_once` guarantees at most one call.
            unsafe { crate::gc::GC_allow_register_threads() };
        });
        // Register this thread. `stack_base = null` lets Boehm
        // discover the bottom of the calling thread's stack itself
        // (per libgc 7.x: passing a NULL stack base requests
        // auto-detection on platforms that support it). The return
        // code is GC_SUCCESS (0) on first registration,
        // GC_DUPLICATE on a thread already registered — both fine.
        let _rc = unsafe { crate::gc::GC_register_my_thread(std::ptr::null()) };
        // Now register this thread's HANDLER_STACK and ARENA ranges
        // as Boehm roots. These are paired with the GC_remove_roots
        // calls in Drop so test thread teardown leaves Boehm's root
        // list clean.
        let handler_stack_root = crate::handlers::register_handler_stack_root_for_calling_thread();
        let arena_root = crate::arena::register_arena_root_for_calling_thread();
        Self {
            handler_stack_root,
            arena_root,
        }
    }
}

impl Drop for GcThreadEnrolment {
    fn drop(&mut self) {
        crate::handlers::unregister_handler_stack_root_for_calling_thread(
            self.handler_stack_root.0,
            self.handler_stack_root.1,
        );
        crate::arena::unregister_arena_root_for_calling_thread(
            self.arena_root.0,
            self.arena_root.1,
        );
        // SAFETY: every test that constructs `GcThreadEnrolment`
        // enrolls before dropping, so the unregister has a matched
        // register. Return code is informational; we ignore it.
        let _rc = unsafe { crate::gc::GC_unregister_my_thread() };
    }
}
