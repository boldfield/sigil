//! Safepoint metadata — plan A1 task 0.11.
//!
//! The compiler emits one safepoint record per Cranelift `call` /
//! `call_indirect` into an object-file section:
//!
//! - ELF (Linux):   `.sigil_stackmaps`
//! - Mach-O:        `__SIGIL,__stackmaps`
//!
//! Plan A1 ships **version 0 (placeholder)** records. v1 Boehm ignores the
//! section entirely; v2 precise GC parses it and will recognise version 0
//! as placeholder data — the reader is expected to skip the section and
//! resynthesise safepoint metadata from relocations, or bail. See
//! `PLAN_A1_DEVIATIONS.md` (`[DEVIATION Task 0.11]`) for the full
//! rationale and the v0 → v1 upgrade path.
//!
//! Binary format (little-endian, host bytes):
//!
//! ```text
//! header  = magic:4 "SGST" | version:4 | record_count:4          // 12 bytes
//! record  = pc_offset:4    | live_count:2 | flags:2              //  8 bytes
//! ```
//!
//! In v0: `live_count` is always 0, `pc_offset` is a placeholder (the
//! Cranelift `Inst` handle, not a post-regalloc code offset), and
//! `flags` has bit 0 (`STACKMAP_FLAG_PLACEHOLDER`) set on every record.

/// ELF section name used on `x86_64-unknown-linux-gnu`.
pub const ELF_SECTION_NAME: &str = ".sigil_stackmaps";

/// Mach-O segment + section pair used on `aarch64-apple-darwin`.
pub const MACHO_SEGMENT_NAME: &str = "__SIGIL";
pub const MACHO_SECTION_NAME: &str = "__stackmaps";

/// Section-header magic bytes. Identifies a Sigil stackmap section to a
/// precise-GC reader regardless of the enclosing object format.
pub const STACKMAP_MAGIC: &[u8; 4] = b"SGST";

/// Section version. Plan A1 ships version 0 (placeholder); Plan B ships
/// version 1 with real post-regalloc PC offsets and live-value lists.
pub const STACKMAP_VERSION_PLACEHOLDER: u32 = 0;

/// Header width in bytes: 4 magic + 4 version + 4 record_count.
pub const STACKMAP_HEADER_SIZE: usize = 12;

/// Record width in bytes: 4 pc_offset + 2 live_count + 2 flags.
pub const STACKMAP_RECORD_SIZE: usize = 8;

/// Flag bit: record is a placeholder (Plan A1 v0 invariant — every record
/// has this bit set).
pub const STACKMAP_FLAG_PLACEHOLDER: u16 = 0x0001;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Section body smaller than the 12-byte header.
    TruncatedHeader,
    /// First four bytes do not match `STACKMAP_MAGIC`.
    BadMagic,
    /// Version field names a format this build does not understand. Plan
    /// A1 only accepts `STACKMAP_VERSION_PLACEHOLDER`; Plan B will accept
    /// `STACKMAP_VERSION_PLACEHOLDER | STACKMAP_VERSION_V1`.
    UnknownVersion(u32),
    /// Header promised more records than the section carries.
    TruncatedRecords,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedRecord {
    pub pc_offset: u32,
    pub live_count: u16,
    pub flags: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSection {
    pub version: u32,
    pub records: Vec<ParsedRecord>,
}

/// Parse a stackmap section body. Does not read object-file headers —
/// pass in the bytes of the section payload only.
pub fn parse_section(bytes: &[u8]) -> Result<ParsedSection, ParseError> {
    if bytes.len() < STACKMAP_HEADER_SIZE {
        return Err(ParseError::TruncatedHeader);
    }
    if &bytes[0..4] != STACKMAP_MAGIC {
        return Err(ParseError::BadMagic);
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != STACKMAP_VERSION_PLACEHOLDER {
        return Err(ParseError::UnknownVersion(version));
    }
    let count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let expected = STACKMAP_HEADER_SIZE.saturating_add(count.saturating_mul(STACKMAP_RECORD_SIZE));
    if bytes.len() < expected {
        return Err(ParseError::TruncatedRecords);
    }
    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let base = STACKMAP_HEADER_SIZE + i * STACKMAP_RECORD_SIZE;
        records.push(ParsedRecord {
            pc_offset: u32::from_le_bytes([
                bytes[base],
                bytes[base + 1],
                bytes[base + 2],
                bytes[base + 3],
            ]),
            live_count: u16::from_le_bytes([bytes[base + 4], bytes[base + 5]]),
            flags: u16::from_le_bytes([bytes[base + 6], bytes[base + 7]]),
        });
    }
    Ok(ParsedSection { version, records })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    fn build_section(records: &[(u32, u16, u16)]) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(STACKMAP_HEADER_SIZE + records.len() * STACKMAP_RECORD_SIZE);
        out.extend_from_slice(STACKMAP_MAGIC);
        out.extend_from_slice(&STACKMAP_VERSION_PLACEHOLDER.to_le_bytes());
        out.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for (pc, live, flags) in records {
            out.extend_from_slice(&pc.to_le_bytes());
            out.extend_from_slice(&live.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
        }
        out
    }

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
        assert_eq!(STACKMAP_HEADER_SIZE, 12);
        assert_eq!(STACKMAP_RECORD_SIZE, 8);
        assert_eq!(STACKMAP_FLAG_PLACEHOLDER, 0x0001);
    }

    #[test]
    fn parse_empty_section_ok() {
        let bytes = build_section(&[]);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.version, STACKMAP_VERSION_PLACEHOLDER);
        assert!(s.records.is_empty());
    }

    #[test]
    fn parse_with_records() {
        let input = [
            (0x1111_2222, 0u16, STACKMAP_FLAG_PLACEHOLDER),
            (0x3333_4444, 0u16, STACKMAP_FLAG_PLACEHOLDER),
        ];
        let bytes = build_section(&input);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.records.len(), 2);
        assert_eq!(s.records[0].pc_offset, 0x1111_2222);
        assert_eq!(s.records[0].flags, STACKMAP_FLAG_PLACEHOLDER);
        assert_eq!(s.records[1].pc_offset, 0x3333_4444);
        // v0 invariant: every record flagged placeholder, live_count=0.
        for r in &s.records {
            assert_eq!(r.live_count, 0);
            assert_eq!(
                r.flags & STACKMAP_FLAG_PLACEHOLDER,
                STACKMAP_FLAG_PLACEHOLDER
            );
        }
    }

    #[test]
    fn short_header_rejected() {
        let bytes = [b'S', b'G'];
        assert_eq!(parse_section(&bytes), Err(ParseError::TruncatedHeader));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = build_section(&[]);
        bytes[0] = b'X';
        assert_eq!(parse_section(&bytes), Err(ParseError::BadMagic));
    }

    #[test]
    fn unknown_version_rejected() {
        let mut bytes = build_section(&[]);
        // Overwrite version field with 1 (reserved for Plan B v1).
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(parse_section(&bytes), Err(ParseError::UnknownVersion(1)));
    }

    #[test]
    fn truncated_records_rejected() {
        // Claim 2 records but ship only 1's worth of bytes after the header.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(STACKMAP_MAGIC);
        bytes.extend_from_slice(&STACKMAP_VERSION_PLACEHOLDER.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; STACKMAP_RECORD_SIZE]);
        assert_eq!(parse_section(&bytes), Err(ParseError::TruncatedRecords));
    }
}
