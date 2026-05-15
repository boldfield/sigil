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
use std::sync::{Once, OnceLock};

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

    /// Plan E2 Phase 3 alloc-trampoline-elision — TLS shadow of
    /// "this thread is inside `GC_do_blocking`". Set to `true` by
    /// [`GcBlockingGuard::enter`] immediately before the
    /// `GC_do_blocking` call; cleared by the guard's `Drop` impl
    /// immediately after. Read by `gc::alloc_dispatch_active`'s
    /// fast path to decide whether the `GC_call_with_gc_active`
    /// wrap can be safely elided — if the thread is NOT blocking,
    /// it's already in GC-active state and the wrap is wasted work.
    ///
    /// Boehm does not expose a "am I blocking" predicate publicly,
    /// so this TLS slot is the cheapest replacement. The cost of a
    /// wrong reading is severe: an elided wrap on a parked thread
    /// puts Boehm's precise walker against inconsistent thread
    /// state. Every production path that calls `GC_do_blocking`
    /// MUST go through [`GcBlockingGuard`] — there is one such site
    /// today (`handlers::sigil_run_loop`'s blocking trampoline);
    /// any future addition needs the guard too. The audit grep
    /// `\bGC_do_blocking\b` in the runtime + compiler crates is the
    /// surface to keep in sync.
    pub(crate) static IS_THREAD_GC_BLOCKING: Cell<bool> = const { Cell::new(false) };
}

/// Read the current thread's GC-blocking flag. Used by
/// `gc::alloc_dispatch_active`'s elision fast path; also exposed for
/// tests that drive the [`GcBlockingGuard`] directly.
#[inline]
pub(crate) fn is_thread_gc_blocking() -> bool {
    IS_THREAD_GC_BLOCKING.with(Cell::get)
}

