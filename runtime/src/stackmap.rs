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
//! Plan E2 Phase 1 Task 5 adds the runtime reader: a one-time
//! section-locator + parse at startup (via linker-defined
//! `__start_sigil_stackmaps` / `__stop_sigil_stackmaps` on ELF or
//! `getsectiondata("__SIGIL", "__stackmaps", ...)` on Mach-O), an
//! indexed `lookup(pc)` API keyed on `dlsym`'d symbol bases, and the
//! `walk_for_gc` fp-chain walker that collects precise root addresses
//! from the current thread's stack. The `SIGIL_GC_CROSS_CHECK=1`
//! harness in `runtime/src/gc.rs` calls these and cross-checks
//! against Boehm's conservative scan. Boehm conservative scanning
//! remains authoritative through Phase 1.

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

// ===== Plan E2 Phase 1 Task 5 — runtime reader =============================
//
// The compiler's emitted binary carries a `sigil_stackmaps` (ELF) /
// `__SIGIL,__stackmaps` (Mach-O) section with the v1 wire format.
// At startup the runtime locates the section, parses it, and builds
// an indexed view keyed by per-function `dlsym`'d symbol base. The
// indexed view is consulted at every safepoint that the cross-check
// harness visits (see `gc.rs`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Indexed view of the parsed stackmap section. For every function
/// block, the symbol's runtime address (via `dlsym(symbol_name)`) keys
/// a `Vec` of `(pc_offset, &ParsedRecord)` pairs. PC lookups compute
/// `pc - symbol_base` and binary-search the per-fn vector.
pub struct StackmapIndex {
    parsed: ParsedSection,
    /// For each function block: (symbol_base_addr, Vec<(absolute_pc, record_idx)>).
    /// `record_idx` indexes into `parsed.functions[i].records`.
    by_base: BTreeMap<usize, Vec<(usize, usize, usize)>>,
}

impl StackmapIndex {
    /// Resolve a runtime PC to a record by binary-searching the
    /// per-fn absolute-PC list. Returns the matching record on exact
    /// pc match (i.e., PC = fn_base + record.pc_offset), or `None`
    /// when the PC is not at a known safepoint.
    pub fn lookup(&self, pc: usize) -> Option<&ParsedRecord> {
        // Walk every function block. We don't have fn-size data so a
        // base-relative range check would be incorrect; instead we
        // exact-match on absolute PCs. This is O(N) over all
        // safepoints in the program — Phase 1's cross-check fires at
        // sample points, not on hot paths, so the simplicity is worth
        // the cycles. Phase 2's precise marker will need a faster
        // structure (e.g., interval map keyed on fn_base + fn_size).
        for entries in self.by_base.values() {
            for &(abs_pc, fn_idx, rec_idx) in entries {
                if abs_pc == pc {
                    return Some(&self.parsed.functions[fn_idx].records[rec_idx]);
                }
            }
        }
        None
    }

    pub fn parsed(&self) -> &ParsedSection {
        &self.parsed
    }

    /// Number of (function-name, record) pairs the index resolved a
    /// non-zero base address for. Functions whose symbol failed to
    /// resolve via `dlsym` are silently skipped (the symbol may have
    /// been stripped or renamed); cross-check assertions rely on
    /// `lookup(pc)` returning `None` rather than panicking.
    pub fn resolved_record_count(&self) -> usize {
        self.by_base.values().map(|v| v.len()).sum()
    }
}

static INDEX: OnceLock<Option<StackmapIndex>> = OnceLock::new();

/// Initialise the runtime stackmap index. Idempotent. Returns `Some`
/// when the section was located + parsed + at least one function
/// block's symbol resolved via `dlsym`; returns `None` when the
/// section is missing (e.g., the binary was linked without the
/// stackmap section, or the runtime is being exercised via a unit test
/// that doesn't link a real binary).
pub fn init_index() -> Option<&'static StackmapIndex> {
    INDEX
        .get_or_init(|| {
            let bytes = locate_section_bytes()?;
            let parsed = parse_section(bytes).ok()?;
            let by_base = resolve_function_bases(&parsed);
            Some(StackmapIndex { parsed, by_base })
        })
        .as_ref()
}

