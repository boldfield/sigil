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
//!   walk this thread's stack precisely. Also installs the
//!   callback (`Once`-gated) and pre-warms the stackmap module's
//!   lazy initialisers (so they don't run inside STW).
//!   Does NOT call `GC_register_my_thread` (per `gc.h:1561`,
//!   the main thread is implicitly registered).
//! - [`register_runtime_thread_for_conservative_roots`] — call
//!   from a runtime-internal thread (Plan E1 profile drainer,
//!   etc.). Pre-warms the same process-wide state. Does NOT
//!   mark the thread for precise walking AND does NOT enroll
//!   it with Boehm (the drainer doesn't allocate from Boehm; the
//!   call to `GC_allow_register_threads` that enrolment would
//!   require has the side effect of starting parallel marker
//!   threads, which Task 11 verified breaks the marker for
//!   single-threaded user programs — see "Why no Boehm enrolment
//!   in production today" below).
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
//! # Why no Boehm enrolment in production today
//!
//! `GC_register_my_thread` requires `GC_allow_register_threads`
//! per `gc.h:1547-1552`. The latter is documented to "include a
//! `GC_start_mark_threads()` call" — switching Boehm from
//! single-marker to parallel-marker mode. PR #170's first cut
//! (commit `e30d6ef`) called both from production for the
//! drainer thread; CI surfaced that switching to parallel
//! markers in an otherwise-single-threaded user program breaks
//! the marker visibly:
//!
//! - `tree.sigil` returned the wrong sum (`6749` instead of
//!   `32767`) — live tree nodes were collected as garbage.
//! - `cpu_profile_writes_*` tests had compiled binaries crash
//!   with empty stdout/stderr.
//! - `std_list_sort_int_ten_thousand_reversed` and
//!   `std_map_ten_thousand_inserts_then_lookups` failed for
//!   the same class of reason.
//!
//! Until Task 12 lands the empirical work to characterise the
//! parallel-marker interaction (likely needs an explicit
//! `GC_set_disable_automatic_collection` knob, or a
//! `GC_call_with_gc_active` wrapping at the marker callback's
//! entry, or a different enrolment path entirely), the
//! discriminator API is provided but neither path enrolls with
//! Boehm. The drainer doesn't need enrolment (no heap
//! allocation); the Sigil main thread is implicitly registered.
//! The push_other_roots callback IS installed, but it pushes
//! roots that Boehm's still-on conservative stack scan also
//! finds — so the callback is observably inert.
//!
//! Task 12's empirical work resolves this when the runtime
//! actually NEEDS Boehm enrolment + the precise walker to be
//! load-bearing (i.e., when conservative stack scan is
//! disabled for Sigil threads via `GC_do_blocking`).
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

// `GC_set_push_other_roots` + `GC_push_all_eager` are Phase 3-
// specific; declared here. Tasks 12+ may fold them into the
// parent extern block once production callers exist outside
// this module.
//
// `GC_allow_register_threads` + `GC_register_my_thread` are
// intentionally NOT used by this module today — see the doc
// comment on `register_runtime_thread_for_conservative_roots`
// for the parallel-marker rationale. Task 12 reintroduces them
// when the empirical interaction with the marker is
// characterised + sigil_alloc gains the `GC_do_blocking`
// wrapping that makes the precise walker load-bearing.
#[link(name = "gc")]
extern "C" {
    fn GC_set_push_other_roots(p: GcPushOtherRootsProc);
    fn GC_push_all_eager(bottom: *mut c_void, top: *mut c_void);
}

