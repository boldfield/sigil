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
