//! Shared ABI constants for the 8-byte Sigil object header — extracted
//! from `sigil-runtime::header` in PR #7 (plan A2 task 32 follow-up).
//!
//! The compiler crate and the runtime crate both need to construct
//! header words: the compiler emits header immediates inline at
//! allocation sites in generated code, and the runtime builds headers
//! in its own `String`/closure/etc. helpers. Until PR #7, both sides
//! maintained their own copy of the bit-layout formula, which made the
//! "SINGLE source of truth" comment on the runtime copy a lie whenever
//! codegen needed a new tag. This crate is the single source.
//!
//! `#![no_std]` so downstream crates can depend on it without pulling
//! `std`; the body is only tag values, bit constants, and one
//! `const fn`.

#![no_std]

/// Reserved type-tag sentinel for "see external descriptor table" (v2 only).
pub const TAG_EXTERNAL_DESCRIPTOR: u8 = 0xFF;

// v1 type tags. Add new tags at the next free slot; never renumber.
pub const TAG_STRING: u8 = 0x01;
pub const TAG_INT64: u8 = 0x02;
/// Closure record layout: `{header, code_ptr, env[0], ..., env[N-1]}`.
/// Payload word 0 is the code pointer (a static fn address, not a GC
/// pointer) so bit 0 of the header's pointer bitmap is always 0 for
/// closures. Subsequent words are env slots; bit `k+1` is set iff
/// payload word `k+1` (env slot `k`) holds a GC-managed pointer.
pub const TAG_CLOSURE: u8 = 0x03;

/// Payload-word-count field layout.
pub const COUNT_BITS: u32 = 6;
pub const COUNT_SHIFT: u32 = 8;
pub const COUNT_MASK: u64 = (1u64 << COUNT_BITS) - 1;

/// Pointer-bitmap field layout.
pub const BITMAP_BITS: u32 = 32;
pub const BITMAP_SHIFT: u32 = 14;
pub const BITMAP_MASK: u64 = ((1u64 << BITMAP_BITS) - 1) << BITMAP_SHIFT;

/// Construct a header word from its three logical fields.
///
/// | Range  | Width | Field                                           |
/// |--------|-------|-------------------------------------------------|
/// | 0..8   | 8     | `tag` (0x00..0xFE per-type; 0xFF = external)    |
/// | 8..14  | 6     | `count` — payload word count (0..63)            |
/// | 14..46 | 32    | `bitmap` — bit k ⇒ payload[k] is a GC pointer   |
/// | 46..64 | 18    | reserved (forwarding pointer / generation / mark) |
///
/// `count` larger than 63 silently truncates via the mask; v1
/// allocations never exceed this since Sigil objects with more than
/// 63 payload words reserve tag `0xFF` and consume a per-type
/// descriptor (v2 only). Callers assert the precondition themselves
/// when they care (see `runtime::header::Header::new`'s
/// `debug_assert!`). This function is `const` so Cranelift can consume
/// the result as an immediate.
#[inline]
pub const fn header_word(tag: u8, count: u8, bitmap: u32) -> u64 {
    (tag as u64)
        | (((count as u64) & COUNT_MASK) << COUNT_SHIFT)
        | ((bitmap as u64) << BITMAP_SHIFT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_word_composes_from_disjoint_fields() {
        let h = header_word(0xAB, 0x3F, u32::MAX);
        let parts =
            header_word(0xAB, 0, 0) | header_word(0x00, 0x3F, 0) | header_word(0x00, 0, u32::MAX);
        assert_eq!(h, parts);
        assert_eq!(h >> 46, 0, "reserved bits must stay zero");
    }

    #[test]
    fn count_mask_truncates_over_63() {
        // count=64 (0b100_0000) overflows the 6-bit field.
        // header_word masks, so only bit 6 is kept, which is 0.
        // Upshot: counts ≥ 64 collapse to the low 6 bits. Callers must
        // check the precondition; this test just pins the behaviour.
        let h64 = header_word(0x01, 64, 0);
        let h0 = header_word(0x01, 0, 0);
        assert_eq!(h64, h0);
    }

    #[test]
    fn tag_constants_are_stable() {
        assert_eq!(TAG_STRING, 0x01);
        assert_eq!(TAG_INT64, 0x02);
        assert_eq!(TAG_CLOSURE, 0x03);
        assert_eq!(TAG_EXTERNAL_DESCRIPTOR, 0xFF);
    }
}
