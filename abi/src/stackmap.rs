//! Stackmap section wire format — single source of truth.
//!
//! The compiler emits one safepoint record per Cranelift `call` /
//! `call_indirect` into an object-file section:
//!
//! - ELF (Linux):   `.sigil_stackmaps`
//! - Mach-O:        `__SIGIL,__stackmaps`
//!
//! Plan A1 ships **version 0 (placeholder)** records: `live_count = 0`,
//! `pc_offset` is a Cranelift `Inst` handle (not a real post-regalloc
//! offset), and the placeholder flag bit is set on every record so a
//! v2 reader can detect stale placeholder data per-record as well as
//! via the version field. Plan B replaces this with real safepoint
//! data (version = 1, live-value list per record, real pc_offset).
//!
//! Binary format (little-endian on the host; the section is not
//! relocated, so emitter endianness == consumer endianness):
//!
//! ```text
//! header  = magic:4 "SGST" | version:4 | record_count:4    // 12 bytes
//! v0 rec  = pc_offset:4    | live_count:2 | flags:2        //  8 bytes
//! ```
//!
//! Plan B's v1 record format reuses the same header. The record gains
//! a live-value list and `pc_offset` becomes a real post-regalloc code
//! offset via Cranelift's safepoint API. The v1 record-layout struct
//! is added when the v1 work lands; this module reserves the version
//! number and flag bit ahead of time so there is no drift window.

/// ELF section name used on `x86_64-unknown-linux-gnu`.
pub const ELF_SECTION_NAME: &str = ".sigil_stackmaps";

/// Mach-O segment + section pair used on `aarch64-apple-darwin`.
pub const MACHO_SEGMENT_NAME: &str = "__SIGIL";
pub const MACHO_SECTION_NAME: &str = "__stackmaps";

/// Section magic. Identifies a Sigil stackmap section to a precise-GC
/// reader regardless of the enclosing object format.
pub const STACKMAP_MAGIC: &[u8; 4] = b"SGST";

/// Version 0: Plan A1 placeholder format.
pub const STACKMAP_VERSION_PLACEHOLDER: u32 = 0;

/// Version 1: Plan B real-safepoint format. Reserved here so
/// version-aware readers can be written before v1 ships.
pub const STACKMAP_VERSION_V1: u32 = 1;

/// Header width in bytes: 4 magic + 4 version + 4 record_count.
pub const STACKMAP_HEADER_SIZE: usize = 12;

/// V0 record width in bytes: 4 pc_offset + 2 live_count + 2 flags.
pub const STACKMAP_RECORD_SIZE: usize = 8;

/// Flag bit: record is a placeholder (Plan A1 v0 invariant — every
/// record has this bit set; v1 records clear it).
pub const STACKMAP_FLAG_PLACEHOLDER: u16 = 0x0001;

/// V0 record layout. Mirrors the on-disk shape: 4 + 2 + 2 = 8 bytes.
///
/// In v0, `live_count` is always 0 and `flags` always carries the
/// placeholder bit. The struct is defined here so consumers (compiler
/// emitter, runtime parser, future precise-GC reader) share one
/// description; serialisation / deserialisation logic stays in the
/// consumer crate that performs IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackMapRecordV0 {
    pub pc_offset: u32,
    pub live_count: u16,
    pub flags: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_names_are_fixed() {
        assert_eq!(ELF_SECTION_NAME, ".sigil_stackmaps");
        assert_eq!(MACHO_SEGMENT_NAME, "__SIGIL");
        assert_eq!(MACHO_SECTION_NAME, "__stackmaps");
    }

    #[test]
    fn format_constants_are_fixed() {
        assert_eq!(STACKMAP_MAGIC, b"SGST");
        assert_eq!(STACKMAP_VERSION_PLACEHOLDER, 0);
        assert_eq!(STACKMAP_VERSION_V1, 1);
        assert_eq!(STACKMAP_HEADER_SIZE, 12);
        assert_eq!(STACKMAP_RECORD_SIZE, 8);
        assert_eq!(STACKMAP_FLAG_PLACEHOLDER, 0x0001);
    }

    #[test]
    fn v0_record_size_matches_constant() {
        // No #[repr] — the struct is a logical model, not an FFI shape.
        // Field-width sum (u32 + u16 + u16 = 8 bytes) must still match
        // the wire-format constant.
        assert_eq!(
            core::mem::size_of::<u32>() + core::mem::size_of::<u16>() * 2,
            STACKMAP_RECORD_SIZE
        );
    }
}
