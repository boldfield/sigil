//! Random number runtime primitives — Plan C Task 75.
//!
//! Sigil v1's `Random` effect (declared in `std/random.sigil`)
//! delegates to runtime primitives for the actual entropy source.
//! The `os_random` handler in the stdlib file calls
//! `sigil_random_os_int` to satisfy each `Random.rand_int(k)` arm
//! with an OS-sourced 63-bit value.
//!
//! Why 63-bit instead of 64-bit: Sigil's `Int` is i64 with the high
//! bit reserved for the value/heap discriminator at the runtime
//! Value level (per `runtime/src/value.rs`). Surfacing only 63
//! bits keeps the sigil-level type clean. v2 `Int64` (Plan C Task
//! 69) gives access to the full 64-bit space.
//!
//! ## Seeded variant
//!
//! The plan body's `seeded(Int64)` handler is deferred to a Task 75
//! follow-up alongside Int64 (Task 69). The skeleton lives in the
//! `std/random.sigil` documentation; an actual implementation would
//! carry PRNG state via a `MutArray[Int]` in the handler's closure
//! and step it on each draw.

use std::sync::Mutex;

/// Lightweight thread-safe PRNG state initialised once per process
/// from `std::time::SystemTime` at first use. Not cryptographically
/// secure; intended for the v1 `Random` effect's `os_random`
/// handler. v2 may swap this for a `getrandom(2)` / `BCryptGenRandom`
/// entropy source for security-sensitive callers.
static OS_RANDOM_STATE: Mutex<Option<u64>> = Mutex::new(None);

#[inline]
fn xorshift64_next(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Returns a fresh 63-bit non-negative `i64` drawn from a
/// process-global xorshift64 PRNG seeded once at first use from the
/// system clock.
///
/// # Safety
///
/// Safe to call from any thread; the state mutex is acquired
/// internally.
#[no_mangle]
pub extern "C" fn sigil_random_os_int() -> i64 {
    // The `Mutex::lock()` Result type encodes poisoning. The PRNG
    // mutex is only ever held inside this function; a panic here
    // means a poisoner has already taken the process down. Recover
    // the inner value either way (poisoned data is just stale state
    // — non-cryptographic, so reusing it is harmless).
    let mut guard = match OS_RANDOM_STATE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let state = guard.get_or_insert_with(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_nanos() as u64) | 0x1)
            .unwrap_or(0xDEAD_BEEF_CAFE_F00D);
        // Mix in process ID so two simultaneously-launched programs
        // don't return the same opening sequence.
        now ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    });
    let raw = xorshift64_next(state);
    // Mask to 63 bits — Sigil's Int is i64 with the top bit reserved
    // at the Value level; non-negative is also more useful for typical
    // user code (modulo to a range, etc.).
    (raw & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn random_returns_non_negative_63bit() {
        for _ in 0..100 {
            let n = sigil_random_os_int();
            assert!(n >= 0, "{n} is negative");
            assert!((n as u64) < (1u64 << 63));
        }
    }

    #[test]
    fn random_changes_across_calls() {
        // Process-global state advances each call; identical
        // consecutive returns would indicate the PRNG is stuck.
        let a = sigil_random_os_int();
        let b = sigil_random_os_int();
        let c = sigil_random_os_int();
        assert!(a != b || b != c, "PRNG returned 3 identical values");
    }
}
