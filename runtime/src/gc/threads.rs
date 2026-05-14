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
//! **\[Forward-looking — Task 12\]:** the constraint below
//! becomes load-bearing only when the callback's body lands
//! in Task 12. Today the callback is a no-op (see the
//! `push_sigil_thread_precise_roots` doc-comment), so there's
//! nothing to short-circuit; the failure mode the invariant
//! describes is dormant. The constraint is documented here
//! so Task 12's implementer doesn't accidentally introduce a
//! runtime worker that allocates before the registry-walk
//! mitigation lands.
//!
//! `push_sigil_thread_precise_roots` will walk the calling
//! thread's stack (post-Task-12). Boehm invokes the callback
//! once per mark phase, on whichever thread holds the GC lock
//! — typically the thread whose alloc triggered GC. So if a
//! non-Sigil thread allocates and triggers GC:
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
//! - **Does not walk Sigil stacks for precise roots.** The
//!   `push_sigil_thread_precise_roots` callback's body is a
//!   no-op for Task 11 (it chains to Boehm's prior
//!   push_other_roots proc + returns). The walker SIGSEGVs
//!   when invoked from inside libgc's mark phase because
//!   libgc may be compiled with `-fomit-frame-pointer`,
//!   making `*fp` reads through libgc internal frames yield
//!   garbage. Task 12 reinstates the walker once
//!   `sigil_alloc`'s `GC_do_blocking` boundary captures the
//!   user-level FP outside libgc's call chain.
//!
//! - **Does not flip Boehm's conservative stack scan off.** That's
//!   Task 12's job. Today's still-conservative scan finds
//!   all the roots a precise walker would (the conservative
//!   scan is a superset). Task 12's behavior change requires
//!   the captured-FP walker above AND the empirical work to
//!   characterise the parallel-marker interaction (PR #170
//!   CI showed that `GC_allow_register_threads` switches
//!   Boehm to parallel-marker mode, which breaks alloc-heavy
//!   workloads on previously-single-threaded user programs).
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

// `crate::stackmap` is used only by Task 11's *future* callback
// body (Task 12 fills it in). The current no-op callback doesn't
// touch the stackmap walker; the qualified-path call to
// `crate::stackmap::prewarm_for_stw()` is the only present
// dependency. Keep the `use` removed until Task 12 reinstates it.

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
// `GC_allow_register_threads` + `GC_register_my_thread` are
// used by `register_runtime_thread_for_conservative_roots`
// post-Task-12. Safe to call now because Task 12's
// `GC_set_markers_count(1)` in `sigil_gc_init` pins Boehm to
// single-marker mode before `GC_allow_register_threads`'s
// implicit `GC_start_mark_threads()` can spawn parallel
// markers.
//
// `GC_push_all_eager` pushes a root range from inside the
// `GC_set_push_other_roots` callback. Task 12 callback uses
// it per precise root the walker yields.
//
// `GC_allow_register_threads` + `GC_register_my_thread` are
// declared in `crate::gc`'s extern block for use by
// `test_support`'s per-test enrolment harness. Task 12 surveyed
// the runtime threads (CPU-profile drainer, alloc-profile
// drainer) and concluded none of them allocate from Boehm or
// hold Boehm pointers, so none need to enrol — see
// `register_runtime_thread_for_conservative_roots`'s doc.
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

    /// Plan E2 Phase 3 Task 12 — captured user-level FP for the
    /// precise walker. `sigil_alloc` writes this on entry (before
    /// calling into libgc) so the `push_other_roots` callback —
    /// which runs from inside libgc's mark phase, where the
    /// current FP is somewhere inside libgc's internal frames
    /// (possibly compiled with `-fomit-frame-pointer`) — can walk
    /// from the LAST KNOWN Sigil-emitted frame instead of from
    /// libgc's internal call chain.
    ///
    /// `null` when no Sigil alloc is in progress on this thread.
    /// The callback short-circuits to "no precise roots from this
    /// thread" in that case. Today this matters in two cases:
    /// (a) a non-Sigil thread triggered GC (drainer-spawned
    /// allocation, future); (b) main thread's startup phase
    /// before the first sigil_alloc fires.
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
/// **Task 12 TODO — parallel-marker safety.** Today Boehm runs
/// single-marker (Task 11 deliberately doesn't call
/// `GC_allow_register_threads` to keep it that way), so the
/// callback always fires on the install thread and the
/// `static mut` read is data-race-free. When Task 12 reinstates
/// enrolment and parallel markers become live, the callback may
/// fire from any marker thread concurrent with the install (if
/// the install ever moves after startup — it doesn't today, but
/// future profile-data hooks or similar may push it). Migrate
/// to `AtomicUsize` + `transmute` (or a `OnceLock` holding the
/// proc pointer) when that happens.
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
///
/// **Task 12 rename TODO.** The name maps to the future
/// (post-Task-12) shape, when this function actually enrolls
/// the calling thread for conservative scan via Boehm's API.
/// Today the function body is pure Once-gated process-state
/// initialisation — `ensure_gc_process_state_initialised()`
/// would be the truer name. Kept as `register_*` to give Task
/// 12 a stable call-site shape (`cpu.rs` already calls it;
/// renaming today would mean renaming again at Task 12). When
/// Task 12 reinstates the Boehm enrolment, fold this rename
/// concern: either split (a) `ensure_gc_process_state_initialised`
/// vs (b) `register_runtime_thread_for_conservative_roots`,
/// where (a) is the process-wide bootstrap and (b) is the
/// per-thread Boehm enrolment; OR keep the name and let it
/// match its docs once enrolment is real.
pub fn register_runtime_thread_for_conservative_roots() {
    install_push_other_roots_once();
    crate::stackmap::prewarm_for_stw();
    // Plan E2 Phase 3 Task 12 — DO NOT call `GC_register_my_thread`
    // for runtime threads. The PR-#170 spike concluded the same
    // for a separate reason (parallel markers), but the conclusion
    // holds post-Task-12 for a clearer reason:
    //
    //   The drainer thread does not allocate from Boehm and does
    //   not hold Boehm pointers on its stack — its data is samples
    //   (POD), `Vec<Sample>` on system malloc, and a lock-free
    //   ring. Boehm's STW does not need to suspend it, and
    //   Boehm's mark phase does not need to scan its stack.
    //   Enrolling it would also leak the registration on thread
    //   exit (the drainer is `std::thread::spawn`'d, which doesn't
    //   route through `GC_pthread_create`'s auto-cleanup hook;
    //   CI surfaced this empirically — CPU-profile e2e tests
    //   crashed before printing anything, while alloc-profile
    //   tests (whose drainer was never enrolled) passed cleanly).
    //
    // IS_SIGIL_THREAD stays at its default `false` so the precise
    // walker callback short-circuits on this thread.
}

