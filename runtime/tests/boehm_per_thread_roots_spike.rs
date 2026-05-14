// Plan E2 Phase 3 Task 10 — Boehm per-thread roots API spike.
//
// Pins the Boehm API surface Phase 3 Tasks 11 + 12 will use to:
//   1. Make Sigil program threads use precise stack roots (root
//      locations supplied by the Plan E2 Phase 1 stackmap walker).
//   2. Keep runtime-internal threads (Plan E1 profile drainer,
//      future runtime workers) on Boehm's default conservative
//      stack scan.
//
// The plan body's question — "does `GC_register_my_thread` support
// a per-thread precise-root callback?" — surveyed in
// `runtime/docs/boehm-per-thread-roots-spike.md` and answered:
// NO, the distinction is global. The workaround uses three
// orthogonal APIs:
//
//   GC_do_blocking          — opt the calling thread's frames out
//                             of conservative scan
//   GC_call_with_gc_active  — opt back in (per-call site)
//   GC_set_push_other_roots — supply root ranges to the marker
//
// This spike verifies the third leg's API contract: the registered
// `GcPushOtherRootsProc` callback IS invoked during the mark phase
// from `GC_gcollect`. Tasks 11 + 12 will hook the stackmap-driven
// walker into this callback site.
//
// The same subprocess-per-test discipline as the Phase 2 spike
// (`runtime/tests/boehm_precise_spike.rs`) — Boehm retains
// per-thread mark state across cargo-test thread tear-down on
// libgc 8.x, so each spike test runs in its own process.
//
// # What this spike does NOT test
//
// Validating that `GC_push_all_eager` actually keeps a pushed
// range alive across GC requires that range to live somewhere
// Boehm does NOT already auto-scan (data segment + registered
// thread stacks + GC_add_roots ranges). A first cut of this
// spike used a `static mut` as the "shadow root"; that's in
// the data segment, so Boehm's auto-scan already covered it
// and the test passed trivially without actually exercising
// the callback. The cleaner verification — heap-alloc the
// shadow root via libc `malloc`, register the callback to
// push that range — is Task 11 territory where we have actual
// stack frames excluded via `GC_do_blocking` and a real
// stackmap walker to plumb in. For Task 10 (an API spike), the
// "callback IS invoked" check is the load-bearing assertion.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

// Boehm FFI surface this spike pins. Each declaration is
// duplicated here rather than added to `runtime/src/gc.rs` —
// Tasks 11 + 12 fold the production versions in once the spike
// proves the API works on both target hosts.
#[link(name = "gc")]
extern "C" {
    fn GC_init();
    fn GC_gcollect();

    /// Per-thread Boehm enrolment. Same surface the Phase 2
    /// spike uses; replicated here for self-containment.
    fn GC_allow_register_threads();
    fn GC_register_my_thread(stack_base: *const c_void) -> i32;

    /// The Plan E2 Phase 3 mechanism: register a callback Boehm
    /// invokes during the mark phase. The callback can push
    /// additional root ranges via `GC_push_all_eager`.
    fn GC_set_push_other_roots(p: GcPushOtherRootsProc);

    /// Inverse getter for `GC_set_push_other_roots`. Used by the
    /// teardown step + by callbacks that want to chain to a
    /// previously-installed proc.
    fn GC_get_push_other_roots() -> Option<GcPushOtherRootsProc>;
}

type GcPushOtherRootsProc = extern "C" fn();

const SPIKE_INNER_VAR: &str = "SIGIL_PER_THREAD_ROOTS_SPIKE_INNER";

fn in_spike_subprocess() -> bool {
    std::env::var(SPIKE_INNER_VAR).is_ok()
}

fn run_spike_in_subprocess(test_name: &str) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("run_spike_in_subprocess: current_exe failed: {e}");
            std::process::abort();
        }
    };
    let status = match std::process::Command::new(&exe)
        .args(["--exact", test_name, "--nocapture"])
        .env(SPIKE_INNER_VAR, "1")
        .status()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("run_spike_in_subprocess: spawn `{test_name}` failed: {e}");
            std::process::abort();
        }
    };
    assert!(
        status.success(),
        "per-thread-roots spike `{test_name}` failed: {status}"
    );
}

const GC_SUCCESS: i32 = 0;
const GC_DUPLICATE: i32 = 1;

static GC_INIT: Once = Once::new();
static GC_ALLOW_REGISTER: Once = Once::new();

