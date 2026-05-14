//! CPU profiler — plan 2026-05-08-sigil-v2-runtime-profile-data
//! Phase 3, Task 4.
//!
//! Gated by `SIGIL_CPU_PROFILE=<path>`. When set, the runtime:
//!
//! 1. Allocates a global [`Ring`] for sample storage.
//! 2. Installs a `SIGPROF` handler that walks the frame-pointer
//!    chain and pushes a [`Sample`] into the ring.
//! 3. Starts a `setitimer(ITIMER_PROF)` at `SIGIL_CPU_PROFILE_HZ`
//!    (default 99 Hz).
//! 4. Spawns a drainer thread that polls the ring every 10 ms and
//!    moves completed samples into a global mutex-protected
//!    [`Vec<Sample>`].
//! 5. Registers an `atexit` hook that disables sampling, joins the
//!    drainer, and invokes the Phase 5 writer with the accumulated
//!    samples.
//!
//! ## Zero-overhead path
//!
//! When `SIGIL_CPU_PROFILE` is unset, [`init`] returns immediately
//! after a single `std::env::var_os` lookup. No signal handler is
//! installed, no thread is spawned, no allocator is touched. The
//! `sigil_alloc` fast path (Phase 4) gates similarly on
//! `ALLOC_PROFILE_ENABLED`; this module sets neither.

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::profile::ring::{Ring, RING_SIZE};
use crate::profile::sample::{Sample, SampleKind};
use crate::profile::sys::{
    setitimer, sigaction, ucontext_fp, Itimerval, Sigaction, SigactionHandler, Timeval,
    ITIMER_PROF, SA_RESTART, SA_SIGINFO, SIGPROF,
};
use crate::profile::unwind;

/// Default sampling rate. Matches Go / Java's typical 100-Hz floor;
/// 99 Hz dodges any 100-Hz aliasing with periodic application work.
const DEFAULT_HZ: u32 = 99;

/// Drainer polling period — keeps the ring's worst-case occupancy
/// well under [`RING_SIZE`].
const DRAINER_POLL_MS: u64 = 10;

/// Global "profiling is active" flag. Read by both the signal handler
/// (relaxed acquire-ish: a stale `false` just means one missed
/// sample) and the alloc hook in Phase 4.
pub static CPU_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Process-wide sample sink. Populated by the drainer thread; read
/// by the atexit writer.
static SAMPLES: OnceLock<Mutex<Vec<Sample>>> = OnceLock::new();

/// Process-start instant for `ts_ns` computation. The signal
/// handler reads this without a lock — it's only ever written once
/// before SIGPROF is enabled.
static PROC_START_NS: AtomicUsize = AtomicUsize::new(0);

/// Pointer to the leaked CPU sample ring. `null` when profiling is
/// off; the SIGPROF handler bails on null.
static CPU_RING_PTR: AtomicPtr<Ring> = AtomicPtr::new(core::ptr::null_mut());

/// Drainer-loop stop flag — atexit sets this true, the drainer
/// observes and exits its loop.
static DRAINER_STOP: AtomicBool = AtomicBool::new(false);

/// Path the atexit writer should target. Populated from
/// `SIGIL_CPU_PROFILE` at init.
static OUTPUT_PATH: OnceLock<String> = OnceLock::new();

