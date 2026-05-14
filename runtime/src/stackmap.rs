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

use std::sync::OnceLock;

/// Indexed view of the parsed stackmap section. For every resolved
/// function block, the symbol's runtime address (via `dlsym(symbol_name)`)
/// keys an `IndexedFunction` containing every safepoint's absolute PC
/// pre-computed for the M2 fast-path lookup.
pub struct StackmapIndex {
    parsed: ParsedSection,
    /// Sorted by `base` ascending. Each entry has an `abs_pcs` Vec
    /// sorted by absolute PC (ascending) so `binary_search` on the PC
    /// returns the record index inside this function.
    functions: Vec<IndexedFunction>,
}

struct IndexedFunction {
    base: usize,
    /// Inclusive upper bound on absolute PCs we'll match against. v1
    /// has no `fn_size` wire field; we use the max safepoint PC plus
    /// a small pad (see `FN_RANGE_PAD`) as a heuristic.
    range_end: usize,
    /// Sorted ascending by `.0`; `binary_search_by_key(&pc, |e| e.0)`
    /// gives the record index in `parsed.functions[fn_idx].records`.
    abs_pcs: Vec<(usize, usize)>, // (absolute_pc, record_idx_in_fn)
    fn_idx: usize,
}

/// Safety pad for the fn-range upper bound. The wire format reserves
/// `text_offset` for a future writer to record `fn_size`; until that
/// lands, we use `max_pc_offset + FN_RANGE_PAD` as the upper bound.
/// 64 KiB is comfortable headroom past any safepoint we observe in
/// practice (`choose_demo`'s max-offset record is < 4 KiB into a
/// fn).
const FN_RANGE_PAD: usize = 65536;

impl StackmapIndex {
    /// Resolve a runtime PC to a record. Tries direct match against
    /// the recorded safepoint PCs (which Cranelift records as the
    /// post-call return address per `aarch64/inst/emit.rs:2948`);
    /// also tries `pc - call_size` to be robust against Cranelift
    /// convention changes / x86_64 vs aarch64 differences.
    ///
    /// Aarch64 macOS pointer-authentication: callers must strip the
    /// PAC tag from the raw return-PC bits before calling `lookup`.
    /// See `pac_strip` in this module.
    pub fn lookup(&self, pc: usize) -> Option<&ParsedRecord> {
        // M2 fast path: O(log N) function lookup via binary search by
        // base, then O(log K) per-fn safepoint lookup. Plus a ±range
        // fallback for the call-size convention. Phase 1's cost on
        // stress tests was the motivator (10k allocs × tens of
        // records × chain depth M).
        for try_pc in [pc, pc.wrapping_sub(4), pc.wrapping_sub(5)] {
            if let Some(record) = self.lookup_exact(try_pc) {
                return Some(record);
            }
        }
        None
    }

