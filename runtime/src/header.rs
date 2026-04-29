//! 8-byte object header — plan A1 Stage 1 task 2.
//!
//! Every heap object begins with an 8-byte header before its payload. The
//! layout is committed for v1/v2 forward compatibility:
//!
//! | Range  | Width | Field                                           |
//! |--------|-------|-------------------------------------------------|
//! | 0..8   | 8     | type tag (0x00..0xFE index per-type descriptor; 0xFF = external)
//! | 8..14  | 6     | payload word count (0..63)                      |
//! | 14..46 | 32    | GC pointer bitmap (bit k ⇒ payload[k] is a GC pointer) |
//! | 46..64 | 18    | reserved (forwarding pointer / generation / mark) |
//!
//! The bit-layout constants and the `header_word` construction formula
//! live in the shared `sigil_header_constants` crate so both the
//! runtime and the compiler build headers from a single source. This
//! module wraps them with the `Header` struct and accessor methods
//! runtime callers use; every runtime allocation site must still route
//! through `Header::new`. The compiler's codegen consumes the same
//! constants + `header_word` directly because it emits header
//! immediates inline at each allocation site in generated code.

pub use sigil_header_constants::{
    TAG_ARRAY, TAG_CLOSURE, TAG_EXTERNAL_DESCRIPTOR, TAG_INT64, TAG_STRING,
};

use sigil_header_constants::{header_word, BITMAP_MASK, BITMAP_SHIFT, COUNT_MASK, COUNT_SHIFT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header(pub u64);

impl Header {
    /// Build a header word from its three logical fields. `count` must be
    /// ≤63 (objects with more payload words reserve tag `0xFF` and consume
    /// a per-type descriptor — not implemented in v1). `bitmap` bit `k`
    /// must be set iff payload word `k` is a GC-managed pointer; the
    /// parameter itself is the raw 32-bit bitmap.
    #[inline]
    pub fn new(type_tag: u8, count: u8, bitmap: u32) -> Self {
        // `type_tag == 0xFF` is reserved; Stage 1 allocations never use it.
        // The debug_assert lets callers catch accidental reserved-tag usage
        // during development; release builds elide the check.
        debug_assert!(
            type_tag != TAG_EXTERNAL_DESCRIPTOR,
            "Header::new: tag 0xFF is reserved for v2 external descriptors",
        );
        debug_assert!(
            (count as u64) <= COUNT_MASK,
            "Header::new: count {count} exceeds 6-bit field",
        );

        Header(header_word(type_tag, count, bitmap))
    }

    #[inline]
    pub fn raw(self) -> u64 {
        self.0
    }

    #[inline]
    pub fn type_tag(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    #[inline]
    pub fn payload_count(self) -> u8 {
        ((self.0 >> COUNT_SHIFT) & COUNT_MASK) as u8
    }

    #[inline]
    pub fn pointer_bitmap(self) -> u32 {
        ((self.0 & BITMAP_MASK) >> BITMAP_SHIFT) as u32
    }

    /// Reserved bits; always zero in v1. v2 will store forwarding pointer
    /// / generation / mark bits here.
    #[inline]
    pub fn reserved_bits(self) -> u32 {
        (self.0 >> 46) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_fields() {
        let h = Header::new(TAG_STRING, 3, 0b1010);
        assert_eq!(h.type_tag(), TAG_STRING);
        assert_eq!(h.payload_count(), 3);
        assert_eq!(h.pointer_bitmap(), 0b1010);
        assert_eq!(h.reserved_bits(), 0);
    }

    #[test]
    fn header_fields_do_not_overlap() {
        let h1 = Header::new(0xAB, 0, 0).raw();
        let h2 = Header::new(0x00, 0x3F, 0).raw();
        let h3 = Header::new(0x00, 0, u32::MAX).raw();
        // No overlap means OR of the three disjoint patterns equals
        // the OR-encoded header with all fields set.
        let combined = Header::new(0xAB, 0x3F, u32::MAX).raw();
        assert_eq!(combined, h1 | h2 | h3);
        // Reserved range is still zero.
        assert_eq!((combined >> 46), 0);
    }

    #[test]
    fn max_count_fits_in_six_bits() {
        let h = Header::new(TAG_STRING, 63, 0);
        assert_eq!(h.payload_count(), 63);
    }

    #[test]
    fn string_tag_is_not_reserved() {
        assert_ne!(TAG_STRING, TAG_EXTERNAL_DESCRIPTOR);
    }
}
