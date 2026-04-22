//! Safepoint metadata — plan A1 task 0.11.
//!
//! v1 codegen emits a safepoint record for every Cranelift `call` and
//! `call_indirect`. Records are written to a custom object-file section:
//!
//! - ELF (Linux):   `.sigil_stackmaps`
//! - Mach-O:        `__SIGIL,__stackmaps` (segment `__SIGIL`, section `__stackmaps`)
//!
//! v1's Boehm runtime ignores the section; v2's precise GC parses it. The
//! binary format is documented in `runtime/README.md` and implemented by
//! `StackMapBuilder` in the compiler crate (`compiler/src/codegen.rs`). This
//! runtime-side module only owns the section-name constants so both
//! compiler and runtime agree.

/// ELF section name used on `x86_64-unknown-linux-gnu`.
pub const ELF_SECTION_NAME: &str = ".sigil_stackmaps";

/// Mach-O segment + section pair used on `aarch64-apple-darwin`.
pub const MACHO_SEGMENT_NAME: &str = "__SIGIL";
pub const MACHO_SECTION_NAME: &str = "__stackmaps";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_names_are_fixed() {
        assert_eq!(ELF_SECTION_NAME, ".sigil_stackmaps");
        assert_eq!(MACHO_SEGMENT_NAME, "__SIGIL");
        assert_eq!(MACHO_SECTION_NAME, "__stackmaps");
    }
}
