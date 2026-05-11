//! Profile symbol-table sidecar emitter — plan 2026-05-08-sigil-v2-
//! runtime-profile-data Phase 1, Task 2.
//!
//! Given a freshly-linked executable on disk, walk its symbol table and
//! write `<output>.symtab` next to it. The format is one tab-separated
//! line per function symbol:
//!
//! ```text
//! <text_offset_hex>\t<size_hex>\t<demangled_name>
//! ```
//!
//! Both hex columns are 16-character lower-case zero-padded so a
//! downstream tool can `lexcmp` for binary search. Lines are sorted by
//! ascending `text_offset_hex`, secondary by name.
//!
//! The full design rationale (why post-link parsing was chosen over the
//! pre-link `ObjectProduct` paths) lives at
//! `compiler/docs/profile-symbol-table.md`.
//!
//! The implementation depends on the `object` crate, which is already a
//! transitive dependency through `cranelift-object`. No new crate
//! dependency is added.
//!
//! Demangling is the inverse of `codegen::mangle_user_fn`: the
//! `sigil_user_` prefix is stripped (and the historical `sigil_user_main`
//! special-case undone), and `__` is rewritten back to `$`. Anything that
//! does not match the mangling pattern (runtime fns, libc, libgc) is
//! passed through verbatim — those symbols are already human-readable.

use std::path::Path;

use cranelift_object::object;
use object::read::{File as ObjectFile, Object, ObjectSymbol};
use object::{BinaryFormat, SymbolKind};

/// One row of the sidecar: a single function symbol with its image-
/// relative address, code size, and demangled name. Public for unit
/// testing the writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymtabEntry {
    pub address: u64,
    pub size: u64,
    pub name: String,
}

/// Emit a `prog.symtab` sidecar alongside the linked executable at
/// `binary_path`. The sidecar path is `<output>.symtab`.
///
/// Reads the binary back from disk, walks its symbol table for entries
/// of kind [`SymbolKind::Text`], demangles each, and writes the sorted
/// result.
///
/// **Size synthesis on Mach-O.** ELF carries explicit symbol sizes;
/// Mach-O does not (the `object` crate returns `size() == 0` for most
/// Mach-O symbols). Rather than discarding those entries (which would
/// produce an empty sidecar on macOS), we sort by address and
/// synthesize size as the gap to the next text symbol. The trailing
/// symbol uses a conservative 4 KiB sentinel.
///
/// **Mach-O underscore prefix.** C-style external symbols on Mach-O
/// pick up a leading `_` (Apple's traditional convention). We strip it
/// before demangling so `_sigil_user_main` resolves to `main` on macOS
/// the same way `sigil_user_main` does on ELF.
///
/// Returns the number of entries written on success.
pub fn write_for_binary(binary_path: &Path, sidecar_path: &Path) -> Result<usize, String> {
    let bytes =
        std::fs::read(binary_path).map_err(|e| format!("read {}: {}", binary_path.display(), e))?;
    let obj = ObjectFile::parse(&bytes[..])
        .map_err(|e| format!("parse {}: {e}", binary_path.display()))?;

    let is_macho = obj.format() == BinaryFormat::MachO;

    // First pass: collect every text symbol with a name, regardless of
    // whether the reader reports a size.
    let mut raw: Vec<(u64, u64, String)> = Vec::new();
    for sym in obj.symbols() {
        if sym.kind() != SymbolKind::Text {
            continue;
        }
        let raw_name = match sym.name() {
            Ok(n) if !n.is_empty() => n,
            _ => continue,
        };
        // Mach-O: strip the leading underscore that `cc` prepends to
        // every C-extern name.
        let stripped = if is_macho {
            raw_name.strip_prefix('_').unwrap_or(raw_name)
        } else {
            raw_name
        };
        raw.push((sym.address(), sym.size(), demangle(stripped)));
    }

    // Sort by address ascending (secondary by name for byte-stable
    // output on ties).
    raw.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));

    // Second pass: fill in zero sizes from the gap to the next entry.
    // Trailing entry gets a conservative 4 KiB sentinel.
    const TRAILING_SIZE_SENTINEL: u64 = 4096;
    let mut entries: Vec<SymtabEntry> = Vec::with_capacity(raw.len());
    for i in 0..raw.len() {
        let (addr, size, name) = (raw[i].0, raw[i].1, raw[i].2.clone());
        let final_size = if size > 0 {
            size
        } else if let Some(next) = raw.iter().skip(i + 1).find(|n| n.0 > addr) {
            next.0 - addr
        } else {
            TRAILING_SIZE_SENTINEL
        };
        if final_size == 0 {
            continue;
        }
        entries.push(SymtabEntry {
            address: addr,
            size: final_size,
            name,
        });
    }

    let rendered = render(&entries);
    std::fs::write(sidecar_path, rendered)
        .map_err(|e| format!("write {}: {}", sidecar_path.display(), e))?;
    Ok(entries.len())
}

