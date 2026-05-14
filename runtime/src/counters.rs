//! Runtime instrumentation counters — plan A1 task 0.10.
//!
//! The fixed set of atomic counters declared in the design doc's Runtime
//! Instrumentation section. Plan A1 wires only the Boehm counters; the
//! arena, handler-walk, trampoline, and CPS counters are declared here so
//! Plan B can populate them without adding a module.
//!
//! All counters are zero-overhead relaxed-ordering increments. `sigil
//! --print-runtime-stats` reads them via the FFI symbol `sigil_counter_read`
//! after the compiled program exits (or at another read point in v2).
//!
//! The stable C-side identifiers are the ALL_CAPS names below; the symbolic
//! IDs are also used by the C ABI. The enum `CounterId` keeps Rust code
//! from mis-numbering them.

use std::sync::atomic::{AtomicU64, Ordering};

/// Stable integer IDs for each counter. The ordering here is the ABI; do
/// not renumber once shipped.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterId {
    BoehmAllocCount = 0,
    BoehmAllocBytes = 1,
    ArenaAllocCount = 2,
    ArenaAllocBytes = 3,
    ArenaEscapeCount = 4,
    HandlerWalkCount = 5,
    HandlerWalkDepthSum = 6,
    TrampolineDispatchCount = 7,
    CpsCallCount = 8,
    NativeCallCount = 9,
    /// Plan C Task 65 — count of `sigil_array_alloc` invocations.
    ArrayAllocCount = 10,
    /// Plan C Task 65 — total bytes allocated by `sigil_array_alloc`
    /// (header + length-word + element slots).
    ArrayAllocBytes = 11,
    /// Plan C Task 66 — count of `sigil_mut_array_new` invocations.
    MutArrayAllocCount = 12,
    /// Plan C Task 66 — total bytes allocated by `sigil_mut_array_new`.
    MutArrayAllocBytes = 13,
    /// Plan C Task 66.5 — count of `sigil_byte_array_*` allocation
    /// invocations (`alloc`, `empty`, `concat`, `slice`,
    /// `string_to_bytes`, `string_from_bytes` allocations of fresh
    /// arrays).
    ByteArrayAllocCount = 14,
    /// Plan C Task 66.5 — total bytes allocated by `sigil_byte_array_*`
    /// (header + length-word + byte payload).
    ByteArrayAllocBytes = 15,
    /// Plan C Task 66.6 — count of `sigil_mut_byte_array_new`
    /// invocations.
    MutByteArrayAllocCount = 16,
    /// Plan C Task 66.6 — total bytes allocated by
    /// `sigil_mut_byte_array_new`.
    MutByteArrayAllocBytes = 17,
    /// Plan C Task 69 — count of `sigil_int64_*` allocation
    /// invocations (every arithmetic / construction / negation op
    /// allocates one fresh boxed Int64).
    Int64AllocCount = 18,
    /// Plan C Task 69 — total bytes allocated by `sigil_int64_*`
    /// (header + payload word; 16 bytes per record).
    Int64AllocBytes = 19,
    /// Plan C Task 67 — count of `sigil_string_builder_*`
    /// allocation invocations (`sb_new`, `sb_finalize`, plus
    /// segment growth inside `sb_append`).
    StringBuilderAllocCount = 20,
    /// Plan C Task 67 — total bytes allocated by
    /// `sigil_string_builder_*` (record + segments + finalized
    /// String).
    StringBuilderAllocBytes = 21,
    /// Plan D Task 117 (b) Phase 4 — count of
    /// `sigil_continuation_alloc` invocations. One alloc per
    /// site that flows a continuation into a fn-parameter (e.g.,
    /// each recursive call in a runtime-N discharger).
    ContinuationAllocCount = 22,
    /// Plan D Task 117 (b) Phase 4 — total bytes allocated by
    /// `sigil_continuation_alloc` (header + 4 ptr fields =
    /// 40 bytes per record; matches the
    /// `counters::add(_, 40)` call at the alloc site).
    ContinuationAllocBytes = 23,
    FloatAllocCount = 24,
    FloatAllocBytes = 25,
    /// Plan C addendum (Char) — count of `sigil_char_box` /
    /// `sigil_int_to_char_box` / classifier-allocating invocations.
    /// Each call allocates one fresh boxed `Char` record.
    CharAllocCount = 26,
    /// Plan C addendum (Char) — total bytes allocated for boxed
    /// `Char` records (header + 4-byte codepoint + 4-byte padding;
    /// 16 bytes per record, matching the `Float` / `Int64` cost).
    CharAllocBytes = 27,
}

const COUNTER_SLOTS: usize = 28;

