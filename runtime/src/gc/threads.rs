//! Plan E2 Phase 3 — thread discriminator + precise-root callback.
//!
//! Boehm has no per-thread "precise vs conservative" switch (the
//! Task 10 spike pinned this — see
//! `runtime/docs/boehm-per-thread-roots-spike.md`). The
//! distinction is a runtime-side choice. This module surfaces it
//! as a process-wide initialiser + a `GC_set_push_other_roots`
//! callback that walks Sigil threads' stacks via Plan E2 Phase 1's
//! stackmap walker.
//!
//! # API
//!
//! - [`register_sigil_thread_for_precise_roots`] — call from a
//!   Sigil program thread (currently only the main thread). Sets
//!   the thread-local [`IS_SIGIL_THREAD`] flag, installs the
//!   `push_other_roots` callback (`Once`-gated), and pre-warms
//!   the stackmap module's lazy initialisers (so they don't run
//!   inside STW). Does NOT call `GC_register_my_thread` (per
//!   `gc.h:1561`, the main thread is implicitly registered).
//! - [`ensure_gc_process_state_initialised`] — call from a
//!   runtime-internal thread (Plan E1 profile drainer; future
//!   workers). Runs the same install/prewarm path. The name
//!   describes what it actually does: pure process-wide
//!   bootstrapping, no per-thread Boehm enrolment. See
//!   "Runtime threads don't enrol with Boehm" below.
//!
//! # Discriminator state
//!
//! - [`IS_SIGIL_THREAD`] (thread-local `Cell<bool>`): set by
//!   `register_sigil_thread_for_precise_roots`; read by the
//!   `push_other_roots` callback.
//! - [`CAPTURED_SIGIL_CALLER_FP`] (thread-local `Cell<*const usize>`):
//!   sigil_alloc's own FP captured at function entry; read by the
//!   callback as the walker's `starting_fp`. See
//!   `push_sigil_thread_precise_roots` + the captured-FP semantics
//!   note on `stackmap::capture_caller_fp_for_walk`.
//! - [`PRECISE_WALKER_INSTALLED`] (process-wide `Once`): runs
//!   `GC_set_push_other_roots(push_sigil_thread_precise_roots)`
//!   exactly once at process startup. Per `gc_mark.h:309`, the
//!   setter requires external synchronization; the `Once`
//!   discipline is the project's mitigation — install before any
//!   worker thread spawns, never re-install.
//!
//! # Runtime invariant — non-Sigil threads MUST NOT allocate from Boehm
//!
//! `push_sigil_thread_precise_roots` runs once per Boehm mark
//! phase, on whichever thread holds the GC lock — typically the
//! thread whose alloc triggered GC. The callback walks the
//! calling thread's Sigil call chain (from
//! `CAPTURED_SIGIL_CALLER_FP`) only when `IS_SIGIL_THREAD = true`.
//! If a non-Sigil thread allocates and triggers GC:
//!
//! 1. The callback runs on the non-Sigil thread.
//! 2. `IS_SIGIL_THREAD = false` → callback short-circuits.
//! 3. Conservative stack scan is OFF for the (other) Sigil
//!    thread via `GC_do_blocking`, so the Sigil thread's stack
//!    roots are pushed by NEITHER Boehm's auto-scan nor the
//!    callback (running on the wrong thread).
//! 4. Live Sigil heap objects are silently collected.
//!
//! Today the constraint is naturally satisfied: the only
//! runtime-internal threads are the profile drainers
//! (`runtime/src/profile/cpu.rs`, `runtime/src/profile/alloc.rs`),
//! which never call `sigil_alloc` — they shuffle `Vec<Sample>`
//! between a Rust SPSC ring and a `Mutex<Vec<Sample>>` using
//! the system allocator only. Any future runtime worker added
//! to the codebase MUST preserve this invariant — or this
//! design needs a multi-Sigil-thread registry walk (see
//! "What this module does NOT do" below) before that worker
//! lands.
//!
//! Surface the constraint at every runtime worker's spawn site.
//!
//! # Runtime threads don't enrol with Boehm
//!
//! `GC_register_my_thread` requires `GC_allow_register_threads`
//! per `gc.h:1547-1552`. The latter is documented to "include a
//! `GC_start_mark_threads()` call" — switching Boehm from
//! single-marker to parallel-marker mode. Task 12's
//! `GC_set_markers_count(1)` in `sigil_gc_init` pins Boehm to
//! single-marker, which neutralises that side effect.
//!
//! Even with the pin, runtime-thread enrolment is not done in
//! production paths. Two reasons compose to make it unnecessary
//! AND unsafe:
//!
//! 1. **Unnecessary.** Surveying the actual runtime threads
//!    (CPU + alloc profile drainers), neither allocates from
//!    Boehm or holds Boehm pointers on its stack. They shuffle
//!    POD `Sample` structs between a lock-free SPSC ring and
//!    a `Vec<Sample>` on system malloc. Boehm's STW does not
//!    need to suspend them; its mark phase does not need to
//!    scan them.
//!
//! 2. **Unsafe today.** `std::thread::spawn`'d threads don't
//!    route through `GC_pthread_create`'s pthread-key cleanup,
//!    so an enrolled drainer never calls
//!    `GC_unregister_my_thread` on exit. Boehm continues to
//!    expect the dead thread, and the next mark phase crashes
//!    on its stale TLS ranges. CI surfaced this empirically as
//!    a CPU-profile e2e crash before any Sigil output appeared
//!    (PR #170 first cut + this PR's `9a9d7d5..add76bf`
//!    iteration).
//!
//! If a future runtime worker is added that DOES allocate from
//! Boehm, it needs (a) `GC_pthread_create` (not
//! `std::thread::spawn`) so cleanup happens automatically, or
//! (b) an explicit `GC_unregister_my_thread` Drop guard at
//! thread exit. Plus the multi-thread registry walk noted
//! below.
//!
//! # What this module does NOT do
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