/// Initialise the CPU profiler if `SIGIL_CPU_PROFILE` is set. Idempotent
/// — wrapped in a `OnceLock`-like guard so the C-main shim's `sigil_gc_init`
/// call is safe.
///
/// Returns `true` if profiling was enabled, `false` if `SIGIL_CPU_PROFILE`
/// is unset (the zero-overhead path).
pub fn maybe_init() -> bool {
    static INIT_ONCE: OnceLock<bool> = OnceLock::new();
    *INIT_ONCE.get_or_init(|| {
        let path = match std::env::var("SIGIL_CPU_PROFILE") {
            Ok(p) if !p.is_empty() => p,
            _ => return false,
        };
        let hz: u32 = std::env::var("SIGIL_CPU_PROFILE_HZ")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| (1..=10_000).contains(n))
            .unwrap_or(DEFAULT_HZ);

        let _ = OUTPUT_PATH.set(path);
        let _ = SAMPLES.set(Mutex::new(Vec::new()));

        // Capture process-start time as the ts_ns origin.
        PROC_START_NS.store(now_ns_raw(), Ordering::Relaxed);

        // Leak the ring so the global pointer stays live for the
        // whole process. Reclamation is the OS's job at exit.
        let ring: &'static Ring = Box::leak(Box::new(Ring::new()));
        CPU_RING_PTR.store(ring as *const Ring as *mut Ring, Ordering::Release);

        // Install SIGPROF handler via `sigaction(SA_SIGINFO|SA_RESTART)`
        // so the handler receives `ucontext_t*` for the interrupted
        // thread. We then read the saved frame pointer directly from
        // the ucontext and pass it to the walker — this is the
        // migration from the original `signal(2)`-based approach
        // (PR #148 review item #4).
        //
        // SAFETY: `sigaction(2)` reads `&act` for the duration of the
        // call. The struct's flags / mask / handler are all populated
        // before the call. Passing `null_mut` for `oldact` discards
        // any previously-installed handler (the sigil runtime is the
        // sole owner of SIGPROF in this process).
        let act = Sigaction {
            sa_sigaction: sigprof_handler as SigactionHandler as usize,
            sa_flags: SA_SIGINFO | SA_RESTART,
            ..Sigaction::default()
        };
        let rc = unsafe { sigaction(SIGPROF, &act, core::ptr::null_mut()) };
        if rc != 0 {
            eprintln!("sigil profile: sigaction(SIGPROF) failed; profiling disabled");
            return false;
        }

        CPU_PROFILE_ENABLED.store(true, Ordering::Release);

        // Start the drainer thread. Ignore the JoinHandle — atexit
        // sets DRAINER_STOP and the drainer self-terminates; we
        // don't strictly need to join (process is exiting anyway).
        std::thread::Builder::new()
            .name("sigil-profile-drainer".into())
            .spawn(drainer_loop)
            .ok();

        // Arm the itimer.
        let usec_period: u64 = 1_000_000 / hz as u64;
        let it = make_itimerval(usec_period);
        // SAFETY: `setitimer(2)` reads `&it` for the duration of the
        // call; passing `null_mut` for old_value discards prior state.
        let rc = unsafe { setitimer(ITIMER_PROF, &it, core::ptr::null_mut()) };
        if rc != 0 {
            eprintln!("sigil profile: setitimer(ITIMER_PROF) failed; profiling disabled");
            CPU_PROFILE_ENABLED.store(false, Ordering::Release);
            return false;
        }

        // Register an atexit hook for the final flush + write.
        // SAFETY: `atexit(3)` only requires the callback pointer to
        // outlive the process; `cpu_atexit_cb` is a static fn.
        unsafe {
            atexit(cpu_atexit_cb);
        }

        true
    })
}

#[cfg(target_os = "linux")]
fn make_itimerval(usec_period: u64) -> Itimerval {
    let tv = Timeval {
        tv_sec: (usec_period / 1_000_000) as i64,
        tv_usec: (usec_period % 1_000_000) as i64,
    };
    Itimerval {
        it_interval: tv,
        it_value: tv,
    }
}

#[cfg(target_os = "macos")]
fn make_itimerval(usec_period: u64) -> Itimerval {
    let tv = Timeval {
        tv_sec: (usec_period / 1_000_000) as i64,
        tv_usec: (usec_period % 1_000_000) as i32,
        _pad: 0,
    };
    Itimerval {
        it_interval: tv,
        it_value: tv,
    }
}

