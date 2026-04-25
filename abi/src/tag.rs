//! Tagged-Value bit layout — single source of truth.
//!
//! A Sigil `Value` is a single `u64`, tagged by its low bits:
//!
//! - `...xxx0` — 63-bit signed integer (`Int`), range `[-2^62, 2^62)`.
//!   Overflow wraps two's-complement. Encoding: `(signed as u64) << 1`.
//! - `...xx01` — heap pointer. The pointer is 8-byte-aligned (Boehm
//!   guarantees that), so the low three bits of the raw pointer are
//!   zero; bits `0..2 = 01` tag heap values.
//! - `...xx11` — immediate (Bool, Unit, small Char).
//!
//! Plan B's CPS transform and effect runtime add code paths that emit
//! tag operations directly in generated code (continuation values,
//! `NextStep` records, `sigil_perform` argument marshalling). Centralising
//! the masks here means the compiler emitter and runtime helpers
//! reference one canonical bit layout instead of inlining `0b1` /
//! `ishl_imm 1` literals across both crates.
//!
//! Layout helpers (`from_int`, `as_int`, `from_heap`, `as_heap_ptr`)
//! continue to live in `sigil-runtime::value`. This module is pure
//! `const`-data so it can be `#![no_std]` and dependency-free.

/// Bitmask isolating the low tag bit (Int vs non-Int).
pub const TAG_INT_MASK: u64 = 0b1;

/// Bitmask isolating the low two tag bits (heap vs immediate when non-Int).
pub const TAG_WIDE_MASK: u64 = 0b11;

/// Tag bits for a heap pointer. `(ptr & !TAG_WIDE_MASK)` recovers the
/// raw pointer; `raw_ptr | TAG_HEAP` tags it.
pub const TAG_HEAP: u64 = 0b01;

/// Tag bits for immediates (Bool, Unit, small Char in v1).
pub const TAG_IMMEDIATE: u64 = 0b11;

/// Bit-shift used to encode/decode an `Int`. `(signed as u64) <<
/// TAG_INT_SHIFT` produces the tagged form; `as i64 >> TAG_INT_SHIFT`
/// recovers the value.
///
/// Codegen historically inlined this as the literal `1` in
/// `ishl_imm 1` / `sshr_imm 1` on `main`'s return-path tagging. Plan B
/// references this constant instead, both for documentation and so a
/// future tagged-vs-raw ABI decision (logged in `QUESTIONS.md` under
/// `[PLAN-B]`) has a single mechanical site to audit.
pub const TAG_INT_SHIFT: u32 = 1;

/// Lower / upper inclusive range of Sigil's 63-bit `Int`. Derived
/// from `TAG_INT_SHIFT` so a future shift change updates both bounds
/// in a single place. `63 - TAG_INT_SHIFT` is the number of payload
/// bits available for the signed value (with one sign bit on top of
/// that, hence the `-` for `INT_MIN`).
pub const INT_MIN: i64 = -(1i64 << (63 - TAG_INT_SHIFT));
pub const INT_MAX: i64 = (1i64 << (63 - TAG_INT_SHIFT)) - 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_are_consistent() {
        // TAG_HEAP and TAG_IMMEDIATE share the low bit being 1 (non-Int)
        // and disagree on bit 1.
        assert_eq!(TAG_HEAP & TAG_INT_MASK, TAG_INT_MASK);
        assert_eq!(TAG_IMMEDIATE & TAG_INT_MASK, TAG_INT_MASK);
        assert_ne!(TAG_HEAP, TAG_IMMEDIATE);
        // TAG_WIDE_MASK covers the two tag bits.
        assert_eq!(TAG_WIDE_MASK, TAG_HEAP | TAG_IMMEDIATE);
    }

    #[test]
    fn int_range_matches_shift() {
        // The 63-bit signed range is exactly [-(1<<62), (1<<62)-1] when
        // TAG_INT_SHIFT = 1 leaves 63 bits for the payload.
        assert_eq!(INT_MIN, -(1i64 << (64 - TAG_INT_SHIFT - 1)));
        assert_eq!(INT_MAX, (1i64 << (64 - TAG_INT_SHIFT - 1)) - 1);
    }
}