// `GC_set_push_other_roots` is Phase 3-specific; declared here.
// `GC_get_push_other_roots` is paired: per `gc_mark.h`'s
// comment "A client supplied procedure should also call the
// original procedure." We capture the prior procedure at
// install time and invoke it from our wrapper so Boehm's
// internal push_other_roots usage (TLS roots, dynamic-library
// roots) keeps working. PR #170 CI surfaced that NOT chaining
// to the prior procedure causes live-object collection on
// alloc-heavy workloads (tree.sigil exit -1, sudoku exit -1,
// etc.) — Boehm's internal roots get dropped on every mark.
//
// `GC_push_all_eager` pushes a root range from inside the
// `GC_set_push_other_roots` callback. The Task 12 callback
// (`push_sigil_thread_precise_roots`) uses it once per precise
// root the stackmap walker yields.
//
// `GC_allow_register_threads` + `GC_register_my_thread` are
// declared in `crate::gc`'s extern block for use by
// `test_support`'s per-test enrolment harness — production
// runtime threads do NOT enrol (see module doc).
#[link(name = "gc")]
extern "C" {
    fn GC_set_push_other_roots(p: GcPushOtherRootsProc);
    fn GC_get_push_other_roots() -> Option<GcPushOtherRootsProc>;
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

    /// Plan E2 Phase 3 Task 12 — captured frame pointer for the
    /// precise walker's `starting_fp`. `sigil_alloc` writes this
    /// on entry (before calling into libgc) so the
    /// `push_other_roots` callback — which runs from inside
    /// libgc's mark phase, where reading the current FP would
    /// land somewhere inside libgc's internal frames (possibly
    /// compiled with `-fomit-frame-pointer`) — can walk from
    /// outside libgc's call chain.
    ///
    /// **The captured FP is `sigil_alloc`'s OWN frame pointer**
    /// (NOT its caller's). The walker iterates UP and, for each
    /// frame `fp`, reads the saved return-PC at `*(fp+8)` to
    /// look up stackmap entries for the function that *called*
    /// that frame. Starting at `sigil_alloc`'s FP, the first
    /// iteration's return-PC points INTO the Sigil function at
    /// the alloc call site — exactly where the stackmap entries
    /// are. Starting one frame higher (the Sigil caller's FP)
    /// would yield the caller's caller's records and miss the
    /// Sigil function's own roots. See
    /// `stackmap::capture_caller_fp_for_walk`'s doc for the
    /// `#[inline(never)]` mechanism that produces this FP.
    ///
    /// `null` when no Sigil alloc is in progress on this thread.
    /// The callback short-circuits to "no precise roots from this
    /// thread" in that case. Today this matters in two cases:
    /// (a) a non-Sigil thread triggered GC; (b) main thread's
    /// startup phase before the first sigil_alloc fires.
    static CAPTURED_SIGIL_CALLER_FP: Cell<*const usize> = const { Cell::new(std::ptr::null()) };
}

