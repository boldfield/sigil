//! Linker driver — plan A1 Stage 1 task 13.
//!
//! Takes the emitted object file plus the runtime staticlib plus Boehm GC
//! and produces an executable via the host `cc`. Reproducibility flags are
//! enumerated per-host:
//!
//! - Linux: `-Wl,--build-id=none`, `SOURCE_DATE_EPOCH=0` in env.
//! - macOS: `-Wl,-reproducible` (see PLAN_A1_DEVIATIONS.md [Task 13] —
//!   dyld rejects binaries without LC_UUID; `-reproducible` yields a
//!   stable content-hash UUID instead of omitting it).
//! - Both: `TZ=UTC` in the link env.
//!
//! The runtime library is located by looking for `libsigil_runtime.a` in
//! the same `target/<profile>/` directory that built the compiler binary.
//! That keeps the setup simple for development builds and for the e2e
//! test which runs `cargo run` before invoking the compiler.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Invoke `cc` to link `obj_path` with the runtime and libgc, producing
/// an executable at `out_path`. Returns the command stdout/stderr as a
/// single string on failure for diagnostic display.
pub fn link(obj_path: &Path, out_path: &Path) -> Result<(), String> {
    let runtime = locate_runtime_lib()
        .ok_or_else(|| "libsigil_runtime.a not found; build the runtime first".to_string())?;

    // Prefer a statically-linkable libgc.a when one is available so the
    // compiled binary carries its own GC and does not depend on a system
    // libgc being installed at runtime. When none is found, fall back to
    // the historical dynamic `-lgc` behavior unchanged.
    let gc_lib = locate_gc_lib();

    let mut cmd = build_cc_command(obj_path, &runtime, gc_lib.as_deref(), out_path);

    let output = cmd
        .output()
        .map_err(|e| format!("cc invocation failed: {e}"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cc exited {}: stdout={stdout} stderr={stderr}",
            output.status
        ));
    }
    Ok(())
}

/// Build the `cc` invocation that links `obj_path` and the runtime
/// staticlib into an executable at `out_path`.
///
/// When `gc_lib` is `Some(path)` a statically-linkable `libgc.a` was
/// located: the archive is passed by (absolute) path immediately after
/// `libsigil_runtime.a` and before the system `-l` libraries, and no
/// `-lgc` (nor the macOS pkg-config `-L` search paths, which only serve
/// the dynamic fallback) is emitted. When `gc_lib` is `None` the
/// historical dynamic behavior is preserved exactly: macOS pkg-config
/// `-L` search paths followed by `-lgc`.
fn build_cc_command(
    obj_path: &Path,
    runtime: &Path,
    gc_lib: Option<&Path>,
    out_path: &Path,
) -> Command {
    let mut cmd = Command::new("cc");
    cmd.arg(obj_path).arg(runtime);

    match gc_lib {
        // Static Boehm located: link the archive directly, no -lgc.
        Some(gc) => {
            cmd.arg(gc);
        }
        // No static archive: dynamic fallback, unchanged.
        //
        // On macOS Homebrew installs libgc outside the default linker
        // search path. Query pkg-config for `-L` entries and pass them
        // through before `-lgc`. Graceful fallback: if pkg-config is
        // missing or has no entry for bdw-gc we proceed with the bare
        // `-lgc`, which works on Ubuntu where apt places libgc on the
        // default path.
        // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
        None => {
            for search_path in pkg_config_search_paths("bdw-gc") {
                cmd.arg(format!("-L{search_path}"));
            }
            cmd.arg("-lgc");
        }
    }

    cmd.arg("-lpthread")
        .arg("-ldl")
        .arg("-lm")
        .arg("-o")
        .arg(out_path)
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", "0");

    #[cfg(target_os = "linux")]
    {
        cmd.arg("-Wl,--build-id=none");
        // Rust staticlibs pull in panic_unwind -> _Unwind_* symbols; cc
        // does not autolink libgcc_s when driving ld directly for a
        // non-Rust object. Add it explicitly.
        cmd.arg("-lgcc_s");
        // Plan E2 Phase 1 Task 5 — `-rdynamic` (`-Wl,--export-dynamic`)
        // exports defined symbols into `.dynsym` so the runtime's
        // `dlsym(RTLD_DEFAULT, "sigil_user_main")` lookup can resolve
        // them at safepoint-cross-check time. Without it,
        // `dlsym(RTLD_DEFAULT, ...)` returns NULL for every emitted
        // function, the stackmap index has zero resolved records, and
        // the cross-check goes silently vacuous on Linux (PR #163
        // review M1). macOS doesn't need an equivalent — all global
        // symbols in Mach-O binaries are dlsym-able by default.
        cmd.arg("-rdynamic");
    }

    #[cfg(target_os = "macos")]
    cmd.arg("-Wl,-reproducible");

    cmd
}

