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
//! Returns nanoseconds since the Unix epoch as a 63-bit non-negative
//! `i64`. Sigil's `Int` type reserves the top bit at the runtime
//! Value layer (per `runtime/src/value.rs`), so the surface is
//! 63 bits unsigned in [0, 2^63 − 1] — about 292.47 years of
//! nanoseconds past the Unix epoch. The implementation **saturates
//! explicitly** at `i64::MAX` for any value beyond that bound rather
//! than wrapping silently:
//!
//! - If the host's `SystemTime::now()` is at or before the Unix
//!   epoch (e.g. clock skew on a fresh boot), returns `0`.
//! - If the host's `SystemTime::now()` is past `1970-01-01 +
//!   (2^63 − 1) ns ≈ year 2262-04-11`, returns `i64::MAX`. The
//!   compiled program can detect saturation by comparing the
//!   result against `i64::MAX`. (Year 2262 is far enough out that
//!   v2's `Int64` (Task 69) is expected to ship long before the
//!   bound matters.)
//!
//! v2 `Int64` gives the full 64-bit range; the `frozen(Int64)`
//! handler from the plan body is deferred to a Task 76 follow-up
//! alongside Int64.

/// Returns nanoseconds since the Unix epoch as a 63-bit non-negative
/// `i64`. Saturates at `0` (epoch or earlier) and `i64::MAX` (past
/// year 2262) — see module docs for the saturation semantics.
///
/// # Safety
///
/// Safe to call from any thread.
#[no_mangle]
pub extern "C" fn sigil_clock_os_now() -> i64 {
    let dur_nanos: u128 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // 63-bit non-negative bound = 2^63 − 1.
    const MAX_NANOS: u128 = i64::MAX as u128;
    if dur_nanos > MAX_NANOS {
        i64::MAX
    } else {
        dur_nanos as i64
    }
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
    fn clock_does_not_go_backwards() {
        // Pin the monotonicity property without claiming a specific
        // resolution (Windows / qemu timer can stall at ns). The
        // assertion only catches the failure mode `b < a`.
        let a = sigil_clock_os_now();
        for _ in 0..10_000 {
            std::hint::black_box(0);
        }
        let b = sigil_clock_os_now();
        assert!(b >= a, "clock went backwards: {a} → {b}");
    }
}