/// SIGPROF handler with `SA_SIGINFO`. Signal-safe by construction:
/// - reads only relaxed/acquire atomics;
/// - reads the interrupted thread's saved fp from `ucontext_t` and
///   walks from there via the Phase 2 walker (no alloc, no libc);
/// - pushes into the lock-free [`Ring`].
///
/// The handler receives the kernel-supplied `siginfo_t*` and
/// `ucontext_t*` opaquely; only the `ucontext` pointer is used (to
/// recover the interrupted thread's frame pointer). Reading from the
/// walker's own fp instead of the ucontext's would put 2-3
/// trampoline / walker frames at the bottom of every sample, which
/// PR #148 review item #4 flagged.
extern "C" fn sigprof_handler(
    _sig: core::ffi::c_int,
    _info: *mut core::ffi::c_void,
    ucontext: *mut core::ffi::c_void,
) {
    // Bail early if profiling has been disabled between signal
    // arming and delivery (atexit path).
    if !CPU_PROFILE_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let ring_ptr = CPU_RING_PTR.load(Ordering::Acquire);
    if ring_ptr.is_null() {
        return;
    }

    // Recover the interrupted code's frame pointer from ucontext. If
    // we can't (null or unrecognised layout), fall back to the
    // walker's own fp — same behaviour as the original signal(2)
    // path, just slightly noisier samples.
    // SAFETY: ucontext is the kernel-supplied pointer for this
    // SA_SIGINFO delivery; reading the saved fp is a single aligned
    // load at a platform-pinned offset.
    let interrupted_fp = unsafe { ucontext_fp(ucontext) };

    // Capture stack via the Phase 2 walker.
    let mut frames = [0usize; unwind::MAX_DEPTH];
    // SAFETY: we're on a live thread with frame pointers preserved
    // (the runtime crate has -C force-frame-pointers=yes; the
    // cranelift-emitted user code has preserve_frame_pointers=true).
    let depth = if interrupted_fp != 0 {
        unsafe { unwind::capture_stack_from(interrupted_fp, &mut frames) }
    } else {
        unsafe { unwind::capture_stack(&mut frames) }
    };
    if depth == 0 {
        return;
    }

    let sample = Sample {
        ts_ns: now_ns_relative(),
        value: 1,
        depth: depth as u32,
        kind: SampleKind::Cpu,
        frames,
    };

    // SAFETY: `ring_ptr` was published with Release after the
    // `Box::leak` write and remains live for the whole process.
    let ring: &Ring = unsafe { &*ring_ptr };
    ring.try_push(sample);
}

/// Drainer thread body. Polls the ring at ~10 ms cadence; merges
/// samples into the global `SAMPLES` vec. Exits cleanly when
/// [`DRAINER_STOP`] is set.
fn drainer_loop() {
    // Plan E2 Phase 3 Task 11: register as "runtime-internal,
    // conservative roots" via the `gc::threads` discriminator.
    // The call is a no-op on Boehm state today (the drainer
    // doesn't allocate from Boehm; the API doesn't enroll the
    // thread until Task 12 chooses to) but pre-warms the
    // process-wide push_other_roots install + stackmap
    // initialisers exactly once per process regardless of
    // which thread (Sigil or runtime) registers first.
    //
    // **Constraint** (see `runtime/src/gc/threads.rs` module
    // doc → "Runtime invariant"): runtime-internal threads
    // MUST NOT call `sigil_alloc`. The drainer obeys this
    // (shuffles `Vec<Sample>` between a Rust SPSC ring and a
    // `Mutex<Vec<Sample>>` via the system allocator only); any
    // future worker added here must too. If a future worker
    // needs heap allocation, the `gc::threads` design needs a
    // multi-Sigil-thread registry walk before that worker
    // lands.
    crate::gc::threads::register_runtime_thread_for_conservative_roots();

    let ring_ptr = CPU_RING_PTR.load(Ordering::Acquire);
    if ring_ptr.is_null() {
        return;
    }
    // SAFETY: pointer stays valid for the whole process (leaked).
    let ring: &'static Ring = unsafe { &*ring_ptr };

    loop {
        // Drain everything currently available.
        let mut batch: Vec<Sample> = Vec::new();
        while let Some(s) = ring.try_pop() {
            batch.push(s);
        }
        if !batch.is_empty() {
            if let Some(global) = SAMPLES.get() {
                if let Ok(mut g) = global.lock() {
                    g.extend(batch);
                }
            }
        }
        if DRAINER_STOP.load(Ordering::Acquire) {
            // Final pass — handler may have pushed between the
            // store(stop) and our check.
            while let Some(s) = ring.try_pop() {
                if let Some(global) = SAMPLES.get() {
                    if let Ok(mut g) = global.lock() {
                        g.push(s);
                    }
                }
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(DRAINER_POLL_MS));
    }
}

/// `atexit` callback installed by `maybe_init`. Stops sampling,
/// drains the ring, hands off to the Phase 5 writer.
extern "C" fn cpu_atexit_cb() {
    // Disable sampling so handler invocations during teardown noop.
    CPU_PROFILE_ENABLED.store(false, Ordering::Release);

    // Disarm the itimer.
    let zero = Itimerval::default();
    // SAFETY: setitimer with a zeroed it_value disarms the timer;
    // ignoring rc — process is exiting either way.
    let _ = unsafe { setitimer(ITIMER_PROF, &zero, core::ptr::null_mut()) };

    // Signal drainer to stop. It will finish its final drain pass
    // and exit; we don't wait (the process is exiting; the drainer
    // will be torn down by the OS when main returns).
    DRAINER_STOP.store(true, Ordering::Release);

    // Direct final drain into SAMPLES from this thread too — the
    // drainer's sleep cadence may miss a few last samples on a
    // short-lived program.
    let ring_ptr = CPU_RING_PTR.load(Ordering::Acquire);
    if !ring_ptr.is_null() {
        // SAFETY: ring is leaked-static.
        let ring: &'static Ring = unsafe { &*ring_ptr };
        let mut tail: Vec<Sample> = Vec::new();
        while let Some(s) = ring.try_pop() {
            tail.push(s);
        }
        if !tail.is_empty() {
            if let Some(global) = SAMPLES.get() {
                if let Ok(mut g) = global.lock() {
                    g.extend(tail);
                }
            }
        }

        // Surface dropped samples in stderr so a thin profile has
        // a visible reason.
        let dropped = ring.dropped_count();
        if dropped > 0 {
            eprintln!(
                "sigil profile: dropped {dropped} CPU samples (ring full); \
                 consider raising RING_SIZE or lowering SIGIL_CPU_PROFILE_HZ"
            );
        }
    }

    // Hand off to the Phase 5 writer dispatcher. The dispatcher
    // reads OUTPUT_PATH, picks a format, and writes the samples.
    if let Some(path) = OUTPUT_PATH.get() {
        crate::profile::output::write_cpu_profile(path.as_str());
    }
}

/// Borrow the accumulated CPU samples. Used by the Phase 5 output
/// dispatcher.
pub fn take_samples() -> Vec<Sample> {
    if let Some(global) = SAMPLES.get() {
        if let Ok(mut g) = global.lock() {
            return std::mem::take(&mut *g);
        }
    }
    Vec::new()
}

/// Total number of samples held in the global vec (does not drain).
/// Used by integration tests.
pub fn buffered_sample_count() -> usize {
    SAMPLES
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.len()))
        .unwrap_or(0)
}