/// Install `GC_set_push_other_roots(push_sigil_thread_precise_roots)`
/// exactly once per process. Per `gc_mark.h:309`, the setter
/// requires external synchronization; we install eagerly at the
/// first thread registration (which runs before any worker thread
/// spawns by the Sigil runtime's structure: `sigil_gc_init` runs
/// on the main thread before the profile drainer ever spawns).
static PRECISE_WALKER_INSTALLED: Once = Once::new();

/// The push_other_roots procedure Boehm had registered BEFORE
/// our install. Per `gc_mark.h`, "A client supplied procedure
/// should also call the original procedure" — Boehm uses
/// `GC_set_push_other_roots` for its own internal root-supply
/// hooks (TLS roots; dynamic-library roots on platforms where
/// `dl_iterate_phdr` isn't sufficient). Without chaining, those
/// roots are silently dropped on every mark phase, leading to
/// live-object collection. PR #170 CI surfaced this as exit -1
/// SIGSEGVs on alloc-heavy workloads.
///
/// Captured at install time + invoked from our wrapper. Stored
/// in a static so the wrapper can find it without any extra
/// state plumbing; the value is set exactly once (by the
/// `Once`-gated install) and read by every callback invocation
/// thereafter — no synchronisation needed beyond the Once's
/// happens-before edge.
///
/// **Single-marker safety.** Task 12 pins Boehm to single-marker
/// mode via `GC_set_markers_count(1)` BEFORE `GC_init`, so the
/// callback always fires on the install thread and the
/// `static mut` read is data-race-free. *[Forward-looking — not
/// Task 12's territory]:* if a FUTURE plan enables parallel
/// markers (workload threshold, multi-Sigil-thread support, etc.),
/// migrate this slot to an `AtomicUsize` carrying the transmuted
/// proc pointer (or a `OnceLock<GcPushOtherRootsProc>`) so
/// concurrent marker threads can read it safely.
static mut PRIOR_PUSH_OTHER_ROOTS: Option<GcPushOtherRootsProc> = None;