/// Static backing storage for all counters. Mutable only via atomic relaxed
/// operations; never touched by reference. This is the only global atomic
/// state the runtime owns.
static COUNTERS: [AtomicU64; COUNTER_SLOTS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Human-readable names paired with `CounterId`. Kept in the same order as
/// the enum so `print_all` can zip them.
pub const NAMES: [&str; COUNTER_SLOTS] = [
    "SIGIL_COUNTER_BOEHM_ALLOC_COUNT",
    "SIGIL_COUNTER_BOEHM_ALLOC_BYTES",
    "SIGIL_COUNTER_ARENA_ALLOC_COUNT",
    "SIGIL_COUNTER_ARENA_ALLOC_BYTES",
    "SIGIL_COUNTER_ARENA_ESCAPE_COUNT",
    "SIGIL_COUNTER_HANDLER_WALK_COUNT",
    "SIGIL_COUNTER_HANDLER_WALK_DEPTH_SUM",
    "SIGIL_COUNTER_TRAMPOLINE_DISPATCH_COUNT",
    "SIGIL_COUNTER_CPS_CALL_COUNT",
    "SIGIL_COUNTER_NATIVE_CALL_COUNT",
    "SIGIL_COUNTER_ARRAY_ALLOC_COUNT",
    "SIGIL_COUNTER_ARRAY_ALLOC_BYTES",
    "SIGIL_COUNTER_MUT_ARRAY_ALLOC_COUNT",
    "SIGIL_COUNTER_MUT_ARRAY_ALLOC_BYTES",
    "SIGIL_COUNTER_BYTE_ARRAY_ALLOC_COUNT",
    "SIGIL_COUNTER_BYTE_ARRAY_ALLOC_BYTES",
    "SIGIL_COUNTER_MUT_BYTE_ARRAY_ALLOC_COUNT",
    "SIGIL_COUNTER_MUT_BYTE_ARRAY_ALLOC_BYTES",
    "SIGIL_COUNTER_INT64_ALLOC_COUNT",
    "SIGIL_COUNTER_INT64_ALLOC_BYTES",
    "SIGIL_COUNTER_STRING_BUILDER_ALLOC_COUNT",
    "SIGIL_COUNTER_STRING_BUILDER_ALLOC_BYTES",
    "SIGIL_COUNTER_CONTINUATION_ALLOC_COUNT",
    "SIGIL_COUNTER_CONTINUATION_ALLOC_BYTES",
    "SIGIL_COUNTER_FLOAT_ALLOC_COUNT",
    "SIGIL_COUNTER_FLOAT_ALLOC_BYTES",
    "SIGIL_COUNTER_CHAR_ALLOC_COUNT",
    "SIGIL_COUNTER_CHAR_ALLOC_BYTES",
];

#[inline]
pub fn add(id: CounterId, delta: u64) {
    let slot = &COUNTERS[id as usize];
    slot.fetch_add(delta, Ordering::Relaxed);
}

#[inline]
pub fn incr(id: CounterId) {
    add(id, 1);
}

#[inline]
pub fn read(id: CounterId) -> u64 {
    COUNTERS[id as usize].load(Ordering::Relaxed)
}

/// FFI — read one counter by stable integer ID. Exposed to the compiled
/// program so `sigil --print-runtime-stats` can surface values at exit.
///
/// Returns `u64::MAX` if the id is out of range; callers should treat
/// that as an error sentinel.
///
/// # Safety
///
/// Safe to call from any thread. Relaxed atomic read.
#[no_mangle]
pub extern "C" fn sigil_counter_read(id: u32) -> u64 {
    if (id as usize) >= COUNTER_SLOTS {
        return u64::MAX;
    }
    COUNTERS[id as usize].load(Ordering::Relaxed)
}

/// FFI — convenience for test drivers to print every counter in a canonical
/// order to stderr. Returns the number of **named counters** printed
/// (`COUNTER_SLOTS`), NOT the total line count.
///
/// Plan E2 Phase 2 closeout (throughput report) appends a single
/// non-counter sidecar line `boehm_gc_time_ms=N` after the named-
/// counter loop so the throughput script can extract Boehm full-GC
/// wall-clock without needing a separate FFI entry point. The
/// sidecar line is well-formed `key=value` so the parser stays
/// grammar-compatible with the rest of the output. The function's
/// return value DOES NOT include the sidecar in its count — it's
/// the count of named counter slots, exposed for FFI callers that
/// want a fixed-shape value rather than a parse-the-stderr API.
/// `boehm_gc_time_ms` reports `0` when the process never triggered
/// a full collection (Boehm's `GC_get_full_gc_total_time` returns
/// 0 in that case).
#[no_mangle]
pub extern "C" fn sigil_counter_print_all() -> u32 {
    let mut eprint = std::io::stderr().lock();
    use std::io::Write;
    for (i, name) in NAMES.iter().enumerate() {
        let v = COUNTERS[i].load(Ordering::Relaxed);
        let _ = writeln!(eprint, "{name}={v}");
    }
    // SAFETY: `GC_get_full_gc_total_time` is a pure accessor over
    // Boehm's process-wide stats; safe to call from any context.
    let gc_ms = unsafe { crate::gc::GC_get_full_gc_total_time() };
    let _ = writeln!(eprint, "boehm_gc_time_ms={gc_ms}");
    COUNTER_SLOTS as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_names_match_enum_order() {
        assert_eq!(NAMES.len(), COUNTER_SLOTS);
        assert_eq!(
            NAMES[CounterId::BoehmAllocCount as usize],
            "SIGIL_COUNTER_BOEHM_ALLOC_COUNT"
        );
        assert_eq!(
            NAMES[CounterId::ArenaEscapeCount as usize],
            "SIGIL_COUNTER_ARENA_ESCAPE_COUNT"
        );
        assert_eq!(
            NAMES[CounterId::NativeCallCount as usize],
            "SIGIL_COUNTER_NATIVE_CALL_COUNT"
        );
    }

    #[test]
    fn ffi_read_out_of_range_returns_sentinel() {
        assert_eq!(sigil_counter_read(999), u64::MAX);
    }
}