fn pkg_config_search_paths(pkg: &str) -> Vec<String> {
    let output = match Command::new("pkg-config").args(["--libs", pkg]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_ascii_whitespace()
        .filter_map(|token| token.strip_prefix("-L").map(str::to_owned))
        .filter(|path| !path.is_empty())
        .collect()
}

fn locate_runtime_lib() -> Option<PathBuf> {
    // Explicit override wins — release-archive consumers who unpack to
    // a non-standard prefix can `export SIGIL_RUNTIME_LIB=...` rather
    // than restructure their tree to match a built-in lookup path.
    if let Ok(p) = std::env::var("SIGIL_RUNTIME_LIB") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

    // Release-archive layout: when the `sigil` binary ships in a
    // tarball like
    //
    //   sigil-<version>-<triple>/
    //     bin/sigil
    //     lib/libsigil_runtime.a
    //     std/...
    //
    // walking up one level from the executable's parent and into
    // `lib/` recovers the staticlib. Also try the flat-bundle layout
    // (`libsigil_runtime.a` next to the binary).
    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        let candidates = [
            // bin/sigil → ../lib/libsigil_runtime.a
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libsigil_runtime.a")),
            // flat: sigil + libsigil_runtime.a in the same dir
            exe_dir.as_ref().map(|d| d.join("libsigil_runtime.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(c);
            }
        }
    }

    // `cargo build` places libsigil_runtime.a under target/<profile>/.
    // Walk a few candidate profile directories in preference order.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target")
            .join(profile)
            .join("libsigil_runtime.a");
        if p.exists() {
            return Some(p);
        }
    }
    // CARGO_MANIFEST_DIR fallback if invoked from a subdir.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        for profile in &["release", "debug"] {
            let p = base.join("target").join(profile).join("libsigil_runtime.a");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Locate a statically-linkable `libgc.a`, mirroring
/// [`locate_runtime_lib`]. Returns the archive as an absolute path
/// (canonicalized) so `cc` links it directly rather than searching for
/// a `-lgc`. Returns `None` when no static archive is found, in which
/// case the caller keeps the dynamic `-lgc` fallback.
///
/// Lookup order:
///   1. `SIGIL_GC_LIB` env override (absolute path to a `libgc.a`).
///   2. Release-archive layout: `bin/sigil` → `../lib/libgc.a`.
///   3. Flat layout: `libgc.a` next to the binary.
///   4. Cargo tree: `target/{release,debug}/libgc.a` (and the same
///      under `CARGO_MANIFEST_DIR/..` when invoked from a subdir).
fn locate_gc_lib() -> Option<PathBuf> {
    // Explicit override wins — a prebuilt static Boehm archive (e.g.
    // produced by scripts/build-static-boehm.sh) at an arbitrary path.
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(absolute(path));
        }
    }

    // Release-archive layout (bin/sigil → ../lib/libgc.a) and flat-bundle
    // layout (libgc.a next to the binary), mirroring locate_runtime_lib.
    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        let candidates = [
            // bin/sigil → ../lib/libgc.a
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libgc.a")),
            // flat: sigil + libgc.a in the same dir
            exe_dir.as_ref().map(|d| d.join("libgc.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(absolute(c));
            }
        }
    }

    // `cargo build` may stage libgc.a under target/<profile>/.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target").join(profile).join("libgc.a");
        if p.exists() {
            return Some(absolute(p));
        }
    }
    // CARGO_MANIFEST_DIR fallback if invoked from a subdir.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        for profile in &["release", "debug"] {
            let p = base.join("target").join(profile).join("libgc.a");
            if p.exists() {
                return Some(absolute(p));
            }
        }
    }
    None
}

/// Resolve `p` to an absolute path, falling back to the original on any
/// canonicalization error (e.g. permission quirks) so callers always
/// receive the best path available.
fn absolute(p: PathBuf) -> PathBuf {
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// When a static libgc.a is located, the emitted cc argv must
    /// contain the archive path and must NOT contain `-lgc`. Driven
    /// directly with an explicit `Some(path)` so the assertion does not
    /// depend on any ambient `target/` filesystem state.
    #[test]
    fn build_cc_command_static_libgc_uses_archive_and_no_lgc() {
        let gc = PathBuf::from("/abs/path/to/libgc.a");
        let cmd = build_cc_command(
            Path::new("foo.o"),
            Path::new("libsigil_runtime.a"),
            Some(gc.as_path()),
            Path::new("out"),
        );
        let args = argv(&cmd);
        assert!(
            args.iter().any(|a| a == "/abs/path/to/libgc.a"),
            "static archive path must be present in cc argv: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "-lgc"),
            "-lgc must be absent when a static libgc.a is located: {args:?}"
        );
    }

    /// When no static libgc.a is located, the cc argv must preserve the
    /// dynamic fallback and emit `-lgc`. Driven directly with `None` so
    /// it does not consult the ambient `target/` tree.
    #[test]
    fn build_cc_command_dynamic_fallback_emits_lgc() {
        let cmd = build_cc_command(
            Path::new("foo.o"),
            Path::new("libsigil_runtime.a"),
            None,
            Path::new("out"),
        );
        let args = argv(&cmd);
        assert!(
            args.iter().any(|a| a == "-lgc"),
            "-lgc must be present in the dynamic fallback: {args:?}"
        );
    }
}
