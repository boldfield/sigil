//! PC → function name resolution — plan 2026-05-08-sigil-v2-runtime-
//! profile-data Phase 5, extended by plan 2026-05-11-sigil-v2-profile-
//! dyld-symbolization for dyld-loaded library coverage.
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
//!
//! ## Dyld-loaded library coverage
//!
//! The sidecar only covers the main executable. PCs that land inside
//! dyld-loaded libraries (libgc, libSystem, libc, ...) miss the
//! sidecar and fall through to a `0x<hex>` rendering — which is the
//! analysis-visibility bug plan 2026-05-11 fixes. Enabling
//! [`Resolver::with_dyld_images`] turns on a `dladdr(3)` fallback so
//! that on a sidecar miss the resolver consults the dynamic linker's
//! loaded-image symbol tables.
//!
//! **Deviation from plan body.** The plan asks us to walk each
//! dyld-loaded image's symbol table with `object::read::File` and
//! append entries to the resolver's `Vec<Sym>`. That requires adding
//! the `object` crate to the runtime — which the plan's hard rule
//! "No new dependencies" forbids. `dladdr` is the standard POSIX
//! per-PC resolver, already linked in via `-ldl` on Linux and
//! libSystem on macOS (see `compiler/src/link.rs`'s linker line), and
//! its NULL-on-miss semantics give us the "stripped dylib silently
//! skipped" behavior plan Task 3 calls for. We get the plan's outcome
//! (libgc PCs resolve to `GC_*` names) without the dep.

use std::path::PathBuf;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::profile::sys;

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
    /// When `true`, [`Resolver::lookup`] falls through to `dladdr(3)`
    /// on a sidecar miss. Enabled via [`Resolver::with_dyld_images`].
    /// Off by default so test-mode resolvers behave deterministically
    /// (no dependence on the test binary's loaded-image state).
    dyld_fallback: bool,
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
            dyld_fallback: false,
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
            dyld_fallback: false,
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
            dyld_fallback: false,
        }
    }

    /// Enable a `dladdr(3)` fallback for PCs that miss the main-binary
    /// sidecar. Plan 2026-05-11 surface: PCs landing in libgc /
    /// libSystem / future dyld-loaded libs resolve to their POSIX
    /// linker-table names instead of falling through to `0x<hex>`.
    ///
    /// **Zero overhead when profiling is off.** This method only flips
    /// a bool; it does no enumeration, allocation, or syscall. The
    /// `dladdr` call only happens at lookup time, which only happens
    /// at flush time, which only happens when `SIGIL_CPU_PROFILE` or
    /// `SIGIL_ALLOC_PROFILE` is set.
    ///
    /// **Stripped images.** `dladdr` returns `dli_sname == NULL` for
    /// addresses inside dylibs with no nearby exported symbol (Apple
    /// ships stripped libSystem to consumers). Those PCs fall through
    /// to the existing `0x<hex>` rendering — same UX as today, per
    /// plan Task 3.
    pub fn with_dyld_images(mut self) -> Self {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.dyld_fallback = true;
        }
        self
    }

    /// Look up a runtime PC. Returns the matching function name if
    /// the PC falls within any symbol's `[address, address + size)`;
    /// otherwise consults `dladdr(3)` (when enabled via
    /// [`Resolver::with_dyld_images`]); otherwise returns `0x<hex>`.
    /// Always returns an owned `String` — the writers (folded, pprof)
    /// need to hold multiple resolved names in flight at once.
    pub fn lookup(&self, pc: usize) -> String {
        // Strip aarch64 PAC bits one more time defensively — the
        // walker already does this, but a sample produced before
        // the strip-mask was added (or one delivered raw via a
        // foreign profile import) shouldn't regress.
        #[cfg(target_arch = "aarch64")]
        let pc = pc & 0x0000_FFFF_FFFF_FFFFusize;

        if !self.entries.is_empty() {
            let pc64 = (pc as u64).saturating_sub(self.image_base);
            // Binary-search for the symbol whose `[address, address + size)`
            // covers `pc64`.
            let idx = self.entries.partition_point(|s| s.address <= pc64);
            if idx > 0 {
                let candidate = &self.entries[idx - 1];
                if pc64 >= candidate.address && pc64 < candidate.address + candidate.size {
                    return candidate.name.clone();
                }
            }
        }

        // Sidecar miss: optionally fall through to dladdr for
        // dyld-loaded library PCs (libgc, libSystem, ...).
        if self.dyld_fallback {
            if let Some(name) = dladdr_lookup(pc) {
                return name;
            }
        }

        format!("0x{:x}", pc)
    }
}