/// Locate the stackmap section bytes via platform-specific linker
/// symbols / OS APIs. Returns `None` when the section is not present
/// (e.g., unit-test binaries that don't link compiler-emitted code).
///
/// On ELF, the section bounds are resolved via `dlsym` rather than
/// extern statics. The runtime is built as a staticlib and a separate
/// rlib; rlibs are used as link-time dependencies of unit-test
/// binaries that DON'T contain the `sigil_stackmaps` section, so
/// declaring the linker auto-symbols as `extern "C" static` would
/// produce an undefined-reference link error at test time. `dlsym`
/// returns NULL when the symbol is absent, which is the right
/// not-present behaviour without nightly-only weak-linkage attributes.
fn locate_section_bytes() -> Option<&'static [u8]> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: dlsym returns NULL for absent symbols; otherwise
        // returns the section bounds the GNU linker auto-generated.
        // Section is in a non-writable ALLOC area; bytes are valid
        // for the program's lifetime.
        unsafe {
            let start = dlsym_resolve("__start_sigil_stackmaps")? as *const u8;
            let end = dlsym_resolve("__stop_sigil_stackmaps")? as *const u8;
            if end <= start {
                return None;
            }
            let len = end.offset_from(start) as usize;
            Some(std::slice::from_raw_parts(start, len))
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `_dyld_get_image_header(0)` is the main executable's
        // Mach-O header; `getsectiondata` validates the (segment,
        // section) pair and returns null if not present.
        unsafe {
            let mh = _dyld_get_image_header(0);
            if mh.is_null() {
                return None;
            }
            let mut size: u64 = 0;
            // SAFETY: gc-heap-ptr arithmetic (CStr ptr into static rodata).
            let seg_name = MACHO_SEGMENT_NAME_CSTR.as_ptr();
            // SAFETY: gc-heap-ptr arithmetic (CStr ptr into static rodata).
            let sec_name = MACHO_SECTION_NAME_CSTR.as_ptr();
            let ptr = getsectiondata(mh, seg_name, sec_name, &mut size);
            if ptr.is_null() || size == 0 {
                return None;
            }
            Some(std::slice::from_raw_parts(ptr, size as usize))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
const MACHO_SEGMENT_NAME_CSTR: &std::ffi::CStr = c"__SIGIL";
#[cfg(target_os = "macos")]
const MACHO_SECTION_NAME_CSTR: &std::ffi::CStr = c"__stackmaps";

#[cfg(target_os = "macos")]
extern "C" {
    fn _dyld_get_image_header(image_index: u32) -> *const std::ffi::c_void;
    fn getsectiondata(
        mhp: *const std::ffi::c_void,
        segname: *const std::os::raw::c_char,
        sectname: *const std::os::raw::c_char,
        size: *mut u64,
    ) -> *const u8;
}

/// Resolve each function-block's `symbol_name` to a runtime address
/// via `dlsym(RTLD_DEFAULT, ...)`. Returns a per-base-address map of
/// absolute safepoint PCs. Unresolved symbols are skipped silently
/// (see `StackmapIndex::resolved_record_count`).
fn resolve_function_bases(parsed: &ParsedSection) -> BTreeMap<usize, Vec<(usize, usize, usize)>> {
    let mut by_base: BTreeMap<usize, Vec<(usize, usize, usize)>> = BTreeMap::new();
    for (fn_idx, f) in parsed.functions.iter().enumerate() {
        let base = match dlsym_resolve(&f.symbol_name) {
            Some(b) => b,
            None => continue,
        };
        let entries = by_base.entry(base).or_default();
        for (rec_idx, r) in f.records.iter().enumerate() {
            let abs_pc = base.wrapping_add(r.pc_offset as usize);
            entries.push((abs_pc, fn_idx, rec_idx));
        }
    }
    by_base
}

fn dlsym_resolve(symbol: &str) -> Option<usize> {
    use std::ffi::CString;
    let cs = CString::new(symbol).ok()?;
    // SAFETY: dlsym(RTLD_DEFAULT, NUL-terminated cstr) -> ptr or null.
    // RTLD_DEFAULT is platform-specific; we use the standard values.
    unsafe {
        // SAFETY: gc-heap-ptr arithmetic (CString-owned NUL-terminated ptr; dlsym arg only).
        let addr = dlsym(RTLD_DEFAULT, cs.as_ptr());
        if addr.is_null() {
            None
        } else {
            Some(addr as usize)
        }
    }
}

extern "C" {
    fn dlsym(
        handle: *mut std::ffi::c_void,
        symbol: *const std::os::raw::c_char,
    ) -> *mut std::ffi::c_void;
}

#[cfg(target_os = "linux")]
const RTLD_DEFAULT: *mut std::ffi::c_void = std::ptr::null_mut();
#[cfg(target_os = "macos")]
const RTLD_DEFAULT: *mut std::ffi::c_void = -2isize as *mut std::ffi::c_void;

// ===== fp-chain walker + walk_for_gc =====================================
//
// Walks the calling thread's frame-pointer chain and yields, for each
// frame whose return-PC lands at a known safepoint, the absolute
// addresses of live GC refs in that frame.
//
// Frame layout (both x86_64 Linux and aarch64 macOS use a saved-FP +
// saved-LR/RA pair at the top of every Cranelift-emitted frame):
//
// ```text
//   higher addresses
//     ↑
//   [ frame N-1 locals + spill slots ]
//   [ return address into frame N-1 ]    ← *(fp + 8)
//   [ saved frame pointer (frame N-1) ]  ← *fp
//   [ frame N locals + spill slots ]
//     ↓
//   lower addresses
// ```
//
// Given current frame's FP, walking is:
//   loop:
//     return_pc = *(fp + 8)
//     saved_fp  = *fp
//     // safepoint pc is the call-instruction PC, which is one of:
//     //   (a) return_pc - 5 on x86_64 (5-byte call)
//     //   (b) return_pc - 4 on aarch64 (4-byte BL)
//     // The stackmap's `pc_offset` field is what Cranelift's
//     // `code.buffer.user_stack_maps()` returned for the call —
//     // typically the byte after the call (= return_pc - fn_base).
//     // We try both `return_pc - fn_base` and `(return_pc - call_size) - fn_base`
//     // to be robust.
//     if let Some(record) = lookup_safepoint_for_return_pc(return_pc):
//        for entry in record.entries:
//           // frame_sp = fp - record.frame_size; addr = frame_sp + entry.sp_offset
//           yield (frame_sp + entry.sp_offset, entry.kind)
//     fp = saved_fp

/// A precise root location surfaced by `walk_for_gc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootLocation {
    /// Address on the stack where the live GC ref currently sits.
    pub addr: usize,
    /// Entry kind copied from the stackmap record (v1: always
    /// `STACKMAP_ENTRY_KIND_HEAP_POINTER`).
    pub kind: u8,
    /// The return-PC the walker matched against. Diagnostic; not
    /// load-bearing for callers.
    pub return_pc: usize,
}

/// Walk the current frame-pointer chain and return precise root
/// locations for every frame whose return-PC matches a known
/// safepoint. Returns an empty Vec when the stackmap index is not
/// initialised, when no FP can be obtained on the current host, or
/// when no frame matches a safepoint.
///
/// **Caller-frame walk.** The walker reads the FP of the immediate
/// caller (one frame above this function) via inline asm; that frame
/// itself is excluded from the walk (its return-PC is back into Rust
/// code, never a safepoint). Frames *above* the caller — i.e., the
/// Sigil call chain — are inspected.
#[inline(never)]
pub fn walk_for_gc() -> Vec<RootLocation> {
    let index = match init_index() {
        Some(i) => i,
        None => return Vec::new(),
    };
    let mut roots = Vec::new();
    let mut fp = current_caller_fp();
    while !fp.is_null() {
        let frame = unsafe { walk_frame(fp) };
        if !frame.saved_fp.is_null() {
            if let Some(record) = index.lookup(frame.return_pc) {
                // The safepoint at `frame.return_pc` lives in the
                // function that **called** the frame at `fp` (the
                // OUTER frame). The outer frame's FP is
                // `frame.saved_fp` (the value the inner prologue
                // saved); its SP at the safepoint is
                // `outer_FP - active_size`, where `active_size` is
                // what Cranelift records as the stackmap record's
                // `frame_size`. Using the inner FP here would
                // mis-address by the inner-vs-outer FP delta and
                // surface as "non-heap-pointer-shaped" values at
                // supposed GC slots — which is what PR #163 CI on
                // macos-14 surfaced before this fix.
                let outer_fp = frame.saved_fp as usize;
                let frame_sp = outer_fp.wrapping_sub(record.frame_size as usize);
                for entry in &record.entries {
                    roots.push(RootLocation {
                        addr: frame_sp.wrapping_add(entry.sp_offset as usize),
                        kind: entry.kind,
                        return_pc: frame.return_pc,
                    });
                }
            }
        }
        if frame.saved_fp.is_null() || frame.saved_fp as usize <= fp as usize {
            // Bottom of chain (or corruption); stop walking.
            break;
        }
        fp = frame.saved_fp;
    }
    roots
}

struct Frame {
    saved_fp: *const usize,
    return_pc: usize,
}

unsafe fn walk_frame(fp: *const usize) -> Frame {
    Frame {
        saved_fp: *fp as *const usize,
        return_pc: *fp.add(1),
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn current_caller_fp() -> *const usize {
    let fp: *const usize;
    // SAFETY: reading the current frame's saved rbp. With
    // `#[inline(never)]` on the caller (`walk_for_gc`), this fn has a
    // standard prologue and `rbp` is the saved-rbp of the caller.
    // We then return *rbp = the caller's saved rbp = the caller-of-
    // caller's frame pointer, which is the first frame we want to
    // inspect (the safepoint that called into Sigil code).
    unsafe {
        std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
        if fp.is_null() {
            std::ptr::null()
        } else {
            *fp as *const usize
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn current_caller_fp() -> *const usize {
    let fp: *const usize;
    // SAFETY: reading the current frame's saved x29 (FP). Same shape
    // as the x86_64 path; aarch64 uses x29 as the frame pointer by
    // convention and Cranelift's prologue saves it at the top of the
    // frame.
    unsafe {
        std::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags));
        if fp.is_null() {
            std::ptr::null()
        } else {
            *fp as *const usize
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn current_caller_fp() -> *const usize {
    std::ptr::null()
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
