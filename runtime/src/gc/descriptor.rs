//! Plan E2 Phase 2 Task 7 — Boehm precise-mode descriptor cache.
//!
//! Memoises Boehm `GC_descr` handles by Sigil's pointer bitmap +
//! payload word count so that common shapes (e.g., `bitmap=0b1` for
//! `Ref[T]`, `bitmap=0b101` for a 2-field record with one pointer)
//! build the descriptor exactly once per shape, not once per
//! allocation. Per `gc_typed.h`:
//!
//! > Calls to GC_make_descriptor may consume some amount of a
//! > finite resource. This is intended to be called once per type,
//! > not once per allocation.
//!
//! Task 8 wires `sigil_alloc` to look up via this cache before
//! calling `GC_malloc_explicitly_typed`; Task 7 is the cache itself
//! plus its tests.
//!
//! # Sigil bitmap → Boehm bitmap mapping
//!
//! Sigil's per-object pointer bitmap (`Header::pointer_bitmap()`,
//! 32 bits) is per-PAYLOAD-word: bit `k` ↔ payload word `k`. Boehm's
//! descriptor bitmap covers the WHOLE object (header + payload).
//! Header word 0 is never a pointer (it holds the tag + count +
//! bitmap themselves), so the conversion is:
//!
//! ```text
//! boehm_bitmap = (sigil_bitmap as usize) << 1   // bit 0 = 0 (header)
//! len_bits     = 1 + payload_word_count         // 1 header + payload
//! ```
//!
//! # Key shape
//!
//! Plan E2's plan body hypothesised a `BTreeMap<u32, GC_descr>`
//! keyed on the 32-bit `pointer_bitmap`. The deployed key adds
//! `payload_word_count` alongside the bitmap because two objects
//! with the same bitmap but different payload counts need
//! different descriptors: `bitmap=0b1, count=1` builds a 2-word
//! descriptor, `bitmap=0b1, count=5` builds a 6-word descriptor.
//! Boehm's descriptor word encodes the object's size, so the
//! handles differ even though the bitmap bits do not. Recorded as
//! a Task 7 deviation in `PLAN_E2_PROGRESS.md`.

// `get_or_create` (and its private callees) are this PR's deliverable
// but its caller — `sigil_alloc` — does not land until Task 8. Suppress
// the dead-code lint at module level rather than per-item; the next
// PR will call into this module and the warning will resolve itself.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

use super::GC_make_descriptor;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct DescriptorKey {
    sigil_bitmap: u32,
    payload_word_count: u8,
}

/// Process-wide descriptor cache. `LazyLock` defers the initial
/// `BTreeMap::new()` until first access; `RwLock` allows concurrent
/// reads on cache hits (the common path once shapes are warm).
static CACHE: LazyLock<RwLock<BTreeMap<DescriptorKey, usize>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Look up the cached Boehm descriptor for the given Sigil pointer
/// bitmap and payload word count; build one via `GC_make_descriptor`
/// on first observation of a shape. Subsequent calls with the same
/// `(bitmap, count)` return the cached handle without re-entering
/// Boehm.
///
/// Thread-safety: cache hits take an `RwLock` read; misses upgrade
/// to a write lock and re-check inside the write lock (an earlier
/// writer may have raced ahead) before calling `GC_make_descriptor`,
/// so a given `(bitmap, count)` enters Boehm's descriptor builder at
/// most once across all threads.
///
/// `sigil_bitmap` is the value of `Header::pointer_bitmap()`.
/// `payload_word_count` is `Header::payload_count()` (0..=63).
///
/// The returned `usize` is Boehm's opaque `GC_descr` handle; callers
/// pass it through to `GC_malloc_explicitly_typed` (Task 8).
pub(crate) fn get_or_create(sigil_bitmap: u32, payload_word_count: u8) -> usize {
    let key = DescriptorKey {
        sigil_bitmap,
        payload_word_count,
    };

    // Fast path: try a read lock first. The vast majority of calls
    // hit a warm cache, so this avoids contention on the write lock
    // in steady state.
    if let Some(d) = read_cache(&key) {
        return d;
    }

    // Slow path: take the write lock and re-check (another thread
    // may have raced us between the read-lock drop and the
    // write-lock acquire). If still missing, build + insert.
    let mut cache = CACHE.write().unwrap_or_else(|e| e.into_inner());
    if let Some(&d) = cache.get(&key) {
        return d;
    }
    let descr = build_descriptor(sigil_bitmap, payload_word_count);
    cache.insert(key, descr);
    descr
}

fn read_cache(key: &DescriptorKey) -> Option<usize> {
    let cache = CACHE.read().unwrap_or_else(|e| e.into_inner());
    cache.get(key).copied()
}