/// RAII guard around a `GC_do_blocking` region. Sets
/// `IS_THREAD_GC_BLOCKING = true` on construction and restores the
/// PRIOR value on `Drop`. The save/restore shape (rather than a bare
/// `set(false)`) is load-bearing for nested `sigil_run_loop` calls:
/// `GC_do_blocking` is documented as stack-disciplined (`gc.h:1626`)
/// and may be re-entered, so a Sigil program that calls back into
/// `sigil_run_loop` from inside a handler arm produces two stacked
/// guards. A naive set/clear would let the inner guard's `Drop`
/// clear the flag while the outer scope is still parked, which is
/// exactly the misfire the plan's correctness rule forbids — Boehm's
/// precise walker would fire against inconsistent thread state and
/// crash. The same idiom is used by `handlers::sigil_run_loop_impl`
/// for `RUN_LOOP_ENTRY_DEPTH` (`handlers.rs:2320`).
///
/// Use shape:
/// ```ignore
/// let _gc_blocking = GcBlockingGuard::enter();
/// unsafe { GC_do_blocking(trampoline, ctx); }
/// // _gc_blocking drops here, restoring the prior IS_THREAD_GC_BLOCKING.
/// ```
///
/// `enter`/`drop` order MUST bracket the `GC_do_blocking` call: the
/// TLS shadow must be `true` for the entire interval the thread is
/// actually parked in Boehm's "inactive" state. A misalignment in
/// either direction (set too late, cleared too early) misfires the
/// alloc-trampoline elision.
#[must_use = "GcBlockingGuard must be held for the lifetime of the GC_do_blocking call; binding to `_` drops it immediately"]
pub(crate) struct GcBlockingGuard {
    prior: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl GcBlockingGuard {
    #[inline]
    pub(crate) fn enter() -> Self {
        let prior = IS_THREAD_GC_BLOCKING.with(|f| f.replace(true));
        Self {
            prior,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for GcBlockingGuard {
    #[inline]
    fn drop(&mut self) {
        IS_THREAD_GC_BLOCKING.with(|f| f.set(self.prior));
    }
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
/// Captured at install time + invoked from our wrapper. Storing
/// in a `OnceLock` provides the same set-once happens-before
/// edge as the `static mut` it replaced (PR #171 re-review N1
/// follow-up) without the unsafe-read at every callback
/// invocation, AND is forward-compatible if a future plan
/// enables parallel markers — `OnceLock::get` is safe to call
/// concurrently from multiple marker threads. Today only the
/// single marker thread reads it (Task 12 keeps Boehm
/// single-marker via `GC_set_markers_count(1)`), so either
/// shape works correctness-wise; `OnceLock` is the modern
/// Rust shape and removes the only `unsafe` read in this
/// module that wasn't FFI.
///
/// **Empty `OnceLock` semantics.** If Boehm has no prior proc
/// installed at the time we capture (the common case when our
/// `GC_set_push_other_roots` is the first call), `GC_get_push_other_roots`
/// returns `None` and we LEAVE the OnceLock empty rather than
/// setting it. The callback then sees `PRIOR_PUSH_OTHER_ROOTS.get() == None`
/// and skips the chain call — same behaviour as the prior
/// `Option<GcPushOtherRootsProc>` shape.
static PRIOR_PUSH_OTHER_ROOTS: OnceLock<GcPushOtherRootsProc> = OnceLock::new();

fn install_push_other_roots_once() {
    PRECISE_WALKER_INSTALLED.call_once(|| {
        // SAFETY (FFI boundaries only): `Once::call_once`
        // guarantees exactly one installation per process,
        // satisfying `gc_mark.h:309`'s external-sync requirement.
        // The proc has `'static` lifetime; Boehm holds it for
        // the process's life. We capture the prior proc BEFORE
        // the setter so our wrapper can chain to it (see
        // `push_sigil_thread_precise_roots` doc +
        // `PRIOR_PUSH_OTHER_ROOTS` doc).
        let prior = unsafe { GC_get_push_other_roots() };
        if let Some(prior) = prior {
            // Best-effort set; under the Once guard a previous
            // set is impossible, so this won't return Err in
            // production. `let _ = ...` swallows the Result for
            // defensiveness against future refactors.
            let _ = PRIOR_PUSH_OTHER_ROOTS.set(prior);
        }
        unsafe {
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
    // installed; calling it is the documented contract.
    // `OnceLock::get` is safe; it returns `Some(&proc)` after
    // the install completed (happens-before via `Once::call_once`
    // → `GC_set_push_other_roots` → callback can fire) or
    // `None` if Boehm had no prior proc to chain to.
    if let Some(prior) = PRIOR_PUSH_OTHER_ROOTS.get() {
        prior();
    }

    // Plan E2 Phase 3 GC-time follow-up — time the walker body so
    // the throughput follow-up doc can decompose Phase 3's net
    // effect into (conservative-scan-savings) - (walker-cost).
    // The snapshot is taken AFTER the chained prior-proc call —
    // that proc is Boehm's internal hook (TLS roots + dynamic-
    // library roots) and its cost is not Phase 3's overhead.
    //
    // Per-call cost: two `Instant::now()` reads + a relaxed atomic
    // add (~50 ns). Always-on (not env-gated) — the cost is below
    // the noise floor on alloc-heavy workloads and gating would add
    // config complexity for marginal savings. See the design doc's
    // §"Key decisions" item 3.
    //
    // Increment on every exit path (gate-short-circuit and walked-
    // body alike) so the counter reflects the actual wall-clock
    // cost of the callback — including the gate checks themselves,
    // which are small but non-zero.
    let walker_start = std::time::Instant::now();

    // Discriminator gates per the doc above: a `false`
    // IS_SIGIL_THREAD or a null captured FP both mean "no
    // precise roots to supply from this thread's call chain".
    let is_sigil = IS_SIGIL_THREAD.with(Cell::get);
    if !is_sigil {
        crate::counters::add(
            crate::counters::CounterId::PreciseWalkerNs,
            walker_start.elapsed().as_nanos() as u64,
        );
        return;
    }
    let captured_fp = CAPTURED_SIGIL_CALLER_FP.with(Cell::get);
    if captured_fp.is_null() {
        crate::counters::add(
            crate::counters::CounterId::PreciseWalkerNs,
            walker_start.elapsed().as_nanos() as u64,
        );
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
    crate::counters::add(
        crate::counters::CounterId::PreciseWalkerNs,
        walker_start.elapsed().as_nanos() as u64,
    );
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
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
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

    // ============ Plan E2 alloc-trampoline-elision tests ===================

    #[test]
    fn fresh_thread_starts_with_is_thread_gc_blocking_false() {
        // The thread-local default is `false`. A fresh cargo-test
        // worker that has never wrapped a `GC_do_blocking` call
        // must observe `false` — anything else means the elision
        // would misfire by skipping the wrap on a thread that
        // never actually parked. Runs on a fresh worker thread
        // per `#[test]`; no subprocess isolation needed since the
        // TLS slot is per-thread.
        std::thread::spawn(|| {
            assert!(!is_thread_gc_blocking());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn gc_blocking_guard_sets_flag_for_scope_then_clears_on_drop() {
        // Scope-bounded set/clear. The guard is the SOLE way
        // production code mutates `IS_THREAD_GC_BLOCKING` — this
        // pins both the enter (set true) and drop (set false) sides
        // in one shape. Runs on a fresh thread so any leak across
        // the assertion surfaces as a hard failure rather than
        // contaminating later tests on the same worker.
        std::thread::spawn(|| {
            assert!(!is_thread_gc_blocking(), "precondition: starts cleared");
            {
                let _guard = GcBlockingGuard::enter();
                assert!(is_thread_gc_blocking(), "guard scope sets the flag");
            }
            assert!(!is_thread_gc_blocking(), "drop clears the flag");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn gc_blocking_guard_clears_flag_on_panic_unwind() {
        // The guard's Drop runs on panic unwind, so the flag
        // clears even when the wrapped operation panics. Without
        // this, a panic inside `GC_do_blocking`'s body fn would
        // leave the TLS shadow stuck at `true` for the rest of
        // the thread's lifetime, permanently disabling elision
        // even after the panic was caught somewhere above.
        // `catch_unwind` is the standard verification shape for
        // RAII-on-unwind guarantees.
        std::thread::spawn(|| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = GcBlockingGuard::enter();
                assert!(is_thread_gc_blocking());
                panic!("simulated panic inside blocking region");
            }));
            assert!(
                !is_thread_gc_blocking(),
                "panic unwind must drop the guard and clear the flag"
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn gc_blocking_guard_nests_via_stack_save_restore() {
        // `sigil_run_loop` may be re-entered from inside a handler
        // arm; `GC_do_blocking` is documented as stack-disciplined
        // (`gc.h:1626`). Two stacked guards must keep the flag
        // `true` across the inner scope's Drop and only restore
        // `false` at the outermost Drop. This pins the save/restore
        // shape: a bare `set(false)` on Drop would clear the flag
        // after the inner guard exits while the outer scope is
        // still parked — the exact correctness regression the
        // plan's "no misfire" rule forbids.
        std::thread::spawn(|| {
            assert!(!is_thread_gc_blocking(), "precondition: starts cleared");
            let outer = GcBlockingGuard::enter();
            assert!(is_thread_gc_blocking());
            {
                let _inner = GcBlockingGuard::enter();
                assert!(is_thread_gc_blocking());
            }
            assert!(
                is_thread_gc_blocking(),
                "outer scope must still observe true after inner drop \
                 (save/restore guard, not a bare set/clear)"
            );
            drop(outer);
            assert!(
                !is_thread_gc_blocking(),
                "outermost drop restores the original false"
            );
        })
        .join()
        .unwrap();
    }
}