/// Render a slice of entries as the sidecar's wire bytes. Public for
/// unit testing.
pub fn render(entries: &[SymtabEntry]) -> String {
    let mut out = String::with_capacity(entries.len() * 64);
    for e in entries {
        out.push_str(&format!(
            "{:016x}\t{:016x}\t{}\n",
            e.address, e.size, e.name
        ));
    }
    out
}

/// Undo the compiler's name mangling for display in the profile sidecar.
///
/// Mirrors `codegen::mangle_user_fn`:
/// - `sigil_user_main` → `main`
/// - `sigil_user_<rest>` → `<rest>` with `__` → `$`
/// - any other input is passed through unchanged
pub fn demangle(name: &str) -> String {
    if name == "sigil_user_main" {
        return "main".to_string();
    }
    if let Some(rest) = name.strip_prefix("sigil_user_") {
        return rest.replace("__", "$");
    }
    name.to_string()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn demangle_user_main_round_trip() {
        assert_eq!(demangle("sigil_user_main"), "main");
    }

    #[test]
    fn demangle_user_fn_strips_prefix() {
        assert_eq!(demangle("sigil_user_foo"), "foo");
        assert_eq!(demangle("sigil_user_my_helper"), "my_helper");
    }

    #[test]
    fn demangle_user_lambda_restores_dollar() {
        // codegen::mangle_user_fn rewrites `$lambda_3` → `sigil_user___lambda_3`
        // (because `$` → `__`). The inverse restores `$`.
        assert_eq!(demangle("sigil_user___lambda_3"), "$lambda_3");
    }

    #[test]
    fn demangle_passes_through_runtime_symbols() {
        // Runtime crate symbols and libc / libgc symbols stay verbatim —
        // they're already human-readable and useful in a profile.
        for name in &[
            "sigil_alloc",
            "sigil_perform",
            "sigil_io_println_arm",
            "sigil_handler_arm_42",
            "post_arm_k_7_2",
            "GC_malloc",
            "malloc",
            "main",
        ] {
            assert_eq!(&demangle(name), name);
        }
    }

    #[test]
    fn render_sorts_emit_matches_sort_invariant() {
        let entries = vec![
            SymtabEntry {
                address: 0x1054,
                size: 0x40,
                name: "main".into(),
            },
            SymtabEntry {
                address: 0x1020,
                size: 0x34,
                name: "sigil_alloc".into(),
            },
        ];
        // render() does NOT sort — the caller is responsible. We test
        // that the order in `entries` is preserved verbatim so the
        // caller-sort + render contract is observable.
        let out = render(&entries);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("main"));
        assert!(lines[1].ends_with("sigil_alloc"));
    }

    #[test]
    fn render_uses_16char_lowercase_hex() {
        let entries = vec![SymtabEntry {
            address: 0xabcdef,
            size: 0x10,
            name: "x".into(),
        }];
        let out = render(&entries);
        // Address column is left-padded to 16 hex digits, lowercase.
        assert!(
            out.starts_with("0000000000abcdef\t0000000000000010\tx\n"),
            "got {out:?}"
        );
    }

    #[test]
    fn demangle_user_main_with_macho_leading_underscore_handled_by_caller() {
        // The size-synthesis writer strips the Mach-O leading `_`
        // BEFORE calling demangle; demangle itself doesn't touch the
        // prefix. This test pins that contract: passing `_sigil_user_main`
        // directly into demangle yields `_sigil_user_main` (no strip),
        // not `main`.
        assert_eq!(
            demangle("_sigil_user_main"),
            "_sigil_user_main",
            "demangle must not strip the Mach-O `_` itself; that's the writer's job"
        );
    }

    #[test]
    fn render_emits_one_line_per_entry_with_trailing_newline() {
        let entries = vec![
            SymtabEntry {
                address: 1,
                size: 2,
                name: "a".into(),
            },
            SymtabEntry {
                address: 3,
                size: 4,
                name: "b".into(),
            },
        ];
        let out = render(&entries);
        assert_eq!(out.matches('\n').count(), 2);
        assert!(out.ends_with("\n"));
    }
}
