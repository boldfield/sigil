//! Stackmap section wire format — single source of truth.
//!
//! The compiler emits one safepoint record per Cranelift `call` /
//! `call_indirect` into an object-file section:
//!
//! - ELF (Linux):   `sigil_stackmaps`  (no leading `.` — see below)
//! - Mach-O:        `__SIGIL,__stackmaps`
//!
//! Plan A1 shipped **version 0 (placeholder)** records: `live_count = 0`,
//! `pc_offset` was a Cranelift `Inst` handle (not a real post-regalloc
//! offset), and every record had a placeholder flag bit set. Plan E2
//! Phase 1 Task 4 replaces this with **version 1 — real safepoint data
//! grouped per function**. The compiler is the v1-only writer from this
//! task forward; the runtime parser accepts v1 only and rejects v0 as a
//! stale build artifact.
//!
//! Binary format. **The wire is little-endian regardless of host
//! endianness** — the writer commits to LE via `to_le_bytes()`
//! unconditionally; the reader uses `from_le_bytes()` unconditionally.
//! Both currently-supported targets (aarch64-darwin, x86_64-linux)
//! are LE, so this matches host endianness in practice. A future
//! port to a BE host would still produce/consume LE bytes — port-
//! time work is verifying the runtime reader correctness, not
//! changing the wire format.
//!
//! ```text
//! section header (12 bytes):
//!   magic:4 "SGST" | version:4 | fn_count:4
//!
//! per-function block (variable size):
//!   fn_header:12 = name_len:4 | record_count:4 | text_offset:4 (reserved=0)
//!   name: name_len bytes (UTF-8 linker symbol, no NUL terminator)
//!   records[record_count]:
//!     record_header:12 = pc_offset:4 (function-local) | frame_size:4 |
//!                        entry_count:2 | flags:2
//!     entries[entry_count]:
//!       entry:5 = kind:1 | sp_offset:4
//! ```
//!
//! `pc_offset` is the offset in bytes from the function's first byte
//! (function-local). The runtime reader resolves the function's base
//! via `dlsym(name)` and adds `pc_offset` to obtain the absolute
//! safepoint PC.
//!
//! `sp_offset` is the offset from the safepoint's SP, growing toward
//! higher addresses (per Cranelift's `UserStackMapEntry::offset` —
//! pointer to the live ref is `sp + sp_offset`).
//!
//! `text_offset` is reserved as zero in v1. A future version may use
//! it to record the function's `.text`-relative offset so the runtime
//! does not need `dlsym` at lookup time.
//!
//! ## ELF section name discipline
//!
//! The ELF section name is `sigil_stackmaps` (NOT `.sigil_stackmaps`).
//! The leading dot was dropped in Plan E2 Phase 1 Task 5 so that the
//! GNU linker auto-generates `__start_sigil_stackmaps` /
//! `__stop_sigil_stackmaps` symbols pointing at the section bounds —
//! the runtime reader uses those directly to locate the section bytes
//! without parsing `/proc/self/exe`. The Mach-O `__SIGIL,__stackmaps`
//! pair is unchanged; its runtime API is `getsectiondata`.

/// ELF section name used on `x86_64-unknown-linux-gnu`. No leading
/// dot — see crate-level doc-comment for the rationale (auto-generated
/// `__start_*` / `__stop_*` linker symbols).
pub const ELF_SECTION_NAME: &str = "sigil_stackmaps";

/// Mach-O segment + section pair used on `aarch64-apple-darwin`.
pub const MACHO_SEGMENT_NAME: &str = "__SIGIL";
pub const MACHO_SECTION_NAME: &str = "__stackmaps";

/// Section magic. Identifies a Sigil stackmap section to a precise-GC
/// reader regardless of the enclosing object format.
pub const STACKMAP_MAGIC: &[u8; 4] = b"SGST";

/// Version 0: Plan A1 placeholder format. Retired; the compiler no
/// longer emits v0 sections from Plan E2 Phase 1 Task 4 forward.
pub const STACKMAP_VERSION_PLACEHOLDER: u32 = 0;

/// Version 1: Plan E2 real-safepoint format. Per-function blocks with
/// real post-regalloc PC offsets, frame sizes, and entry lists.
pub const STACKMAP_VERSION_V1: u32 = 1;

/// Section-header size in bytes: 4 magic + 4 version + 4 fn_count.
pub const STACKMAP_HEADER_SIZE: usize = 12;

/// Per-function block header size in bytes:
/// 4 name_len + 4 record_count + 4 text_offset.
pub const STACKMAP_FN_HEADER_SIZE: usize = 12;

/// Per-record header size in bytes:
/// 4 pc_offset + 4 frame_size + 2 entry_count + 2 flags.
pub const STACKMAP_RECORD_HEADER_SIZE_V1: usize = 12;

/// Per-entry size in bytes: 1 kind + 4 sp_offset.
pub const STACKMAP_ENTRY_SIZE_V1: usize = 5;

/// Entry kind: heap-managed pointer. The only kind v1 emits — every
/// `declare_value_needs_stack_map` / `declare_var_needs_stack_map`
/// site in codegen flags a heap pointer. Phase 2 may add kinds for
/// boxed scalars if precise marking needs to distinguish them; the
/// runtime parser rejects any unknown kind via `UnknownEntryKind(k)`.
pub const STACKMAP_ENTRY_KIND_HEAP_POINTER: u8 = 0x01;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_names_are_fixed() {
        assert_eq!(ELF_SECTION_NAME, "sigil_stackmaps");
        assert_eq!(MACHO_SEGMENT_NAME, "__SIGIL");
        assert_eq!(MACHO_SECTION_NAME, "__stackmaps");
    }

    #[test]
    fn format_constants_are_fixed() {
        assert_eq!(STACKMAP_MAGIC, b"SGST");
        assert_eq!(STACKMAP_VERSION_PLACEHOLDER, 0);
        assert_eq!(STACKMAP_VERSION_V1, 1);
        assert_eq!(STACKMAP_HEADER_SIZE, 12);
        assert_eq!(STACKMAP_FN_HEADER_SIZE, 12);
        assert_eq!(STACKMAP_RECORD_HEADER_SIZE_V1, 12);
        assert_eq!(STACKMAP_ENTRY_SIZE_V1, 5);
        assert_eq!(STACKMAP_ENTRY_KIND_HEAP_POINTER, 0x01);
    }

    #[test]
    fn header_widths_sum_to_constants() {
        // Section header: 4 + 4 + 4 = 12.
        assert_eq!(
            core::mem::size_of::<[u8; 4]>()
                + core::mem::size_of::<u32>()
                + core::mem::size_of::<u32>(),
            STACKMAP_HEADER_SIZE
        );
        // Fn header: 4 + 4 + 4 = 12.
        assert_eq!(core::mem::size_of::<u32>() * 3, STACKMAP_FN_HEADER_SIZE);
        // Record header: 4 + 4 + 2 + 2 = 12.
        assert_eq!(
            core::mem::size_of::<u32>() * 2 + core::mem::size_of::<u16>() * 2,
            STACKMAP_RECORD_HEADER_SIZE_V1
        );
        // Entry: 1 + 4 = 5.
        assert_eq!(
            core::mem::size_of::<u8>() + core::mem::size_of::<u32>(),
            STACKMAP_ENTRY_SIZE_V1
        );
    }
}
