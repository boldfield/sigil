//! PC → function name resolution — plan 2026-05-08-sigil-v2-runtime-
//! profile-data Phase 5.
//!
//! Reads the `prog.symtab` sidecar (emitted by Phase 1's
//! `--emit-symbol-table`) and maps captured runtime PCs to demangled
//! function names. The format the loader expects is the wire format
//! the compiler writes:
//!
//! ```text
//! <text_offset_hex>\t<size_hex>\t<demangled_name>\n
//! ```
//!
//! The lookup is invoked from the Phase 5 writers (NOT from a signal
//! handler), so allocation, file I/O, and locking are all fine here.
//!
//! ## Address translation
//!
//! Symbols in the sidecar carry image-relative virtual addresses
//! (the binary's symbol-table values; the linker outputs PIE
//! executables for sigil, so VAs are relative to image base 0). At
//! runtime the loader applies an offset that we recover via
//! `dl_iterate_phdr` on Linux and `_dyld_get_image_vmaddr_slide(0)`
//! on macOS. Captured PCs subtract that offset to get image-relative
//! VAs, which we binary-search in the sorted sidecar.

use std::path::PathBuf;

#[derive(Debug, Clone)]
struct Sym {
    address: u64,
    size: u64,
    name: String,
}

/// PC resolver. Construct via [`Resolver::from_env_for_main_binary`]
/// or, in tests, with [`Resolver::from_entries`].
pub struct Resolver {
    /// Sorted ascending by `address`. Binary-searchable.
    entries: Vec<Sym>,
    /// Offset to subtract from runtime PCs before binary-search.
    /// Zero when the resolver was constructed in test mode without
    /// a real binary to inspect.
    image_base: u64,
}

impl Resolver {
    /// Construct an empty resolver. Every lookup falls back to the
    /// `0x<hex>` representation of the PC. Used when no symtab is
    /// available (no `--emit-symbol-table`) or when the sidecar
    /// cannot be opened.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            image_base: 0,
        }
    }

    /// Test-only constructor: build from a slice of (address, size,
    /// name) triples. Sorts internally; image_base = 0.
    pub fn from_entries<I: IntoIterator<Item = (u64, u64, String)>>(entries: I) -> Self {
        let mut entries: Vec<Sym> = entries
            .into_iter()
            .map(|(address, size, name)| Sym {
                address,
                size,
                name,
            })
            .collect();
        entries.sort_by_key(|s| s.address);
        Self {
            entries,
            image_base: 0,
        }
    }

    /// Production constructor: derives the symtab path from
    /// `/proc/self/exe` (Linux) or `_dyld_get_image_name(0)` (macOS),
    /// reads it, and discovers the image-load offset.
    ///
    /// If anything fails (no sidecar, parse error, dlinfo failure)
    /// the resolver falls back to [`Resolver::empty`] and every
    /// lookup returns `0x<hex>`.
    pub fn from_env_for_main_binary() -> Self {
        let binary_path = match current_exe_path() {
            Some(p) => p,
            None => return Self::empty(),
        };
        let mut symtab_path = binary_path.clone();
        let new_name = match binary_path.file_name() {
            Some(n) => {
                let mut s = n.to_os_string();
                s.push(".symtab");
                s
            }
            None => return Self::empty(),
        };
        symtab_path.set_file_name(new_name);

        let body = match std::fs::read_to_string(&symtab_path) {
            Ok(s) => s,
            Err(_) => return Self::empty(),
        };
        let entries = parse_symtab(&body);
        let image_base = main_image_base().unwrap_or(0);
        Self {
            entries,
            image_base,
        }
    }

    /// Look up a runtime PC. Returns the matching function name if
    /// the PC falls within any symbol's `[address, address + size)`;
    /// otherwise `0x<hex>`. Always returns an owned `String` — the
    /// writers (folded, pprof) need to hold multiple resolved names
    /// in flight at once.
    pub fn lookup(&self, pc: usize) -> String {
        // Strip aarch64 PAC bits one more time defensively — the
        // walker already does this, but a sample produced before
        // the strip-mask was added (or one delivered raw via a
        // foreign profile import) shouldn't regress.
        #[cfg(target_arch = "aarch64")]
        let pc = pc & 0x0000_FFFF_FFFF_FFFFusize;

        if self.entries.is_empty() {
            return format!("0x{:x}", pc);
        }
        let pc64 = (pc as u64).saturating_sub(self.image_base);
        // Binary-search for the symbol whose `[address, address + size)`
        // covers `pc64`.
        let idx = self.entries.partition_point(|s| s.address <= pc64);
        if idx == 0 {
            return format!("0x{:x}", pc);
        }
        let candidate = &self.entries[idx - 1];
        if pc64 >= candidate.address && pc64 < candidate.address + candidate.size {
            candidate.name.clone()
        } else {
            format!("0x{:x}", pc)
        }
    }
}

