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
//!
//! When a static `libgc.a` is available (located via `SIGIL_GC_LIB` env
//! var or the standard search paths), it is passed by absolute path after
//! `libsigil_runtime.a` and `-lgc` is omitted. When no static archive is
//! found, the original dynamic `-lgc` (+ macOS pkg-config `-L`) behavior
//! is preserved exactly.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Invoke `cc` to link `obj_path` with the runtime and libgc, producing
/// an executable at `out_path`. Returns the command stdout/stderr as a
/// single string on failure for diagnostic display.
pub fn link(obj_path: &Path, out_path: &Path) -> Result<(), String> {
    let runtime = locate_runtime_lib()
        .ok_or_else(|| "libsigil_runtime.a not found; build the runtime first".to_string())?;
    let gc_lib = locate_gc_lib();

    let mut cmd = build_cc_command(obj_path, &runtime, gc_lib.as_deref());
    cmd.arg("-o")
        .arg(out_path)
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", "0");

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

/// Build the `cc` invocation for linking. Separated from `link()` so
/// tests can inspect the argv without running the linker.
///
/// `gc_lib` is the located static `libgc.a` archive (absolute path), or
/// `None` to fall back to the dynamic `-lgc` + pkg-config `-L` behavior.
fn build_cc_command(obj_path: &Path, runtime: &Path, gc_lib: Option<&Path>) -> Command {
    let mut cmd = Command::new("cc");
    cmd.arg(obj_path).arg(runtime);

    match gc_lib {
        Some(gc_path) => {
            // Static archive: pass by the path locate_gc_lib() returned
            // (canonicalized to absolute), -lgc must not be emitted.
            cmd.arg(gc_path);
        }
        None => {
            // Dynamic fallback: query pkg-config for macOS Homebrew -L
            // paths, then emit -lgc. On Ubuntu libgc is on the default
            // search path so pkg-config returns nothing and -lgc suffices.
            for search_path in pkg_config_search_paths("bdw-gc") {
                cmd.arg(format!("-L{search_path}"));
            }
            cmd.arg("-lgc");
        }
    }

    cmd.arg("-lpthread").arg("-ldl").arg("-lm");

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

/// Locate a static `libgc.a` to link against, returning its canonicalized
/// absolute path. Search order mirrors `locate_runtime_lib()`:
///
/// 1. `SIGIL_GC_LIB` env var (absolute path to a custom `libgc.a`).
/// 2. Release-archive layout: `bin/sigil` → `../lib/libgc.a`.
/// 3. Flat layout: `libgc.a` beside the `sigil` binary.
/// 4. `target/{release,debug}/libgc.a` (emitted by `build-static-boehm.sh`).
///
/// Returns `None` when no static archive is found; `link()` then falls
/// back to the dynamic `-lgc` path.
fn locate_gc_lib() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path.canonicalize().unwrap_or(path));
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        let candidates = [
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libgc.a")),
            exe_dir.as_ref().map(|d| d.join("libgc.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(c.canonicalize().unwrap_or(c));
            }
        }
    }

    for profile in &["release", "debug"] {
        let p = PathBuf::from("target").join(profile).join("libgc.a");
        if p.exists() {
            return Some(p.canonicalize().unwrap_or(p));
        }
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        for profile in &["release", "debug"] {
            let p = base.join("target").join(profile).join("libgc.a");
            if p.exists() {
                return Some(p.canonicalize().unwrap_or(p));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When locate_gc_lib() returns Some(path), build_cc_command must
    /// include that path in the cc argv and must NOT emit -lgc.
    #[test]
    fn build_cc_command_includes_static_libgc_when_found() {
        // Use a fixed absolute path — build_cc_command passes whatever
        // path it receives straight through, so the file need not exist.
        let gc_path = Path::new("/nonexistent/libgc.a");
        let cmd = build_cc_command(
            Path::new("foo.o"),
            Path::new("libsigil_runtime.a"),
            Some(gc_path),
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            args.iter().any(|a| a == "/nonexistent/libgc.a"),
            "static libgc.a path must appear in cc argv; got: {args:?}"
        );
        assert!(
            !args.contains(&"-lgc".to_string()),
            "-lgc must not appear in cc argv when static archive is used; got: {args:?}"
        );
    }

    /// When locate_gc_lib() returns None, build_cc_command must emit -lgc
    /// for the dynamic fallback path.
    #[test]
    fn build_cc_command_dynamic_fallback_when_no_static_libgc() {
        let cmd = build_cc_command(Path::new("foo.o"), Path::new("libsigil_runtime.a"), None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            args.contains(&"-lgc".to_string()),
            "-lgc must appear in cc argv when no static archive is available; got: {args:?}"
        );
    }
}