/// `GC_set_push_other_roots` callback. Boehm invokes this once
/// per mark phase, from whatever thread holds the GC lock.
///
/// **Task 11's callback is intentionally a no-op for now.** PR
/// #170 CI surfaced that walking the FP chain from inside this
/// callback SIGSEGVs alloc-heavy workloads (`tree.sigil` exit
/// code -1 with empty stdout). Root cause: the walker reads
/// `current_caller_fp` then traverses the chain via
/// `*fp` (saved_fp) reads (`stackmap::walk_for_gc_with_callback`).
/// When this callback is invoked from inside Boehm's mark
/// phase, the call chain passes through libgc internal
/// functions — and libgc 8.x on both target hosts is compiled
/// with optimisations that may omit frame pointers from
/// internal frames. Walking through a frame-pointer-omitted
/// libgc frame reads garbage as the next `saved_fp` value, and
/// the subsequent `walk_frame` call dereferences an arbitrary
/// address → SIGSEGV.
///
/// The Phase 3 design's `GC_do_blocking` + `GC_call_with_gc_active`
/// boundary is the safe re-entry point: `sigil_alloc` would
/// capture the user-level FP at the active-state boundary
/// (where Sigil-emitted frames are still on top of the
/// call chain, with conventional FP layout), and this
/// callback would walk from THAT captured FP rather than
/// `current_caller_fp` (which is somewhere inside libgc when
/// the callback fires). That boundary is Task 12's territory
/// — it ships the `GC_do_blocking` wrapping the captured-FP
/// mechanism needs.
///
/// Until then, this callback is a no-op. The runtime side
/// effect of Task 11 is therefore:
/// - The discriminator API (`IS_SIGIL_THREAD` thread-local,
///   the `register_*` functions) exists for Task 12 to
///   build on.
/// - The push_other_roots callback IS installed via
///   `Once`-gated setter, so Task 12's wiring doesn't need
///   to repeat the `gc_mark.h:309` external-sync mitigation.
/// - The stackmap module's lazy initialisers ARE pre-warmed
///   so future STW-time walks don't lazy-init inside the
///   mark phase (the prewarm fix from commit 620a891 stays).
///
/// When Task 12 adds the captured-FP mechanism, the body of
/// this function changes to read the captured FP + invoke
/// `stackmap::walk_for_gc_with_callback_from(fp, …)` (a
/// follow-up variant that takes the starting FP as a
/// parameter instead of reading the current one). The
/// boundary outside libgc → libgc internal frames don't get
/// walked.
/// Plan E2 Phase 3 Task 12 — Sigil-side FP-capture hook. Called
/// from `sigil_alloc` (only when the calling thread is a Sigil
/// thread) BEFORE the call into libgc, so the captured FP is
/// outside any libgc internal frames.
///
/// The captured FP is the FP of `sigil_alloc`'s caller — i.e.,
/// the Sigil-emitted function that allocated. The mark-phase
/// callback walks UP from there through the Sigil call chain.
///
/// Cheap: one TLS write per alloc. Sigil alloc-heavy workloads
/// pay ~ns/alloc for this.
#[inline]
pub fn capture_sigil_caller_fp(fp: *const usize) {
    CAPTURED_SIGIL_CALLER_FP.with(|c| c.set(fp));
}

/// Clear the captured FP. Called by `sigil_alloc` AFTER the
/// allocation returns, so a subsequent GC triggered from
/// non-Sigil code (e.g., a runtime worker that allocated) sees
/// `null` and short-circuits the walker.
#[inline]
pub fn clear_sigil_caller_fp() {
    CAPTURED_SIGIL_CALLER_FP.with(|c| c.set(std::ptr::null()));
}

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

    // Plan E2 Phase 3 Task 12 — walk the Sigil thread's stack
    // precisely. Gate on `IS_SIGIL_THREAD` AND the captured FP
    // being non-null: the callback fires once per mark phase
    // on whichever thread holds the GC lock, and that thread
    // may or may not be a Sigil thread + may or may not have
    // an alloc in progress.
    let is_sigil = IS_SIGIL_THREAD.with(Cell::get);
    if !is_sigil {
        return;
    }
    let captured_fp = CAPTURED_SIGIL_CALLER_FP.with(Cell::get);
    if captured_fp.is_null() {
        // No alloc in progress on this thread — nothing to
        // walk. This happens at process startup (before the
        // first sigil_alloc) and during teardown.
        return;
    }
    // Walk from the captured FP — outside libgc's call chain,
    // so frame-pointer-omitted libgc frames are not on the
    // traversal path. The walker yields each precise root via
    // the closure, which calls `GC_push_all_eager` (mark-stack-
    // safe, no allocation) per root.
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