/// Resolve `pc` via `dladdr(3)`. Returns `Some(name)` on success
/// (name is a heap-owned string copied out of the loader-owned
/// `dli_sname` storage); `None` when the address isn't inside any
/// loaded image or the resolved symbol is anonymous.
///
/// Non-Linux / non-macOS hosts compile this as a no-op returning
/// `None` — the `sys::dladdr` binding is only compiled on those two
/// targets.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn dladdr_lookup(pc: usize) -> Option<String> {
    let mut info = sys::DlInfo {
        dli_fname: core::ptr::null(),
        dli_fbase: core::ptr::null_mut(),
        dli_sname: core::ptr::null(),
        dli_saddr: core::ptr::null_mut(),
    };
    // SAFETY: `&mut info` is a valid pointer to a stack-owned
    // `DlInfo`; `pc as *const c_void` is a numerical address treated
    // as an opaque pointer by `dladdr`, which doesn't dereference it.
    let rc = unsafe { sys::dladdr(pc as *const core::ffi::c_void, &mut info as *mut _) };
    if rc == 0 || info.dli_sname.is_null() {
        return None;
    }
    // SAFETY: dladdr success + non-null dli_sname guarantees a
    // NUL-terminated C string owned by the dynamic linker. We copy
    // the bytes into an owned String so the result outlives any
    // subsequent dladdr calls / dlclose.
    let cstr = unsafe { core::ffi::CStr::from_ptr(info.dli_sname) };
    Some(cstr.to_string_lossy().into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn dladdr_lookup(_pc: usize) -> Option<String> {
    None
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

    /// Plan 2026-05-11 Task 2 — `with_dyld_images()` enables a
    /// `dladdr`-backed fallback so PCs that miss the main-binary
    /// sidecar resolve to dyld-loaded library symbols (the libgc-
    /// resolution surface the plan exists to fix).
    ///
    /// We can't reach a real libgc PC from a runtime unit test
    /// without compiling and running a sigil program, so the test
    /// uses a libc function pointer (`malloc` family — the test
    /// binary always links libc dynamically) as a stand-in. Any
    /// address inside any loaded shared object exercises the same
    /// code path the libgc surface does.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn with_dyld_images_resolves_libc_function_pointer() {
        extern "C" {
            fn getpid() -> i32;
        }
        // Function pointers in Rust are opaque; cast through usize
        // to get the runtime address bytes dladdr will resolve.
        let pc = getpid as *const () as usize;
        let r = Resolver::empty().with_dyld_images();
        let name = r.lookup(pc);
        assert!(
            name.contains("getpid"),
            "with_dyld_images() should resolve a libc function pointer to its symbol name; got {name:?}"
        );
    }

    /// Without `with_dyld_images()`, an empty resolver returns the
    /// `0x<hex>` fallback even for PCs that dladdr could resolve.
    /// Pins the opt-in contract — production code paths that want
    /// dyld coverage must explicitly chain `.with_dyld_images()`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn empty_resolver_without_dyld_fallback_returns_hex() {
        extern "C" {
            fn getpid() -> i32;
        }
        let pc = getpid as *const () as usize;
        let r = Resolver::empty();
        let name = r.lookup(pc);
        assert!(
            name.starts_with("0x"),
            "without with_dyld_images() the empty resolver must fall through to hex; got {name:?}"
        );
    }

    /// Plan 2026-05-11 Task 3 — stripped dylibs (or any address that
    /// doesn't land inside a known image) silently fall through to
    /// the `0x<hex>` rendering. We probe a clearly-unmapped userspace
    /// address (`0xdeadbeef`) to verify dladdr's NULL-on-miss surfaces
    /// as the hex fallback rather than a spurious symbol.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn dyld_fallback_returns_hex_for_unmapped_address() {
        let r = Resolver::empty().with_dyld_images();
        let name = r.lookup(0xDEADBEEF);
        assert!(
            name.starts_with("0x"),
            "unmapped PC must surface as `0x<hex>` not a spurious symbol; got {name:?}"
        );
    }

    /// Sidecar entries take precedence over dladdr — the main-binary
    /// sidecar's address ranges are authoritative because they cover
    /// the demangled sigil-user names (`main`, `foo$$Int`) which
    /// dladdr would otherwise return in their mangled form
    /// (`sigil_user_main`, `sigil_user_foo____Int`).
    #[test]
    fn sidecar_takes_precedence_over_dladdr_fallback() {
        let r = Resolver::from_entries(vec![(0x1000, 0x40, "main".into())]).with_dyld_images();
        // PC inside the sidecar entry → sidecar wins.
        assert_eq!(r.lookup(0x1010), "main");
    }
}
