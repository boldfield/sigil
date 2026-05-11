//! SPSC ring buffer for profile samples — plan 2026-05-08-sigil-v2-
//! runtime-profile-data Phase 3.
//!
//! Lock-free single-producer / single-consumer ring. The producer is
//! the SIGPROF signal handler (or, in Phase 4, the `sigil_alloc` hook
//! at sample boundaries); the consumer is the runtime's drainer
//! thread. The ring sits behind an `AtomicPtr` so the signal handler
//! can find it without taking a lock.
//!
//! Wraparound is detected via overflow-aware arithmetic on the
//! monotonically-increasing `head` / `tail` counters — capacity is
//! `RING_SIZE` and `head.wrapping_sub(tail)` gives the live count.
//!
//! When the ring is full, the producer **drops** the sample and bumps
//! the dropped-counter. The atexit writer surfaces the drop count in
//! the final report so a profile with visibly thin coverage has an
//! explanation.
//!
//! ## Signal-safety contract
//!
//! - The producer side ([`Ring::try_push`]) uses only relaxed and
//!   release atomic stores — no allocation, no libc call, no
//!   synchronisation primitive that could block.
//! - The producer writes `Sample` bytes via an `UnsafeCell` pointer
//!   *before* publishing the new `head`; the consumer's acquire load
//!   of `head` establishes the happens-before so it sees the bytes.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::profile::sample::Sample;

/// Ring capacity in samples. Power-of-two so `% RING_SIZE` is a fast
/// bit-and. 64 entries at ~99 Hz means the drainer has ~640ms before
/// the producer starts dropping — generous given the 10 ms drain
/// cadence the design targets.
pub const RING_SIZE: usize = 64;

/// SPSC ring buffer. Wrap in `Box::leak` for the global
/// always-live storage; the runtime never frees these (process
/// teardown reclaims).
#[repr(C)]
pub struct Ring {
    /// Backing storage. `UnsafeCell` because the producer (signal
    /// handler) and the consumer (drainer thread) each access slots
    /// without &mut Ring — synchronisation happens via the head/tail
    /// atomics, not Rust's aliasing rules.
    slots: [UnsafeCell<Sample>; RING_SIZE],
    /// Next free slot index for the producer (monotonic, wraps via
    /// modulo at use time).
    head: AtomicUsize,
    /// Next unread slot index for the consumer.
    tail: AtomicUsize,
    /// Count of samples the producer attempted to push when the
    /// ring was full. Reported by the drainer at flush.
    dropped: AtomicUsize,
}

// SAFETY: Ring is designed for one producer + one consumer accessing
// different slots concurrently, synchronised via head/tail atomics.
// The slot access pattern preserves the SPSC invariants.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    /// Construct an empty ring.
    pub fn new() -> Self {
        Self {
            slots: [const { UnsafeCell::new(Sample::zero()) }; RING_SIZE],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    /// Producer side: write `sample` into the next slot. Returns
    /// `true` on success, `false` if the ring is full (sample
    /// dropped, dropped-counter incremented).
    ///
    /// **Signal-safe.** All atomics are `Relaxed` / `Release`; the
    /// slot write is an in-place byte copy with no allocation.
    pub fn try_push(&self, sample: Sample) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= RING_SIZE {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let idx = head % RING_SIZE;
        // SAFETY: the SPSC invariant guarantees this slot is not
        // being read by the consumer (the consumer reads only at
        // indices `tail..head`; we're writing at `head`).
        unsafe {
            *self.slots[idx].get() = sample;
        }
        // Release-store publishes the slot bytes to the consumer.
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side: pop the next sample, if any.
    pub fn try_pop(&self) -> Option<Sample> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let idx = tail % RING_SIZE;
        // SAFETY: head > tail means the producer has finished
        // writing this slot (its release-store on head synchronised
        // with our acquire-load); we read the bytes by copy.
        let sample = unsafe { *self.slots[idx].get() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(sample)
    }

    /// Approximate live count. Reads `head` and `tail` separately
    /// without atomicity — the value may be stale but never
    /// negative-by-overflow.
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        head.wrapping_sub(tail).min(RING_SIZE)
    }

    /// Approximate "is empty" — true when the producer hasn't
    /// written anything past the consumer's tail. Stale by the same
    /// amount as [`Ring::len`].
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot the count of dropped (ring-full) samples.
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::profile::sample::SampleKind;

    fn mk_sample(value: u64) -> Sample {
        let mut s = Sample::zero();
        s.value = value;
        s.kind = SampleKind::Cpu;
        s
    }

    #[test]
    fn fresh_ring_is_empty() {
        let r = Ring::new();
        assert_eq!(r.len(), 0);
        assert!(r.try_pop().is_none());
    }

    #[test]
    fn push_pop_round_trips_values() {
        let r = Ring::new();
        for i in 0..8 {
            assert!(r.try_push(mk_sample(i)));
        }
        for i in 0..8 {
            let s = r.try_pop().expect("pop");
            assert_eq!(s.value, i);
        }
        assert!(r.try_pop().is_none());
    }

    #[test]
    fn full_ring_drops_samples_and_bumps_counter() {
        let r = Ring::new();
        for i in 0..RING_SIZE {
            assert!(r.try_push(mk_sample(i as u64)));
        }
        // Now full — next push drops.
        assert!(!r.try_push(mk_sample(999)));
        assert_eq!(r.dropped_count(), 1);
        for _ in 0..3 {
            assert!(!r.try_push(mk_sample(0)));
        }
        assert_eq!(r.dropped_count(), 4);
    }

    #[test]
    fn drain_after_full_then_push_more() {
        let r = Ring::new();
        for i in 0..RING_SIZE {
            assert!(r.try_push(mk_sample(i as u64)));
        }
        for _ in 0..(RING_SIZE / 2) {
            r.try_pop().unwrap();
        }
        for i in 0..(RING_SIZE / 2) {
            assert!(r.try_push(mk_sample(0xFF00 + i as u64)));
        }
        // Ring should be full again.
        assert!(!r.try_push(mk_sample(0)));
    }

    /// Concurrent producer + consumer at high volume. Pins the
    /// SPSC ordering: every produced value is delivered exactly
    /// once in arrival order. Non-deterministic threading; the test
    /// proves we don't lose or duplicate samples under load.
    #[test]
    fn spsc_concurrent_round_trip_is_lossless_and_in_order() {
        use std::sync::Arc;

        let r = Arc::new(Ring::new());
        let total: u64 = 10_000;

        let producer = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                let mut next: u64 = 0;
                while next < total {
                    let s = mk_sample(next);
                    if r.try_push(s) {
                        next += 1;
                    } else {
                        // Busy-wait if the ring is full — consumer
                        // will drain.
                        std::thread::yield_now();
                    }
                }
            })
        };

        let consumer = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                let mut expected: u64 = 0;
                while expected < total {
                    match r.try_pop() {
                        Some(s) => {
                            assert_eq!(s.value, expected, "out-of-order or dropped sample");
                            expected += 1;
                        }
                        None => std::thread::yield_now(),
                    }
                }
            })
        };

        producer.join().unwrap();
        consumer.join().unwrap();
        // The producer-side `dropped` counter reflects how many
        // try_push attempts hit a full ring (the producer retries
        // via yield_now). For an SPSC ring this is benign — every
        // produced value is eventually delivered exactly once. The
        // consumer's strict in-order assertion already covers the
        // correctness property; we don't pin a particular dropped
        // count because it depends on scheduler choices.
        assert!(r.try_pop().is_none(), "drained at end of test");
    }
}
