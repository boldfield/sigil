//! Allocation profiler — plan 2026-05-08-sigil-v2-runtime-profile-data
//! Phase 4, Task 6.
//!
//! Gated by `SIGIL_ALLOC_PROFILE=<path>`. When set, the runtime:
//!
//! 1. Allocates a global sample ring + sample sink (same shape as
//!    [`super::cpu`]).
//! 2. Sets [`ALLOC_PROFILE_ENABLED`] so `sigil_alloc` invokes the
//!    sampler hook.
//! 3. Spawns a drainer thread that polls the ring every 10 ms.
//! 4. Registers an `atexit` hook for final flush + write.
//!
//! The fast path inside `sigil_alloc` is a single relaxed atomic
//! load + branch when profiling is off, so the cold case is
//! effectively free. When on, every `SIGIL_ALLOC_SAMPLE_RATE`-th
//! allocation triggers a stack walk via the Phase 2 walker. Note
//! that this hook is **not in a signal handler**; allocation is
//! permitted inside the sampler itself, but we still avoid it to
//! keep the hook fast on the warm path.

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::profile::ring::Ring;
use crate::profile::sample::{Sample, SampleKind};
use crate::profile::unwind;

const DEFAULT_SAMPLE_RATE: u64 = 512;
const DRAINER_POLL_MS: u64 = 10;

/// Allocation-profile gate. `sigil_alloc` reads this with `Relaxed`
/// load + branch; the fast path is a single test-and-jump when off.
pub static ALLOC_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Sample rate — sample every Nth allocation. Loaded once at init.
static SAMPLE_RATE: AtomicU64 = AtomicU64::new(DEFAULT_SAMPLE_RATE);

/// Per-process allocation counter. `sigil_alloc` increments;
/// sampling fires every Nth increment.
static ALLOC_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Pointer to the leaked allocation sample ring.
static ALLOC_RING_PTR: AtomicPtr<Ring> = AtomicPtr::new(core::ptr::null_mut());

static SAMPLES: OnceLock<Mutex<Vec<Sample>>> = OnceLock::new();
static DRAINER_STOP: AtomicBool = AtomicBool::new(false);
static OUTPUT_PATH: OnceLock<String> = OnceLock::new();

/// Initialise the allocation profiler if `SIGIL_ALLOC_PROFILE` is
/// set. Idempotent.
pub fn maybe_init() -> bool {
    static INIT_ONCE: OnceLock<bool> = OnceLock::new();
    *INIT_ONCE.get_or_init(|| {
        let path = match std::env::var("SIGIL_ALLOC_PROFILE") {
            Ok(p) if !p.is_empty() => p,
            _ => return false,
        };
        let rate: u64 = std::env::var("SIGIL_ALLOC_SAMPLE_RATE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n: &u64| *n >= 1)
            .unwrap_or(DEFAULT_SAMPLE_RATE);

        let _ = OUTPUT_PATH.set(path);
        let _ = SAMPLES.set(Mutex::new(Vec::new()));
        SAMPLE_RATE.store(rate, Ordering::Release);

        let ring: &'static Ring = Box::leak(Box::new(Ring::new()));
        ALLOC_RING_PTR.store(ring as *const Ring as *mut Ring, Ordering::Release);

        ALLOC_PROFILE_ENABLED.store(true, Ordering::Release);

        std::thread::Builder::new()
            .name("sigil-alloc-drainer".into())
            .spawn(drainer_loop)
            .ok();

        // SAFETY: `atexit(3)` only requires the callback pointer to
        // outlive the process; the cb is a static fn.
        unsafe {
            atexit(alloc_atexit_cb);
        }

        true
    })
}

/// Sampler hook invoked from `sigil_alloc` on the slow path. NOT
/// in a signal handler. `size_bytes` is the requested allocation
/// size (used as the sample's `value`).
///
/// Inlined at the call site so the hot branch (no-sample) is a
/// single counter increment + comparison.
#[inline]
pub fn maybe_sample_alloc(size_bytes: u64) {
    // Hot path: profile disabled — single relaxed load + branch.
    if !ALLOC_PROFILE_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let n = ALLOC_COUNTER.fetch_add(1, Ordering::Relaxed);
    let rate = SAMPLE_RATE.load(Ordering::Relaxed);
    if rate == 0 {
        return;
    }
    if !n.is_multiple_of(rate) {
        return;
    }

    let ring_ptr = ALLOC_RING_PTR.load(Ordering::Acquire);
    if ring_ptr.is_null() {
        return;
    }

    let mut frames = [0usize; unwind::MAX_DEPTH];
    // SAFETY: live thread with frame pointers preserved (cf. the
    // `-C force-frame-pointers=yes` setting in `.cargo/config.toml`).
    let depth = unsafe { unwind::capture_stack(&mut frames) };
    if depth == 0 {
        return;
    }

    let sample = Sample {
        ts_ns: std::time::Instant::now().elapsed().as_nanos() as u64,
        // Multiply by rate so the recorded value represents the
        // *unsampled* allocation volume — pprof renders this as
        // "bytes allocated" with rate-weighted correctness.
        value: size_bytes.saturating_mul(rate),
        depth: depth as u32,
        kind: SampleKind::Alloc,
        frames,
    };

    // SAFETY: leaked-static for process lifetime.
    let ring: &Ring = unsafe { &*ring_ptr };
    ring.try_push(sample);
}

fn drainer_loop() {
    let ring_ptr = ALLOC_RING_PTR.load(Ordering::Acquire);
    if ring_ptr.is_null() {
        return;
    }
    // SAFETY: leaked-static.
    let ring: &'static Ring = unsafe { &*ring_ptr };

    loop {
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

extern "C" fn alloc_atexit_cb() {
    ALLOC_PROFILE_ENABLED.store(false, Ordering::Release);
    DRAINER_STOP.store(true, Ordering::Release);

    let ring_ptr = ALLOC_RING_PTR.load(Ordering::Acquire);
    if !ring_ptr.is_null() {
        // SAFETY: leaked-static.
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
        let dropped = ring.dropped_count();
        if dropped > 0 {
            eprintln!(
                "sigil profile: dropped {dropped} allocation samples (ring full); \
                 consider raising SIGIL_ALLOC_SAMPLE_RATE"
            );
        }
    }

    if let Some(path) = OUTPUT_PATH.get() {
        crate::profile::output::write_alloc_profile(path.as_str());
    }
}

/// Drain and return the accumulated allocation samples.
pub fn take_samples() -> Vec<Sample> {
    if let Some(global) = SAMPLES.get() {
        if let Ok(mut g) = global.lock() {
            return std::mem::take(&mut *g);
        }
    }
    Vec::new()
}

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_init_returns_false_when_env_unset() {
        std::env::remove_var("SIGIL_ALLOC_PROFILE");
        assert!(!maybe_init());
        assert!(!ALLOC_PROFILE_ENABLED.load(Ordering::Acquire));
    }

    #[test]
    fn maybe_sample_alloc_is_noop_when_disabled() {
        // ALLOC_PROFILE_ENABLED defaults to false; the hook should
        // bail before doing any work.
        let counter_before = ALLOC_COUNTER.load(Ordering::Relaxed);
        maybe_sample_alloc(123);
        let counter_after = ALLOC_COUNTER.load(Ordering::Relaxed);
        // Counter must NOT increment when disabled — the hot path
        // is single-branch early-return.
        assert_eq!(counter_before, counter_after);
    }
}