fn parse_symtab(body: &str) -> Vec<Sym> {
    let mut out: Vec<Sym> = Vec::new();
    for line in body.lines() {
        let mut parts = line.split('\t');
        let addr = match parts.next().and_then(|s| u64::from_str_radix(s, 16).ok()) {
            Some(a) => a,
            None => continue,
        };
        let size = match parts.next().and_then(|s| u64::from_str_radix(s, 16).ok()) {
            Some(s) => s,
            None => continue,
        };
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        out.push(Sym {
            address: addr,
            size,
            name,
        });
    }
    out.sort_by_key(|s| s.address);
    out
}

fn current_exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

#[cfg(target_os = "linux")]
fn main_image_base() -> Option<u64> {
    // Read /proc/self/maps and find the first executable mapping of
    // the main exe. That mapping's start address minus its file
    // offset gives the image base.
    let path = current_exe_path()?;
    let exe_name = path.canonicalize().ok()?;
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        // 564... -564... r-xp 00000000 fd:01 12345 /path/to/exe
        let mut fields = line.split_ascii_whitespace();
        let range = fields.next()?;
        let perms = fields.next()?;
        if !perms.contains('x') {
            continue;
        }
        let offset_hex = fields.next()?;
        let _dev = fields.next()?;
        let _inode = fields.next()?;
        let pathname = fields.next()?;
        if PathBuf::from(pathname).canonicalize().ok().as_ref() != Some(&exe_name) {
            continue;
        }
        let mut range_parts = range.split('-');
        let start = u64::from_str_radix(range_parts.next()?, 16).ok()?;
        let offset = u64::from_str_radix(offset_hex, 16).ok()?;
        return Some(start.saturating_sub(offset));
    }
    None
}

#[cfg(target_os = "macos")]
fn main_image_base() -> Option<u64> {
    extern "C" {
        fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
    }
    // SAFETY: image index 0 is the main binary on macOS; the
    // function is documented as safe to call from any thread.
    let slide = unsafe { _dyld_get_image_vmaddr_slide(0) };
    Some(slide as u64)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main_image_base() -> Option<u64> {
    None
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn empty_resolver_returns_hex_fallback() {
        let r = Resolver::empty();
        let name = r.lookup(0xDEADBEEF);
        assert_eq!(name, "0xdeadbeef");
    }

    #[test]
    fn resolver_finds_symbol_in_range() {
        let r = Resolver::from_entries(vec![
            (0x1000, 0x40, "foo".into()),
            (0x1040, 0x20, "bar".into()),
            (0x1100, 0x10, "baz".into()),
        ]);
        assert_eq!(r.lookup(0x1010), "foo");
        assert_eq!(r.lookup(0x103F), "foo");
        assert_eq!(r.lookup(0x1040), "bar");
        assert_eq!(r.lookup(0x105F), "bar");
        assert_eq!(r.lookup(0x1100), "baz");
    }

    #[test]
    fn resolver_falls_back_for_out_of_range_pcs() {
        let r = Resolver::from_entries(vec![(0x1000, 0x40, "foo".into())]);
        // Below the first symbol — fallback.
        assert_eq!(r.lookup(0x500), "0x500");
        // In the gap above foo's end — fallback.
        assert_eq!(r.lookup(0x1100), "0x1100");
    }

    #[test]
    fn parse_symtab_handles_well_formed_input() {
        let body = "\
0000000000001020\t0000000000000034\tsigil_alloc
0000000000001054\t0000000000000080\tmain
";
        let entries = parse_symtab(body);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sigil_alloc");
        assert_eq!(entries[1].name, "main");
        assert_eq!(entries[0].address, 0x1020);
        assert_eq!(entries[0].size, 0x34);
    }

    #[test]
    fn parse_symtab_skips_malformed_lines() {
        let body = "\
not-three-fields
0000000000001020\t0000000000000034\tsigil_alloc
\t\t
xx\t00\tbar
0000000000002000\tnothex\tbaz
";
        let entries = parse_symtab(body);
        assert_eq!(entries.len(), 1, "only the valid line should survive");
        assert_eq!(entries[0].name, "sigil_alloc");
    }
}