fn enrol_gc() {
    // SAFETY: each Once.call_once fires at most once per process.
    unsafe {
        GC_INIT.call_once(|| GC_init());
        GC_ALLOW_REGISTER.call_once(|| GC_allow_register_threads());
    }
    let rc = unsafe { GC_register_my_thread(std::ptr::null()) };
    assert!(
        rc == GC_SUCCESS || rc == GC_DUPLICATE,
        "GC_register_my_thread returned rc={rc}"
    );
}

/// Counter the callback increments. The callback receives no
/// client data argument (`GcPushOtherRootsProc` is `void(*)()`),
/// so we communicate via a process-wide atomic.
static CALLBACK_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_invocations() {
    CALLBACK_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn push_other_roots_callback_is_invoked_during_mark() {
    // Verifies the API contract: a callback installed via
    // `GC_set_push_other_roots` IS invoked from inside the mark
    // phase of `GC_gcollect`. This is the load-bearing
    // observation for Phase 3 — Tasks 11 + 12 plug the stackmap-
    // driven precise root walker into this callback site.
    //
    // What the assertion proves:
    //   - libgc 8.x honours `GC_set_push_other_roots` on both
    //     target hosts (ubuntu-24.04 / macos-14).
    //   - The callback fires from inside `GC_gcollect`'s mark
    //     phase, not at some deferred / async moment.
    //   - Multiple `GC_gcollect()` calls invoke the callback
    //     multiple times (counter monotonically increases).
    //
    // What the assertion does NOT prove:
    //   - That `GC_push_all_eager` from inside the callback
    //     actually retains arbitrary user-supplied root ranges.
    //     That's testable but requires the root range to live
    //     in memory Boehm doesn't auto-scan — covered by Task
    //     11's `runtime/src/gc.rs`-side end-to-end test.
    if !in_spike_subprocess() {
        run_spike_in_subprocess("push_other_roots_callback_is_invoked_during_mark");
        return;
    }
    enrol_gc();
    CALLBACK_INVOCATIONS.store(0, Ordering::SeqCst);

    // Install the callback.
    unsafe { GC_set_push_other_roots(count_invocations) };

    // Force two GC cycles. Each should invoke the callback at
    // least once.
    let before = CALLBACK_INVOCATIONS.load(Ordering::SeqCst);
    unsafe { GC_gcollect() };
    let after_first = CALLBACK_INVOCATIONS.load(Ordering::SeqCst);
    unsafe { GC_gcollect() };
    let after_second = CALLBACK_INVOCATIONS.load(Ordering::SeqCst);

    assert_eq!(
        before, 0,
        "callback was invoked before GC_gcollect — unexpected"
    );
    assert!(
        after_first >= 1,
        "GC_gcollect did not invoke the push_other_roots callback \
         (CALLBACK_INVOCATIONS went from {before} to {after_first})"
    );
    assert!(
        after_second > after_first,
        "second GC_gcollect did not invoke the callback again \
         (CALLBACK_INVOCATIONS stayed at {after_first} between cycles)"
    );

    // Teardown: clear the callback. If a future cargo-test
    // refactor re-uses test binaries across integration tests
    // (it doesn't today, but this is cheap defensiveness), the
    // next test wouldn't inherit our counter callback.
    unsafe { GC_set_push_other_roots(noop_push) };
}

#[test]
fn push_other_roots_getter_round_trips_setter() {
    // Sanity check for the getter side of the API. Tasks 11/12
    // will use the getter to chain into a previously-installed
    // proc (so a process that installs both a profile-data
    // callback AND the stackmap callback can coexist).
    if !in_spike_subprocess() {
        run_spike_in_subprocess("push_other_roots_getter_round_trips_setter");
        return;
    }
    enrol_gc();

    // Set a known proc and read it back. The getter returns
    // None when no callback is installed (= NULL function
    // pointer, which Rust models as `Option<fn>`).
    unsafe { GC_set_push_other_roots(count_invocations) };
    let read_back_opt = unsafe { GC_get_push_other_roots() };
    assert!(
        read_back_opt.is_some(),
        "GC_get_push_other_roots returned None after a setter call"
    );
    let read_back = match read_back_opt {
        Some(p) => p,
        None => unreachable!(),
    };

    // Pointer-equality check: the proc Boehm hands back should
    // be the one we installed. (Function pointer equality is
    // well-defined across `extern "C"` callbacks.)
    assert!(
        std::ptr::fn_addr_eq(read_back, count_invocations as GcPushOtherRootsProc),
        "GC_get_push_other_roots returned a different proc than installed"
    );

    // Teardown.
    unsafe { GC_set_push_other_roots(noop_push) };
}

extern "C" fn noop_push() {
    // No-op marker proc; used by the teardown steps + by
    // `push_other_roots_getter_round_trips_setter`'s teardown.
}