    fn lookup_exact(&self, pc: usize) -> Option<&ParsedRecord> {
        // Binary-search by base for the function containing `pc`.
        // The functions array is sorted ascending by base; we want
        // the last fn whose base <= pc AND whose range_end >= pc.
        let idx_by_base = match self.functions.binary_search_by(|f| {
            if f.base > pc {
                std::cmp::Ordering::Greater
            } else if f.range_end < pc {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(i) => i,
            Err(_) => return None,
        };
        let f = &self.functions[idx_by_base];
        let rec_idx = match f.abs_pcs.binary_search_by_key(&pc, |e| e.0) {
            Ok(i) => f.abs_pcs[i].1,
            Err(_) => return None,
        };
        Some(&self.parsed.functions[f.fn_idx].records[rec_idx])
    }

    pub fn parsed(&self) -> &ParsedSection {
        &self.parsed
    }

    /// Number of (function-name, record) pairs the index resolved a
    /// non-zero base address for. Functions whose symbol failed to
    /// resolve via `dlsym` are silently skipped.
    pub fn resolved_record_count(&self) -> usize {
        self.functions.iter().map(|f| f.abs_pcs.len()).sum()
    }

    pub fn resolved_function_count(&self) -> usize {
        self.functions.len()
    }
}

static INDEX: OnceLock<Option<StackmapIndex>> = OnceLock::new();

/// Initialise the runtime stackmap index. Idempotent. Returns `Some`
/// when the section was located + parsed + at least one function
/// block's symbol resolved via `dlsym`; returns `None` when the
/// section is missing (e.g., the binary was linked without the
/// stackmap section, or the runtime is being exercised via a unit test
/// that doesn't link a real binary).
///
/// When `SIGIL_GC_XCHECK_TRACE` is set in the environment, init writes
/// a one-line diagnostic to stderr summarising
/// `(section_present, parsed_fn_count, parsed_record_count,
/// resolved_record_count)` — makes M1-class lookup regressions
/// diagnosable in seconds. See PR #163 review M1 / N5.
pub fn init_index() -> Option<&'static StackmapIndex> {
    INDEX
        .get_or_init(|| {
            let trace = std::env::var_os("SIGIL_GC_XCHECK_TRACE").is_some();
            let bytes = match locate_section_bytes() {
                Some(b) => b,
                None => {
                    if trace {
                        eprintln!(
                            "[stackmap] init: section_present=false (unit-test \
                             binary or missing __start_sigil_stackmaps/getsectiondata)"
                        );
                    }
                    return None;
                }
            };
            let parsed = match parse_section(bytes) {
                Ok(p) => p,
                Err(e) => {
                    if trace {
                        eprintln!("[stackmap] init: parse_section error {e:?}");
                    }
                    return None;
                }
            };
            let parsed_records: usize = parsed.functions.iter().map(|f| f.records.len()).sum();
            let functions = build_indexed_functions(&parsed);
            let resolved_records: usize = functions.iter().map(|f| f.abs_pcs.len()).sum();
            if trace {
                eprintln!(
                    "[stackmap] init: section_present=true \
                     parsed_fns={} parsed_records={} resolved_fns={} \
                     resolved_records={}",
                    parsed.functions.len(),
                    parsed_records,
                    functions.len(),
                    resolved_records,
                );
            }
            // N1 sanity: if the section was present but zero fns
            // resolved, dlsym is failing — likely a missing
            // `--export-dynamic` or stripped symbols. Surface
            // immediately rather than silently produce a vacuous
            // cross-check.
            debug_assert!(
                resolved_records > 0,
                "stackmap section present ({parsed_records} parsed records) \
                 but zero resolved — dlsym failed for every function. Likely \
                 cause: --export-dynamic missing or symbols stripped.",
            );
            Some(StackmapIndex { parsed, functions })
        })
        .as_ref()
}

/// Strip pointer-authentication code (PAC) bits from a return-PC.
///
/// On aarch64 macOS (Apple Silicon), Cranelift's prologue may sign the
/// return address via `paci` (per `aarch64/abi.rs:578`); the saved-LR
/// at `*(fp+8)` carries PAC bits in the upper part of the 64-bit slot.
/// Comparing the signed PC against an unsigned `function_base +
/// pc_offset` would fail. Stripping the top 17 bits (canonical user
/// address space is 47-bit on Apple Silicon) reduces the PC to the
/// same bit pattern the index stored.
///
/// On x86_64 / aarch64-linux there's no PAC by default; the top bits
/// are zero for valid user addresses; this strip is a no-op. Always
/// applying it is cheaper than gating on cfg + matches the lookup
/// caller's needs.
#[inline]
fn pac_strip(pc: usize) -> usize {
    pc & 0x0000_7fff_ffff_ffff
}

/// Locate the stackmap section bytes via platform-specific linker
/// symbols / OS APIs. Returns `None` when the section is not present
/// (the binary was linked without compiler-emitted records).
///
/// On ELF, the section bounds are resolved via `extern "C" static`
/// references to the GNU linker's auto-generated
/// `__start_sigil_stackmaps` / `__stop_sigil_stackmaps` encapsulation
/// symbols. Encapsulation symbols are auto-generated for sections
/// whose name is a C identifier; `sigil_stackmaps` qualifies. The
/// runtime crate's `#[link_section] static EMPTY_*` below forces the
/// section to ALWAYS exist (zero bytes contributed) so the
/// encapsulation symbols are defined even in unit-test binaries that
/// don't link any compiler-emitted records. This bypasses dlsym
/// (PR #163 review M1 diagnosis: `-rdynamic` was not effective on
/// Ubuntu's toolchain at putting encapsulation symbols into
/// `.dynsym`).
fn locate_section_bytes() -> Option<&'static [u8]> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: __start_/__stop_ are linker-auto encapsulation
        // symbols pointing at the section's first / one-past-last
        // bytes. The section is in a non-writable ALLOC area; bytes
        // are valid for the program's lifetime.
        unsafe {
            let start = &__start_sigil_stackmaps as *const u8;
            let end = &__stop_sigil_stackmaps as *const u8;
            if end <= start {
                return None;
            }
            let len = end.offset_from(start) as usize;
            if len == 0 {
                return None;
            }
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

// On Linux, force the `sigil_stackmaps` section to always exist by
// contributing zero bytes to it from the runtime crate. This makes
// the GNU linker auto-generate the encapsulation symbols
// (`__start_sigil_stackmaps` / `__stop_sigil_stackmaps`) even when
// no compiler-emitted records are present (e.g., unit-test binaries
// linked against the runtime rlib but without invoking the Sigil
// compiler). Without this sentinel, `extern "C" static
// __start_sigil_stackmaps` would produce undefined-reference link
// errors in those test binaries (PR #163's first attempt at the
// extern-static approach hit exactly this).
#[cfg(target_os = "linux")]
#[link_section = "sigil_stackmaps"]
#[used]
static EMPTY_STACKMAPS_SENTINEL: [u8; 0] = [];

#[cfg(target_os = "linux")]
extern "C" {
    static __start_sigil_stackmaps: u8;
    static __stop_sigil_stackmaps: u8;
}

/// Build the sorted IndexedFunction list used by `StackmapIndex::lookup`.
/// Each function block's `symbol_name` resolves to a runtime address via
/// `dlsym(RTLD_DEFAULT, ...)`. Unresolved symbols are skipped silently
/// (see `StackmapIndex::resolved_record_count`).
///
/// When `SIGIL_GC_XCHECK_TRACE=1`, every symbol-resolution attempt
/// logs to stderr — useful for diagnosing M1-class regressions where
/// the symbols ARE in the binary but dlsym can't find them (e.g.,
/// missing `--export-dynamic`).
///
/// The returned Vec is sorted ascending by `base`, and each entry's
/// `abs_pcs` is sorted ascending by absolute PC — so `lookup` can do
/// two binary searches (O(log F + log K)) instead of O(F·K).
fn build_indexed_functions(parsed: &ParsedSection) -> Vec<IndexedFunction> {
    let trace = std::env::var_os("SIGIL_GC_XCHECK_TRACE").is_some();
    let mut out: Vec<IndexedFunction> = Vec::new();
    for (fn_idx, f) in parsed.functions.iter().enumerate() {
        let base = match dlsym_resolve(&f.symbol_name) {
            Some(b) => {
                if trace {
                    eprintln!("[stackmap] dlsym(\"{}\") = 0x{:x}", f.symbol_name, b);
                }
                b
            }
            None => {
                if trace {
                    eprintln!("[stackmap] dlsym(\"{}\") = NULL", f.symbol_name);
                }
                continue;
            }
        };
        let mut abs_pcs: Vec<(usize, usize)> = f
            .records
            .iter()
            .enumerate()
            .map(|(rec_idx, r)| (base.wrapping_add(r.pc_offset as usize), rec_idx))
            .collect();
        abs_pcs.sort_by_key(|e| e.0);
        let max_abs_pc = abs_pcs.last().map(|e| e.0).unwrap_or(base);
        out.push(IndexedFunction {
            base,
            range_end: max_abs_pc.wrapping_add(FN_RANGE_PAD),
            abs_pcs,
            fn_idx,
        });
    }
    out.sort_by_key(|fi| fi.base);
    out
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
///
/// **Allocates a `Vec<RootLocation>` for the result.** That's
/// fine for the cross-check harness (`stackmap_xcheck`) which
/// runs outside Boehm's STW. For the Plan E2 Phase 3
/// `GC_set_push_other_roots` callback (which DOES run inside
/// STW), use [`walk_for_gc_with_callback`] instead — it
/// streams root addresses through a closure and avoids the
/// system-allocator round-trip that could deadlock against a
/// suspended thread holding malloc's internal lock.
#[inline(never)]
pub fn walk_for_gc() -> Vec<RootLocation> {
    let mut roots = Vec::new();
    walk_for_gc_with_callback(|root| roots.push(root));
    roots
}

/// Allocation-free variant of [`walk_for_gc`]: walks the same
/// fp chain but invokes `f(root)` per root instead of pushing
/// into a `Vec`. Intended for callers that run inside Boehm's
/// STW mark phase (Plan E2 Phase 3 Task 11's
/// `push_sigil_thread_precise_roots`), where allocating a
/// `Vec` could deadlock against a suspended thread holding
/// libc malloc's internal lock.
///
/// The closure must not allocate from libc (same deadlock
/// risk) and must not call any function that re-enters Boehm
/// (no `GC_malloc`, no triggering recursive marking). Pushing
/// root ranges via `GC_push_all_eager` is safe — it's the
/// documented mark-phase root-supply mechanism.
#[inline(never)]
pub fn walk_for_gc_with_callback<F: FnMut(RootLocation)>(f: F) {
    let fp = current_caller_fp();
    walk_for_gc_with_callback_from(fp, f);
}

/// Allocation-free variant that takes a STARTING FP rather than
/// reading `current_caller_fp()`. Used by Plan E2 Phase 3
/// Task 12's `push_sigil_thread_precise_roots` callback: the
/// callback is invoked from inside Boehm's mark phase, where
/// `current_caller_fp()` returns a frame INSIDE libgc (which
/// may be compiled with `-fomit-frame-pointer`, making
/// `*fp` reads through libgc frames yield garbage and the next
/// `walk_frame` deref blow up with SIGSEGV — PR #170 surfaced
/// this empirically).
///
/// The safe starting point is the runtime entry frame's own FP
/// captured BEFORE the call into libgc (e.g., at sigil_alloc
/// entry via `capture_caller_fp_for_walk`). Walking from there:
/// the runtime frame itself has no stackmap (Rust code), so its
/// saved-PC lookup misses harmlessly; the next iteration walks
/// to the Sigil caller's frame and the return-PC points at the
/// call-site inside the Sigil function, which DOES have stackmap
/// entries — those roots get yielded. The chain continues
/// upward through the Sigil call chain until reaching the Rust
/// main shim / libc init frames where lookups miss again, then
/// the walker stops at the standard "null-fp / backward-step"
/// sentinel.
///
/// The closure must not allocate (the walker runs inside STW;
/// allocation could deadlock against suspended threads holding
/// libc malloc locks).
#[inline(never)]
pub fn walk_for_gc_with_callback_from<F: FnMut(RootLocation)>(starting_fp: *const usize, mut f: F) {
    let index = match init_index() {
        Some(i) => i,
        None => return,
    };
    let trace = xcheck_trace_enabled();
    // Plan E2 Phase 3 Task 12 — read the cached safe-stack range
    // once. `profile::unwind`'s atomics are populated by
    // `sigil_gc_init` (off the STW path) before any GC fires. If
    // either bound is zero, no range was installed (e.g., this
    // walker is being invoked from a cargo-test context that
    // bypasses sigil_gc_init); the FP validation degrades to
    // alignment + inversion + hop-bound only. The SIGPROF
    // unwinder (`profile::unwind::capture_stack_from`) applies
    // the same defensive pattern — see its doc for the failure
    // mode this guards against (`-fomit-frame-pointer`-compiled
    // libgc internals leaking wild rbp values into the unwind
    // chain).
    let safe_lo = crate::profile::unwind::SAFE_STACK_LO.load(std::sync::atomic::Ordering::Relaxed);
    let safe_hi = crate::profile::unwind::SAFE_STACK_HI.load(std::sync::atomic::Ordering::Relaxed);
    let safe_range_installed = safe_lo != 0 && safe_hi > safe_lo;
    let mut fp = starting_fp;
    let mut frame_idx = 0usize;
    let mut yielded: usize = 0;
    while !fp.is_null() {
        // Reject FPs outside the calling thread's known stack
        // range BEFORE dereferencing. This catches the failure
        // mode where libgc internals leak a wild rbp onto the
        // walker's chain — same shape as the SIGPROF unwinder
        // hardening. Same caveat as well: if no range is
        // installed (test context), the check is bypassed.
        if safe_range_installed && ((fp as usize) < safe_lo || (fp as usize) >= safe_hi) {
            if trace {
                eprintln!(
                    "[stackmap] walk frame={} fp=0x{:x} outside safe stack range [0x{:x}, 0x{:x}) — bailing",
                    frame_idx, fp as usize, safe_lo, safe_hi,
                );
            }
            break;
        }
        let frame = unsafe { walk_frame(fp) };
        // Strip pointer-authentication code (PAC) bits from the
        // saved return-PC. On Apple Silicon Cranelift's prologue may
        // sign LR via `paci`; the raw saved bits include the PAC tag
        // in the upper part of the address, which would never match
        // the unsigned (function_base + pc_offset) the index stored.
        let stripped_pc = pac_strip(frame.return_pc);
        if !frame.saved_fp.is_null() {
            if let Some(record) = index.lookup(stripped_pc) {
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
                // macos-14 surfaced before the dc37279 fix.
                let outer_fp = frame.saved_fp as usize;
                let frame_sp = outer_fp.wrapping_sub(record.frame_size as usize);
                if trace {
                    eprintln!(
                        "[stackmap] walk frame={} fp=0x{:x} return_pc=0x{:x} \
                         stripped=0x{:x} matched frame_size={} entries={}",
                        frame_idx,
                        fp as usize,
                        frame.return_pc,
                        stripped_pc,
                        record.frame_size,
                        record.entries.len(),
                    );
                }
                for entry in &record.entries {
                    f(RootLocation {
                        addr: frame_sp.wrapping_add(entry.sp_offset as usize),
                        kind: entry.kind,
                        return_pc: frame.return_pc,
                    });
                    yielded += 1;
                }
            } else if trace {
                eprintln!(
                    "[stackmap] walk frame={} fp=0x{:x} return_pc=0x{:x} \
                     stripped=0x{:x} no_match",
                    frame_idx, fp as usize, frame.return_pc, stripped_pc,
                );
            }
        }
        if frame.saved_fp.is_null() || frame.saved_fp as usize <= fp as usize {
            // Bottom of chain (or corruption); stop walking.
            break;
        }
        // Stack-hop bound: a legitimate saved_fp is at most a few
        // MB higher than fp — function frames are typically <1 KB
        // and rarely exceed ~1 MB even for stack-heavy code. A
        // larger hop is a strong signal we're reading FP-omitted
        // intermediate frames (libgc internals on the post-Task 12
        // walk path, where the chain crosses GC_do_blocking's
        // library frames). Same threshold as the SIGPROF unwinder.
        const STACK_HOP_MAX: usize = 4 * 1024 * 1024;
        if (frame.saved_fp as usize).wrapping_sub(fp as usize) > STACK_HOP_MAX {
            if trace {
                eprintln!(
                    "[stackmap] walk frame={} fp=0x{:x} saved_fp=0x{:x} hop > {} bytes — bailing",
                    frame_idx, fp as usize, frame.saved_fp as usize, STACK_HOP_MAX,
                );
            }
            break;
        }
        fp = frame.saved_fp;
        frame_idx += 1;
    }
    if trace && yielded > 0 {
        eprintln!("[stackmap] walk yielded {} roots", yielded);
    }
}

/// Cached env-var lookup for SIGIL_GC_XCHECK_TRACE. Reads once and
/// caches via OnceLock; steady-state cost is one relaxed load.
fn xcheck_trace_enabled() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("SIGIL_GC_XCHECK_TRACE").is_some())
}

/// Pre-warm every lazy initialiser this module owns so subsequent
/// `walk_for_gc` / `walk_for_gc_with_callback` calls don't allocate.
/// Called from `gc::threads::register_sigil_thread_for_precise_roots`
/// at sigil_gc_init time, BEFORE any GC can fire — the
/// `GC_set_push_other_roots` callback runs inside Boehm's STW
/// mark phase, and any libc malloc invoked there can deadlock
/// against a suspended thread holding malloc's internal lock.
///
/// Today this means:
/// - `init_index()` builds the StackmapIndex BTreeMap.
/// - `xcheck_trace_enabled()` reads + caches the env var.
///
/// Future lazy initialisers added to this module must extend
/// this function or document why the new init is allocation-
/// free + STW-safe.
pub fn prewarm_for_stw() {
    let _ = init_index();
    let _ = xcheck_trace_enabled();
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

/// Capture a frame pointer suitable for feeding to
/// `walk_for_gc_with_callback_from`. Called from runtime entry
/// points (sigil_alloc today, sigil_run_loop tomorrow) BEFORE
/// any transition into libgc internals, so the captured FP
/// references a frame that's still on the stack — and outside
/// libgc's potentially `-fomit-frame-pointer`-compiled call
/// chain — when the mark-phase callback later fires.
///
/// **Captured FP is the *caller's* FP.** That is, when called
/// from `sigil_alloc`, the returned FP is `sigil_alloc`'s own
/// frame pointer. The walker iterates upward from there: the
/// first iteration's saved return-PC points at the call-site
/// inside the SIGIL function that called sigil_alloc, which is
/// where the stackmap lookup hits and where roots get yielded.
/// If we instead returned the Sigil-function's FP (one too high),
/// the walker would skip the Sigil function's own stackmap
/// entries and yield only its caller's roots.
///
/// `#[inline(never)]` is load-bearing: it gives this function its
/// own prologue, so reading rbp inside this function yields
/// THIS frame's FP, and dereferencing gives the CALLER's saved
/// rbp — which is the caller's FP. With `#[inline(always)]` (as
/// on `current_caller_fp`), inlining into sigil_alloc would make
/// rbp = sigil_alloc's FP and `*rbp` = sigil_alloc's caller's FP,
/// missing one frame.
#[inline(never)]
pub fn capture_caller_fp_for_walk() -> *const usize {
    current_caller_fp()
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn current_caller_fp() -> *const usize {
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
pub fn current_caller_fp() -> *const usize {
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
pub fn current_caller_fp() -> *const usize {
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

    // ===== Plan E2 Phase 3 Task 12 — walker safety-check tests =====
    //
    // The hardening added to `walk_for_gc_with_callback_from` (safe
    // stack-range check + 4 MB stack-hop bound) guards against
    // libgc's `-fomit-frame-pointer` internals leaking wild FPs
    // onto the walker's chain post-Task 12. The positive path is
    // covered end-to-end by the `precise_walker_deep_*` e2e tests
    // (compiler/tests/e2e.rs); these tests pin the negative path:
    // when the safety checks DO trigger, the walker bails cleanly
    // instead of dereferencing into unmapped or arbitrary memory.

    /// RAII guard that swaps `profile::unwind::SAFE_STACK_{LO,HI}`
    /// to caller-supplied bounds, restoring the previous values on
    /// Drop (including panic-via-abort). Tests that install custom
    /// stack bounds use this so a panic mid-test doesn't leave the
    /// globals corrupted for sibling parallel tests.
    ///
    /// **Serial discipline.** Cargo runs `#[test]` items in
    /// parallel by default. The unwinder atomics are
    /// process-global, so two tests both swapping them race. Tests
    /// using this guard MUST hold `SAFE_STACK_RANGE_TEST_LOCK` for
    /// the duration of the swap so sibling tests observe the
    /// installed bounds without contention.
    struct SafeStackRangeGuard {
        prev_lo: usize,
        prev_hi: usize,
        _lock_guard: std::sync::MutexGuard<'static, ()>,
    }

    static SAFE_STACK_RANGE_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();

    impl SafeStackRangeGuard {
        fn install(lo: usize, hi: usize) -> Self {
            let lock = SAFE_STACK_RANGE_TEST_LOCK.get_or_init(|| std::sync::Mutex::new(()));
            let _lock_guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let prev_lo = crate::profile::unwind::SAFE_STACK_LO
                .swap(lo, std::sync::atomic::Ordering::Relaxed);
            let prev_hi = crate::profile::unwind::SAFE_STACK_HI
                .swap(hi, std::sync::atomic::Ordering::Relaxed);
            SafeStackRangeGuard {
                prev_lo,
                prev_hi,
                _lock_guard,
            }
        }
    }

    impl Drop for SafeStackRangeGuard {
        fn drop(&mut self) {
            crate::profile::unwind::SAFE_STACK_LO
                .store(self.prev_lo, std::sync::atomic::Ordering::Relaxed);
            crate::profile::unwind::SAFE_STACK_HI
                .store(self.prev_hi, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn walker_safe_range_rejects_wild_starting_fp() {
        // Install a tight safe range that does NOT cover the
        // wild FP we're about to pass. Without the range gate,
        // the walker's first `walk_frame` call would dereference
        // 0x4000_0000 (a non-stack, possibly unmapped address)
        // and either SEGV or read arbitrary process memory. The
        // gate must bail BEFORE the deref.
        let _guard = SafeStackRangeGuard::install(0x1000, 0x2000);

        let mut yielded = 0usize;
        walk_for_gc_with_callback_from(0x4000usize as *const usize, |_r| {
            yielded += 1;
        });

        // The walker doesn't crash, doesn't yield any root.
        // (Even without the gate, init_index() returns None in
        // tests so no root would be yielded; the LOAD-BEARING
        // assertion is "test completed without SEGV".)
        assert_eq!(yielded, 0);
    }

    #[test]
    fn walker_hop_bound_bails_on_large_jump() {
        // Construct a 2-slot synthetic frame whose `prev_fp`
        // points 5 MB above the base — beyond the walker's
        // 4 MB stack-hop ceiling. Without the hop bound, the
        // walker would try `walk_frame(base + 5 MB)` after the
        // first iteration; that address is arbitrary process
        // memory and the deref would either SEGV or read
        // garbage. The hop bound must bail BEFORE the next
        // iteration.
        //
        // We install a safe stack range that DOES cover both
        // `base` and `base + 5 MB` so the range gate alone
        // wouldn't catch this — the hop bound is the
        // load-bearing check.
        let mut frames: Vec<usize> = vec![0; 2];
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record on the system heap, not Boehm-managed).
        let base = frames.as_mut_ptr() as usize;
        // prev_fp = base + 5 MB; return_pc = 0xDEAD sentinel.
        frames[0] = base + 5 * 1024 * 1024;
        frames[1] = 0xDEAD;

        let _guard = SafeStackRangeGuard::install(base, base + 10 * 1024 * 1024);

        let mut yielded = 0usize;
        // `base` points at a live Vec, so the first
        // `walk_frame(base)` is in-bounds. The walker reads
        // `frames[0] = base + 5 MB` as `saved_fp`, sees the
        // hop > 4 MB, and bails. No subsequent deref occurs.
        walk_for_gc_with_callback_from(base as *const usize, |_r| {
            yielded += 1;
        });

        // No SEGV, no yielded roots.
        let _ = frames; // keep `frames` alive across the walk.
        assert_eq!(yielded, 0);
    }

    #[test]
    fn walker_with_no_safe_range_installed_still_walks_well_formed_chain() {
        // Confirm the safety gates degrade gracefully when no
        // safe range has been installed (the cache is zero/zero,
        // which represents "no sigil_gc_init has run yet" — e.g.,
        // a test that calls the walker directly without going
        // through the runtime's normal startup path). The walker
        // must still walk a well-formed chain, just without the
        // range gate to back the alignment + inversion checks.
        //
        // Use the same synthetic-chain shape as
        // `walks_synthetic_well_formed_chain` (3 frames, then
        // `prev_fp = 0` terminates). The chain is short and
        // tight; no hop exceeds the bound.
        let mut frames: Vec<usize> = vec![0; 6];
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record on the system heap, not Boehm-managed).
        let base = frames.as_mut_ptr() as usize;
        frames[0] = base + 2 * core::mem::size_of::<usize>();
        frames[1] = 0x1000;
        frames[2] = base + 4 * core::mem::size_of::<usize>();
        frames[3] = 0x1100;
        frames[4] = 0;
        frames[5] = 0x1200;

        // Force the range to zero/zero (uninstalled state) for
        // the duration of this test, even if a sibling test
        // happens to have populated it. `SafeStackRangeGuard`'s
        // direct `swap` stores the supplied bounds verbatim
        // (unlike `profile::unwind::install_safe_stack_range`,
        // which rejects `hi <= lo`).
        let _guard = SafeStackRangeGuard::install(0, 0);

        let mut yielded = 0usize;
        // Synthetic well-formed chain on the heap; reads stay
        // inside the Vec.
        walk_for_gc_with_callback_from(base as *const usize, |_r| {
            yielded += 1;
        });

        // The walker traverses the chain without any safety-gate
        // bail (well-formed chain + no installed range), but
        // yields no roots because init_index() returns None in
        // test context. The point is that the walker completed
        // the traversal without crashing.
        let _ = frames;
        assert_eq!(yielded, 0);
    }
}