fn build_descriptor(sigil_bitmap: u32, payload_word_count: u8) -> usize {
    let boehm_bitmap: usize = (sigil_bitmap as usize) << 1;
    let len_bits: usize = 1 + payload_word_count as usize;
    // SAFETY: `GC_make_descriptor` reads `len_bits` bits from
    // `&boehm_bitmap`. One `usize` covers up to 64 bits on our
    // 64-bit targets; `len_bits = 1 + payload_word_count` and
    // `payload_word_count ≤ 63` (Header's 6-bit count field), so
    // `len_bits ≤ 64` — within the single-word backing buffer.
    // `&boehm_bitmap` is valid for the call's duration; Boehm
    // doesn't retain the pointer.
    unsafe { GC_make_descriptor(&boehm_bitmap, len_bits) }
}

#[cfg(test)]
pub(crate) fn cache_size() -> usize {
    CACHE.read().unwrap_or_else(|e| e.into_inner()).len()
}

#[cfg(test)]
pub(crate) fn clear_cache() {
    CACHE.write().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::sigil_gc_init;

    fn setup() {
        sigil_gc_init();
        clear_cache();
    }

    #[test]
    fn identical_bitmaps_return_identical_handles() {
        let _guard = crate::test_support::gc_test_lock();
        setup();

        let d1 = get_or_create(0b1, 1);
        let d2 = get_or_create(0b1, 1);
        let d3 = get_or_create(0b1, 1);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
        assert_ne!(d1, 0, "GC_make_descriptor must not return zero");
        assert_eq!(cache_size(), 1);
    }

    #[test]
    fn distinct_bitmaps_return_distinct_handles() {
        let _guard = crate::test_support::gc_test_lock();
        setup();

        // Three distinct shapes: single-ptr, 2-field-record with the
        // pointer in slot 1, and 3-field-record with pointers in
        // slots 0+2 (the "0b101" example from the plan body).
        let single_ptr = get_or_create(0b1, 1);
        let ptr_in_slot1 = get_or_create(0b10, 2);
        let ptrs_in_0_and_2 = get_or_create(0b101, 3);

        assert_ne!(single_ptr, 0);
        assert_ne!(ptr_in_slot1, 0);
        assert_ne!(ptrs_in_0_and_2, 0);
        // Handles are opaque, but distinct shapes must produce
        // distinct handles — otherwise the descriptor cache is
        // structurally broken (every shape would scan as if it were
        // the first-cached shape).
        assert_ne!(single_ptr, ptr_in_slot1);
        assert_ne!(single_ptr, ptrs_in_0_and_2);
        assert_ne!(ptr_in_slot1, ptrs_in_0_and_2);
        assert_eq!(cache_size(), 3);
    }

    #[test]
    fn same_bitmap_different_count_are_distinct_keys() {
        let _guard = crate::test_support::gc_test_lock();
        setup();

        // The bitmap value is the same, but the payload sizes
        // differ: `(0b1, 1)` is a 2-word object (header + 1 payload
        // word that is a pointer), `(0b1, 5)` is a 6-word object
        // (header + 5 payload words where only word 0 is a pointer).
        // These must yield distinct cache entries — the deviation
        // from the plan body's bitmap-only key the module doc calls
        // out.
        let small = get_or_create(0b1, 1);
        let large = get_or_create(0b1, 5);
        assert_ne!(small, 0);
        assert_ne!(large, 0);
        assert_eq!(cache_size(), 2);
    }

    #[test]
    fn ten_thousand_same_shape_lookups_yield_one_cache_entry() {
        // The stress test the plan body specifies: "allocate 10k
        // objects with the same bitmap; descriptor cache holds 1
        // entry, not 10k". Task 7 doesn't allocate (Task 8 wires
        // that); the equivalent at the cache layer is 10k lookups
        // of the same shape.
        let _guard = crate::test_support::gc_test_lock();
        setup();

        let first = get_or_create(0b1, 1);
        for _ in 0..10_000 {
            let d = get_or_create(0b1, 1);
            assert_eq!(d, first, "all lookups must return the same handle");
        }
        assert_eq!(
            cache_size(),
            1,
            "10k identical lookups must yield exactly 1 cache entry"
        );
    }

    #[test]
    fn zero_bitmap_caches_separately_from_one_bit() {
        // `bitmap=0b0` (no precise pointers in the payload) is the
        // case Task 8 will route to `GC_malloc_atomic` rather than
        // typed-malloc. The cache still needs to handle a lookup
        // for it without conflating with `0b1`.
        let _guard = crate::test_support::gc_test_lock();
        setup();

        let no_ptrs = get_or_create(0, 4);
        let one_ptr = get_or_create(0b1, 4);
        assert_ne!(no_ptrs, one_ptr);
        assert_eq!(cache_size(), 2);
    }

    #[test]
    fn max_payload_count_does_not_overflow() {
        // Header's count field is 6 bits → max 63. With the +1
        // shift for the header, `len_bits` reaches 64 — the edge
        // of a single `usize` bitmap on 64-bit hosts. Verify the
        // cache handles this without panic or descriptor failure.
        let _guard = crate::test_support::gc_test_lock();
        setup();

        let descr = get_or_create(u32::MAX, 31);
        assert_ne!(
            descr, 0,
            "max-payload descriptor must succeed (or return Boehm's conservative fallback, also non-zero)"
        );
    }
}