type GcPushOtherRootsProc = extern "C" fn();

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
/// callback is installed + pre-warms the stackmap module's
/// lazy initialisers.
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
/// **No `GC_allow_register_threads` call either.** That call
/// is the precondition for `GC_register_my_thread`, which
/// this path doesn't make. More importantly,
/// `GC_allow_register_threads` is documented to "include a
/// `GC_start_mark_threads()` call" (`gc.h:1551`) — it switches
/// Boehm from single-marker to parallel-marker mode. PR #170
/// CI surfaced that turning on parallel markers for an
/// otherwise-single-threaded user program (no drainer because
/// `SIGIL_CPU_PROFILE` is unset) breaks the marker visibly:
/// `tree.sigil` returned the wrong sum (live nodes collected
/// as garbage). Until Task 12 understands the parallel-marker
/// interaction, keep allow off the main-thread path. The
/// drainer-thread path (`register_runtime_thread_for_conservative_roots`)
/// fires it conditionally, only when `SIGIL_CPU_PROFILE` is
/// set + the drainer actually spawns.
///
/// Idempotent: calling more than once is a no-op (the TLS flag
/// is already set; the `Once`s already fired).
pub fn register_sigil_thread_for_precise_roots() {
    install_push_other_roots_once();
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
/// (Plan E1 profile drainer; any future runtime worker). Sets
/// the discriminator state machine to "this is NOT a Sigil
/// thread" — i.e., a no-op on `IS_SIGIL_THREAD` (which defaults
/// to `false` already), but ensures `install_push_other_roots_once` +
/// `prewarm_for_stw` have fired regardless of which thread
/// (Sigil or runtime) registers first.
///
/// **Does NOT call `GC_register_my_thread`.** The Plan E1
/// drainer doesn't touch GC-managed memory (it shuffles
/// `Vec<Sample>` between a Rust SPSC ring and a
/// `Mutex<Vec<Sample>>` — system allocator only), so it doesn't
/// need Boehm STW participation. Calling `GC_register_my_thread`
/// would require `GC_allow_register_threads` first, which per
/// `gc.h:1551` "includes a `GC_start_mark_threads()` call" —
/// switching Boehm to parallel-marker mode. PR #170 CI
/// (commit 620a891) showed that just enabling parallel markers
/// for an otherwise-single-threaded user program breaks the
/// marker (live `tree.sigil` nodes collected as garbage).
/// Until Task 12 lands the empirical work to characterise the
/// parallel-marker interaction, this entry point keeps Boehm
/// in single-marker mode by NOT calling allow/register.
///
/// **Future runtime workers that DO allocate from Boehm** will
/// need a different entry point that explicitly enrolls
/// (`GC_allow_register_threads` + `GC_register_my_thread`).
/// That's a follow-up when such a worker exists; the
/// "Runtime invariant" in the module doc spells out the
/// constraint until then.
pub fn register_runtime_thread_for_conservative_roots() {
    install_push_other_roots_once();
    crate::stackmap::prewarm_for_stw();
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

    #[test]
    fn fresh_thread_starts_with_is_sigil_thread_false() {
        // The thread-local default is `false`. This test runs
        // on a fresh cargo-test thread (cargo spawns one per
        // `#[test]`); without an explicit register call,
        // `IS_SIGIL_THREAD` should remain `false`. The
        // discriminator API doesn't touch Boehm state, so no
        // subprocess isolation is needed.
        assert!(!is_sigil_thread());
    }

    #[test]
    fn register_runtime_thread_does_not_set_sigil_flag() {
        // Conservative registration must NOT turn this thread
        // into a "Sigil" thread for the precise-walker callback's
        // purposes. Post-PR-#170-rework, this entry point is
        // a pure no-op on Boehm state (just install/prewarm
        // OnceLocks + leave `IS_SIGIL_THREAD = false`), so it
        // can run in any cargo-test thread without subprocess
        // isolation.
        register_runtime_thread_for_conservative_roots();
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
        // Sets `IS_SIGIL_THREAD` on the calling cargo-test
        // worker thread. The flag is `thread_local!`, so it
        // doesn't bleed into other workers; the
        // `fresh_thread_starts_with_is_sigil_thread_false`
        // test running on a different worker is unaffected.
        register_sigil_thread_for_precise_roots();
        assert!(is_sigil_thread());
        assert!(precise_walker_was_installed());
    }
}