/// Number of dropped samples on the CPU ring. Used by tests + the
/// atexit reporter.
pub fn dropped_count() -> usize {
    let ring_ptr = CPU_RING_PTR.load(Ordering::Acquire);
    if ring_ptr.is_null() {
        return 0;
    }
    // SAFETY: leaked-static.
    let ring: &'static Ring = unsafe { &*ring_ptr };
    ring.dropped_count()
}

/// Capacity (samples) of the CPU ring. Exposed for tests + the
/// atexit reporter's diagnostic message.
pub const fn ring_capacity() -> usize {
    RING_SIZE
}

// `atexit(3)` from libc — pulled in via the same extern block
// `gc.rs` uses. Forward-declared here so this module compiles in
// isolation; the linker resolves to the same C symbol.
extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

fn now_ns_raw() -> usize {
    // Coarse monotonic source. `Instant::now()` calls into
    // `clock_gettime(CLOCK_MONOTONIC)` (Linux) or
    // `mach_absolute_time` (macOS) — both signal-safe per their
    // platform docs. Used outside the signal handler at init.
    let dur = std::time::Instant::now().elapsed();
    dur.as_nanos() as usize
}

fn now_ns_relative() -> u64 {
    // The signal handler computes the elapsed nanoseconds since
    // process start. We approximate by reading Instant::now()
    // again. `Instant::now()` in a signal handler is OK on the
    // platforms we target because the underlying syscall is signal-
    // safe; Rust's std wrapper does not allocate or panic on the
    // happy path.
    std::time::Instant::now().elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_init_returns_false_when_env_unset() {
        // Ensure clean env in case a sibling test polluted it.
        std::env::remove_var("SIGIL_CPU_PROFILE");
        assert!(!maybe_init());
        // No ring allocated, no flag set, no thread spawned.
        assert!(!CPU_PROFILE_ENABLED.load(Ordering::Acquire));
        assert!(CPU_RING_PTR.load(Ordering::Acquire).is_null());
    }

    #[test]
    fn ring_capacity_is_power_of_two() {
        let cap = ring_capacity();
        assert!(cap > 0 && cap.is_power_of_two());
    }
}
