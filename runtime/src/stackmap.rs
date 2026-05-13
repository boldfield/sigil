//! Safepoint metadata — Plan E2 Phase 1 Task 4 (v1 reader).
//!
//! Wire-format constants live in `sigil-abi::stackmap`. This module
//! re-exports them so existing `sigil_runtime::stackmap::*` callers
//! keep working, and adds the v1 parser that turns section bytes into
//! a Rust-side `ParsedSection`.
//!
//! The compiler emits **v1 only** from Plan E2 Phase 1 Task 4 forward;
//! the v0 placeholder format is retired and a v0 section is rejected
//! as a stale build artifact (recompile against this runtime).
//!
//! Phase 1 ships the writer + reader + parser shape; the runtime
//! lookup-by-PC + GC walker integration land with **Task 5**
//! (`SIGIL_GC_CROSS_CHECK=1` harness in `runtime/src/gc.rs`). Boehm
//! conservative scanning remains authoritative through Phase 1.

pub use sigil_abi::stackmap::{
    ELF_SECTION_NAME, MACHO_SECTION_NAME, MACHO_SEGMENT_NAME, STACKMAP_ENTRY_KIND_HEAP_POINTER,
    STACKMAP_ENTRY_SIZE_V1, STACKMAP_FN_HEADER_SIZE, STACKMAP_HEADER_SIZE, STACKMAP_MAGIC,
    STACKMAP_RECORD_HEADER_SIZE_V1, STACKMAP_VERSION_PLACEHOLDER, STACKMAP_VERSION_V1,
};

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Section body smaller than the 12-byte section header.
    TruncatedHeader,
    /// First four bytes do not match `STACKMAP_MAGIC`.
    BadMagic,
    /// Version field names a format this build does not understand.
    /// Plan E2 Phase 1 Task 4 only accepts `STACKMAP_VERSION_V1`; v0
    /// (`STACKMAP_VERSION_PLACEHOLDER`) returns `UnknownVersion(0)` to
    /// surface stale build artifacts.
    UnknownVersion(u32),
    /// A function block's header was truncated mid-read.
    TruncatedFunctionHeader,
    /// A function block's name field was truncated mid-read.
    TruncatedFunctionName,
    /// Function name claimed by `name_len` is not valid UTF-8.
    NonUtf8FunctionName,
    /// A record header was truncated mid-read.
    TruncatedRecordHeader,
    /// A record's entry list was truncated mid-read.
    TruncatedRecordEntries,
    /// An entry's `kind` byte is not in the v1 known-kinds set
    /// (`STACKMAP_ENTRY_KIND_HEAP_POINTER`).
    UnknownEntryKind(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedEntry {
    pub kind: u8,
    pub sp_offset: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRecord {
    pub pc_offset: u32,
    pub frame_size: u32,
    pub flags: u16,
    pub entries: Vec<ParsedEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFunction {
    pub symbol_name: String,
    pub text_offset: u32,
    pub records: Vec<ParsedRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSection {
    pub version: u32,
    pub functions: Vec<ParsedFunction>,
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
    if version != STACKMAP_VERSION_V1 {
        return Err(ParseError::UnknownVersion(version));
    }
    let fn_count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let mut cur = STACKMAP_HEADER_SIZE;
    let mut functions = Vec::with_capacity(fn_count);

    for _ in 0..fn_count {
        if cur + STACKMAP_FN_HEADER_SIZE > bytes.len() {
            return Err(ParseError::TruncatedFunctionHeader);
        }
        let name_len =
            u32::from_le_bytes([bytes[cur], bytes[cur + 1], bytes[cur + 2], bytes[cur + 3]])
                as usize;
        let record_count = u32::from_le_bytes([
            bytes[cur + 4],
            bytes[cur + 5],
            bytes[cur + 6],
            bytes[cur + 7],
        ]) as usize;
        let text_offset = u32::from_le_bytes([
            bytes[cur + 8],
            bytes[cur + 9],
            bytes[cur + 10],
            bytes[cur + 11],
        ]);
        cur += STACKMAP_FN_HEADER_SIZE;

        if cur + name_len > bytes.len() {
            return Err(ParseError::TruncatedFunctionName);
        }
        let symbol_name = std::str::from_utf8(&bytes[cur..cur + name_len])
            .map_err(|_| ParseError::NonUtf8FunctionName)?
            .to_string();
        cur += name_len;

        let mut records = Vec::with_capacity(record_count);
        for _ in 0..record_count {
            if cur + STACKMAP_RECORD_HEADER_SIZE_V1 > bytes.len() {
                return Err(ParseError::TruncatedRecordHeader);
            }
            let pc_offset =
                u32::from_le_bytes([bytes[cur], bytes[cur + 1], bytes[cur + 2], bytes[cur + 3]]);
            let frame_size = u32::from_le_bytes([
                bytes[cur + 4],
                bytes[cur + 5],
                bytes[cur + 6],
                bytes[cur + 7],
            ]);
            let entry_count = u16::from_le_bytes([bytes[cur + 8], bytes[cur + 9]]) as usize;
            let flags = u16::from_le_bytes([bytes[cur + 10], bytes[cur + 11]]);
            cur += STACKMAP_RECORD_HEADER_SIZE_V1;

            if cur + entry_count * STACKMAP_ENTRY_SIZE_V1 > bytes.len() {
                return Err(ParseError::TruncatedRecordEntries);
            }
            let mut entries = Vec::with_capacity(entry_count);
            for _ in 0..entry_count {
                let kind = bytes[cur];
                let sp_offset = u32::from_le_bytes([
                    bytes[cur + 1],
                    bytes[cur + 2],
                    bytes[cur + 3],
                    bytes[cur + 4],
                ]);
                cur += STACKMAP_ENTRY_SIZE_V1;
                if kind != STACKMAP_ENTRY_KIND_HEAP_POINTER {
                    return Err(ParseError::UnknownEntryKind(kind));
                }
                entries.push(ParsedEntry { kind, sp_offset });
            }
            records.push(ParsedRecord {
                pc_offset,
                frame_size,
                flags,
                entries,
            });
        }

        functions.push(ParsedFunction {
            symbol_name,
            text_offset,
            records,
        });
    }

    Ok(ParsedSection { version, functions })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    type TestEntry = (u8, u32);
    type TestRecord = (u32, u32, u16, Vec<TestEntry>);
    type TestFunction = (String, u32, Vec<TestRecord>);

    fn build_v1_section(fns: &[TestFunction]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(STACKMAP_MAGIC);
        out.extend_from_slice(&STACKMAP_VERSION_V1.to_le_bytes());
        out.extend_from_slice(&(fns.len() as u32).to_le_bytes());
        for (name, text_offset, records) in fns {
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(&(records.len() as u32).to_le_bytes());
            out.extend_from_slice(&text_offset.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            for (pc, fsz, flags, entries) in records {
                out.extend_from_slice(&pc.to_le_bytes());
                out.extend_from_slice(&fsz.to_le_bytes());
                out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
                for (k, sp) in entries {
                    out.push(*k);
                    out.extend_from_slice(&sp.to_le_bytes());
                }
            }
        }
        out
    }

    #[test]
    fn re_exported_constants_match_abi_crate() {
        assert_eq!(STACKMAP_MAGIC, sigil_abi::stackmap::STACKMAP_MAGIC);
        assert_eq!(
            STACKMAP_VERSION_V1,
            sigil_abi::stackmap::STACKMAP_VERSION_V1
        );
        assert_eq!(
            STACKMAP_HEADER_SIZE,
            sigil_abi::stackmap::STACKMAP_HEADER_SIZE
        );
        assert_eq!(
            STACKMAP_FN_HEADER_SIZE,
            sigil_abi::stackmap::STACKMAP_FN_HEADER_SIZE
        );
        assert_eq!(
            STACKMAP_RECORD_HEADER_SIZE_V1,
            sigil_abi::stackmap::STACKMAP_RECORD_HEADER_SIZE_V1
        );
        assert_eq!(
            STACKMAP_ENTRY_SIZE_V1,
            sigil_abi::stackmap::STACKMAP_ENTRY_SIZE_V1
        );
        assert_eq!(
            STACKMAP_ENTRY_KIND_HEAP_POINTER,
            sigil_abi::stackmap::STACKMAP_ENTRY_KIND_HEAP_POINTER
        );
    }

    #[test]
    fn parse_empty_section_ok() {
        let bytes = build_v1_section(&[]);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.version, STACKMAP_VERSION_V1);
        assert!(s.functions.is_empty());
    }

    #[test]
    fn parse_single_function_with_one_record_one_entry() {
        let input = vec![(
            "sigil_user_main".to_string(),
            0u32,
            vec![(
                0x10,
                32u32,
                0u16,
                vec![(STACKMAP_ENTRY_KIND_HEAP_POINTER, 0x18)],
            )],
        )];
        let bytes = build_v1_section(&input);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.functions.len(), 1);
        let f = &s.functions[0];
        assert_eq!(f.symbol_name, "sigil_user_main");
        assert_eq!(f.text_offset, 0);
        assert_eq!(f.records.len(), 1);
        let r = &f.records[0];
        assert_eq!(r.pc_offset, 0x10);
        assert_eq!(r.frame_size, 32);
        assert_eq!(r.flags, 0);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].kind, STACKMAP_ENTRY_KIND_HEAP_POINTER);
        assert_eq!(r.entries[0].sp_offset, 0x18);
    }

    #[test]
    fn parse_function_with_nonzero_text_offset_round_trips() {
        // v1 writers commit to text_offset = 0 today (the runtime
        // resolves bases via dlsym), but the field is part of the wire
        // format and a future writer might populate it. Pin
        // round-trip preservation so a regression — e.g. shifting the
        // 4-byte field by an alignment fix — surfaces here rather
        // than silently producing zero on the read side.
        let input = vec![(
            "sigil_user_offset_test".to_string(),
            0xDEAD_BEEFu32,
            vec![(0x40, 16u32, 0u16, Vec::new())],
        )];
        let bytes = build_v1_section(&input);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.functions.len(), 1);
        assert_eq!(s.functions[0].text_offset, 0xDEAD_BEEF);
        assert_eq!(s.functions[0].symbol_name, "sigil_user_offset_test");
    }

    #[test]
    fn parse_two_functions_round_trip_each_records_set() {
        let input = vec![
            (
                "sigil_user_alpha".to_string(),
                0u32,
                vec![
                    (
                        0x10,
                        16u32,
                        0u16,
                        vec![(STACKMAP_ENTRY_KIND_HEAP_POINTER, 0x08)],
                    ),
                    (0x20, 16u32, 0u16, Vec::new()),
                ],
            ),
            (
                "sigil_user_beta".to_string(),
                0u32,
                vec![(
                    0x30,
                    24u32,
                    0u16,
                    vec![
                        (STACKMAP_ENTRY_KIND_HEAP_POINTER, 0x10),
                        (STACKMAP_ENTRY_KIND_HEAP_POINTER, 0x18),
                    ],
                )],
            ),
        ];
        let bytes = build_v1_section(&input);
        let s = parse_section(&bytes).expect("parse");
        assert_eq!(s.functions.len(), 2);
        assert_eq!(s.functions[0].symbol_name, "sigil_user_alpha");
        assert_eq!(s.functions[0].records.len(), 2);
        assert_eq!(s.functions[0].records[0].entries.len(), 1);
        assert!(s.functions[0].records[1].entries.is_empty());
        assert_eq!(s.functions[1].symbol_name, "sigil_user_beta");
        assert_eq!(s.functions[1].records[0].entries.len(), 2);
    }

    #[test]
    fn short_header_rejected() {
        let bytes = [b'S', b'G'];
        assert_eq!(parse_section(&bytes), Err(ParseError::TruncatedHeader));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = build_v1_section(&[]);
        bytes[0] = b'X';
        assert_eq!(parse_section(&bytes), Err(ParseError::BadMagic));
    }

    #[test]
    fn v0_version_rejected_as_unknown() {
        let mut bytes = build_v1_section(&[]);
        bytes[4..8].copy_from_slice(&STACKMAP_VERSION_PLACEHOLDER.to_le_bytes());
        assert_eq!(parse_section(&bytes), Err(ParseError::UnknownVersion(0)));
    }

    #[test]
    fn truncated_function_header_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(STACKMAP_MAGIC);
        bytes.extend_from_slice(&STACKMAP_VERSION_V1.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // claim 1 function
                                                      // ...but ship only 4 bytes of fn_header (need 12).
        bytes.extend_from_slice(&[0u8; 4]);
        assert_eq!(
            parse_section(&bytes),
            Err(ParseError::TruncatedFunctionHeader)
        );
    }

    #[test]
    fn truncated_function_name_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(STACKMAP_MAGIC);
        bytes.extend_from_slice(&STACKMAP_VERSION_V1.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        // fn_header: name_len=8 but only 4 name bytes follow.
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"abcd");
        assert_eq!(
            parse_section(&bytes),
            Err(ParseError::TruncatedFunctionName)
        );
    }

    #[test]
    fn truncated_record_header_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(STACKMAP_MAGIC);
        bytes.extend_from_slice(&STACKMAP_VERSION_V1.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let name = b"f";
        bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 record promised
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(name);
        // ...but record_header is 12 bytes; ship only 4.
        bytes.extend_from_slice(&[0u8; 4]);
        assert_eq!(
            parse_section(&bytes),
            Err(ParseError::TruncatedRecordHeader)
        );
    }

    #[test]
    fn truncated_record_entries_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(STACKMAP_MAGIC);
        bytes.extend_from_slice(&STACKMAP_VERSION_V1.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let name = b"f";
        bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(name);
        // record_header: pc=0, frame_size=0, entry_count=2, flags=0
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        // promised 2 entries (10 bytes) but ship only 5.
        bytes.push(STACKMAP_ENTRY_KIND_HEAP_POINTER);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            parse_section(&bytes),
            Err(ParseError::TruncatedRecordEntries)
        );
    }

    #[test]
    fn unknown_entry_kind_rejected() {
        let input = vec![(
            "f".to_string(),
            0u32,
            vec![(0u32, 16u32, 0u16, vec![(0xAB, 0x10)])],
        )];
        let bytes = build_v1_section(&input);
        assert_eq!(
            parse_section(&bytes),
            Err(ParseError::UnknownEntryKind(0xAB))
        );
    }
}