fn install_push_other_roots_once() {
    PRECISE_WALKER_INSTALLED.call_once(|| {
        // SAFETY: `Once::call_once` guarantees exactly one
        // installation per process, satisfying `gc_mark.h:309`'s
        // external-sync requirement. The proc has `'static`
        // lifetime; Boehm holds it for the process's life. We
        // capture the prior proc BEFORE the setter so our
        // wrapper can chain to it (see `push_sigil_thread_precise_roots`
        // doc + `PRIOR_PUSH_OTHER_ROOTS` doc).
        unsafe {
            let prior = GC_get_push_other_roots();
            PRIOR_PUSH_OTHER_ROOTS = prior;
            GC_set_push_other_roots(push_sigil_thread_precise_roots);
        }
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
/// `GC_register_my_thread` for non-main Sigil threads. Boehm
/// is pinned to single-marker mode via
/// `GC_set_markers_count(1)` before `GC_init`, so a future
/// `GC_allow_register_threads` call won't auto-spawn parallel
/// markers — see the spike doc's Finding 1 for the breakage
/// PR #170 surfaced before the pin landed.)
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

/// Ensure the process-wide GC state this module owns is
/// initialised. Idempotent. Call from any runtime-internal
/// thread (Plan E1 profile drainers; any future runtime worker)
/// at thread-function entry — the call ensures
/// `install_push_other_roots_once` + `stackmap::prewarm_for_stw`
/// have fired regardless of which thread (Sigil or runtime)
/// hits the initialisation path first.
///
/// **Does NOT enrol the calling thread with Boehm.** Per the
/// module doc's "Runtime threads don't enrol with Boehm"
/// section, the profile drainers don't allocate from Boehm and
/// don't hold Boehm pointers on their stacks — Boehm's STW
/// does not need to suspend them, and the mark phase doesn't
/// need to scan them. Calling `GC_register_my_thread` here
/// would additionally leak the registration on thread exit
/// (`std::thread::spawn` doesn't route through
/// `GC_pthread_create`'s pthread-key cleanup hook).
///
/// **Future runtime workers that DO allocate from Boehm** will
/// need a different entry point that explicitly enrols
/// (`GC_allow_register_threads` + `GC_register_my_thread`) AND
/// arranges for `GC_unregister_my_thread` on thread exit (e.g.,
/// via a Drop guard, or by spawning through `GC_pthread_create`).
/// The "Runtime invariant" in the module doc spells out the
/// constraint until then.
///
/// Name history: this function was called
/// `register_runtime_thread_for_conservative_roots` through
/// PR #170. Renamed in Task 12 (this PR) to match its actual
/// behaviour — pure process-wide bootstrap, no per-thread
/// enrolment.
pub fn ensure_gc_process_state_initialised() {
    install_push_other_roots_once();
    crate::stackmap::prewarm_for_stw();
    // IS_SIGIL_THREAD stays at its default `false` so the precise
    // walker callback short-circuits on this thread.
}

/// Plan E2 Phase 3 Task 12 — write `sigil_alloc`'s captured FP
/// into the thread-local `CAPTURED_SIGIL_CALLER_FP` slot. Called
/// from `sigil_alloc`'s `SigilCallerFpGuard::capture()` BEFORE
/// the call into libgc, so the FP references a frame that's
/// still on the stack — and outside libgc's potentially
/// `-fomit-frame-pointer`-compiled call chain — when the
/// mark-phase callback (`push_sigil_thread_precise_roots`)
/// later reads it.
///
/// The captured FP is `sigil_alloc`'s OWN frame pointer (NOT
/// its caller's); see [`CAPTURED_SIGIL_CALLER_FP`]'s doc for
/// the walker-semantics reasoning.
///
/// Cheap: one TLS write per alloc.
#[inline]
pub fn capture_sigil_caller_fp(fp: *const usize) {
    CAPTURED_SIGIL_CALLER_FP.with(|c| c.set(fp));
}

/// Clear the captured FP. Called from
/// `SigilCallerFpGuard`'s `Drop` impl AFTER `sigil_alloc`
/// returns, so a subsequent GC triggered from non-Sigil code
/// (a runtime worker, a future thread that allocated) sees
/// `null` and the `push_other_roots` callback short-circuits
/// instead of walking from a stale FP that has since been
/// popped off the stack.
#[inline]
pub fn clear_sigil_caller_fp() {
    CAPTURED_SIGIL_CALLER_FP.with(|c| c.set(std::ptr::null()));
}

/// `GC_set_push_other_roots` callback. Boehm invokes this once
/// per mark phase, from whatever thread holds the GC lock —
/// typically the thread whose `sigil_alloc` triggered the
/// allocation that crossed Boehm's GC threshold.
///
/// Behaviour, in order:
///
/// 1. **Chain to the prior `push_other_roots` proc.** Boehm
///    uses this hook internally for its own root-supply
///    (TLS roots, dynamic-library roots on platforms where
///    `dl_iterate_phdr` isn't sufficient). Per `gc_mark.h`'s
///    contract — "a client supplied procedure should also
///    call the original procedure" — we capture Boehm's prior
///    proc at install time (see `PRIOR_PUSH_OTHER_ROOTS`) and
///    invoke it FIRST. Skipping this chain caused
///    live-object collection in PR #170 CI (`tree.sigil`
///    exit -1, sudoku exit -1) — Boehm's internal roots got
///    dropped on every mark.
///
/// 2. **Short-circuit if `IS_SIGIL_THREAD = false`.** The
///    callback fires on whichever thread holds the GC lock;
///    only Sigil threads have a meaningful captured FP to
///    walk from. The runtime invariant in the module doc
///    pins GC-triggering allocations to Sigil threads only,
///    so a `false` reading here corresponds to no precise
///    roots needing to be supplied from this thread.
///
/// 3. **Short-circuit if `CAPTURED_SIGIL_CALLER_FP` is null.**
///    No `sigil_alloc` is in progress on this thread (process
///    startup before the first alloc; teardown after the last).
///    Walking from a stale or null FP would either walk
///    nothing useful or follow popped stack data.
///
/// 4. **Walk via `walk_for_gc_with_callback_from(captured_fp, …)`.**
///    The walker iterates UP the FP chain from the captured
///    FP — outside libgc's call chain, so any
///    `-fomit-frame-pointer`-compiled libgc internals are
///    not on the traversal path. For each precise root the
///    walker yields, the closure pushes an 8-byte range via
///    `GC_push_all_eager` so Boehm's mark phase tracks the
///    word as a potential heap pointer.
///
/// The walker variant + the captured FP together close the
/// SIGSEGV failure mode PR #170 surfaced when the original
/// `walk_for_gc_with_callback` traversed libgc frames
/// directly. See the spike doc's Finding 2 for the empirical
/// breakage and the "Captured-FP walker entry-point semantics"
/// section under Task 12 implementation notes for the
/// fp-arithmetic correctness argument.
extern "C" fn push_sigil_thread_precise_roots() {
    // Chain to Boehm's prior push_other_roots first. Per
    // `gc_mark.h`, "A client supplied procedure should also
    // call the original procedure" — Boehm's internal proc
    // pushes TLS roots + dynamic-library roots that our
    // wrapper would otherwise drop. The prior proc is
    // captured at install time (see `install_push_other_roots_once`).
    //
    // SAFETY: the prior proc is an `extern "C" fn()` Boehm
    // installed; calling it is the documented contract. The
    // `static mut` read is safe because the value is written
    // exactly once (inside `Once::call_once`) and is read only
    // after that write has completed (the `Once` provides the
    // happens-before edge: the callback can only fire after
    // `GC_set_push_other_roots` returns, which happens after
    // the write).
    let prior = unsafe { PRIOR_PUSH_OTHER_ROOTS };
    if let Some(prior) = prior {
        prior();
    }

    // Discriminator gates per the doc above: a `false`
    // IS_SIGIL_THREAD or a null captured FP both mean "no
    // precise roots to supply from this thread's call chain".
    let is_sigil = IS_SIGIL_THREAD.with(Cell::get);
    if !is_sigil {
        return;
    }
    let captured_fp = CAPTURED_SIGIL_CALLER_FP.with(Cell::get);
    if captured_fp.is_null() {
        return;
    }
    crate::stackmap::walk_for_gc_with_callback_from(captured_fp, |r| {
        // SAFETY: the stack range [r.addr, r.addr + 8) lives on
        // the calling thread's stack. Inside `GC_do_blocking`
        // the thread is in "inactive" state but not suspended;
        // Boehm reads the range as a conservative root range
        // (1 word) but our descriptor-emitted roots are heap
        // pointer slots, so Boehm's mark-stack tracks each via
        // the typed-malloc descriptors.
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
    fn ensure_gc_process_state_does_not_set_sigil_flag() {
        // The process-state initialiser must NOT turn this
        // thread into a "Sigil" thread for the precise-walker
        // callback's purposes — it's a pure no-op on Boehm
        // state (install + prewarm OnceLocks, leave
        // `IS_SIGIL_THREAD = false`), safe to run in any
        // cargo-test thread without subprocess isolation.
        ensure_gc_process_state_initialised();
        assert!(!is_sigil_thread());
        // The walker must be installed regardless — the install
        // is `Once`-gated and reachable from BOTH this entry
        // point and `register_sigil_thread_for_precise_roots`,
        // so it fires no matter which thread (Sigil or runtime)
        // hits the initialisation path first.
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
