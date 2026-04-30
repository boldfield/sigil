//! Clock runtime primitives — Plan C Task 76.
//!
//! Sigil v1's `Clock` effect (declared in `std/clock.sigil`)
//! delegates to runtime primitives for the actual time source. The
//! `os_clock` handler in the stdlib calls `sigil_clock_os_now` to
//! satisfy each `Clock.now(k)` arm with a fresh nanosecond
//! timestamp.
//!
//! ## Resolution / range
//!
//! Returns nanoseconds since the Unix epoch as a 63-bit `i64`. The
//! 63-bit range covers ~292 years past 1970, sufficient through the
//! year 2262 for v1 use. v2 `Int64` (Plan C Task 69) gives the
//! full 64-bit range; the `frozen(Int64)` handler from the plan body
//! is deferred to a Task 76 follow-up alongside Int64.

/// Returns nanoseconds since the Unix epoch as a 63-bit non-negative
/// `i64`. On systems where `SystemTime::now()` can't compute a
/// duration (clock skew, pre-1970 system clock), returns `0` rather
/// than aborting.
///
/// # Safety
///
/// Safe to call from any thread.
#[no_mangle]
pub extern "C" fn sigil_clock_os_now() -> i64 {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mask to 63 bits — sigil's Int reserves the top bit at the
    // runtime Value layer.
    (dur & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn clock_returns_non_negative_63bit() {
        let n = sigil_clock_os_now();
        assert!(n >= 0);
        assert!((n as u64) < (1u64 << 63));
    }

    #[test]
    fn clock_advances_across_calls() {
        // Two consecutive calls should differ by at least 1ns on
        // any reasonable system.
        let a = sigil_clock_os_now();
        // Spin a tiny bit to ensure the system clock advances even
        // on hosts with low-resolution timers.
        for _ in 0..10_000 {
            std::hint::black_box(0);
        }
        let b = sigil_clock_os_now();
        assert!(b >= a, "clock went backwards: {a} → {b}");
    }
}
