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

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Invoke `cc` to link `obj_path` with the runtime and libgc, producing
/// an executable at `out_path`. Returns the command stdout/stderr as a
/// single string on failure for diagnostic display.
pub fn link(obj_path: &Path, out_path: &Path) -> Result<(), String> {
    let runtime = locate_runtime_lib()
        .ok_or_else(|| "libsigil_runtime.a not found; build the runtime first".to_string())?;

    // On macOS Homebrew installs libgc outside the default linker search
    // path. Query pkg-config for `-L` entries and pass them through before
    // `-lgc`. Graceful fallback: if pkg-config is missing or has no entry
    // for bdw-gc we proceed with the bare `-lgc`, which works on Ubuntu
    // where apt places libgc on the default path.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    let search_paths = pkg_config_search_paths("bdw-gc");
    let argv = build_link_argv(obj_path, out_path, &runtime, &search_paths);

    let mut cmd = Command::new("cc");
    cmd.args(&argv)
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

/// Build the `cc` argument vector for the link step.
///
/// Pure helper — no I/O and no process spawning — so the exact emitted argv
/// can be asserted in a unit test. The caller resolves the runtime archive
/// and the pkg-config `-L` search paths and passes them in. The order is
/// identical to the previous inline construction in `link()`:
///
///   <obj> <runtime> [-L<search>...] -lgc -lpthread -ldl -lm -o <out>
///   (Linux:) -Wl,--build-id=none -lgcc_s -rdynamic
///   (macOS:) -Wl,-reproducible
///
/// The `TZ`/`SOURCE_DATE_EPOCH` reproducibility env is applied by the caller
/// on the `Command`, not part of the argv.
fn build_link_argv(
    obj_path: &Path,
    out_path: &Path,
    runtime: &Path,
    search_paths: &[String],
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        obj_path.as_os_str().to_owned(),
        runtime.as_os_str().to_owned(),
    ];

    for search_path in search_paths {
        argv.push(OsString::from(format!("-L{search_path}")));
    }

    argv.extend([
        OsString::from("-lgc"),
        OsString::from("-lpthread"),
        OsString::from("-ldl"),
        OsString::from("-lm"),
        OsString::from("-o"),
        out_path.as_os_str().to_owned(),
    ]);

    #[cfg(target_os = "linux")]
    argv.extend([
        OsString::from("-Wl,--build-id=none"),
        // Rust staticlibs pull in panic_unwind -> _Unwind_* symbols; cc
        // does not autolink libgcc_s when driving ld directly for a
        // non-Rust object. Add it explicitly.
        OsString::from("-lgcc_s"),
        // Plan E2 Phase 1 Task 5 — `-rdynamic` (`-Wl,--export-dynamic`)
        // exports defined symbols into `.dynsym` so the runtime's
        // `dlsym(RTLD_DEFAULT, "sigil_user_main")` lookup can resolve
        // them at safepoint-cross-check time. Without it,
        // `dlsym(RTLD_DEFAULT, ...)` returns NULL for every emitted
        // function, the stackmap index has zero resolved records, and
        // the cross-check goes silently vacuous on Linux (PR #163
        // review M1). macOS doesn't need an equivalent — all global
        // symbols in Mach-O binaries are dlsym-able by default.
        OsString::from("-rdynamic"),
    ]);

    #[cfg(target_os = "macos")]
    argv.push(OsString::from("-Wl,-reproducible"));

    argv
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

#[allow(dead_code)]
fn locate_gc_lib() -> Option<PathBuf> {
    // Explicit override wins — release-archive consumers who unpack to
    // a non-standard prefix can `export SIGIL_GC_LIB=...` rather than
    // restructure their tree to match a built-in lookup path.
    // The override must be an absolute path that exists.
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
        let path = PathBuf::from(&p);
        if path.is_absolute() && path.exists() {
            return Some(path);
        }
    }

    let exe = std::env::current_exe().ok();
    locate_gc_lib_internal(exe.as_deref())
}

fn locate_gc_lib_internal(exe_path: Option<&Path>) -> Option<PathBuf> {
    // Release-archive layout: when libgc.a ships in a tarball like
    //
    //   sigil-<version>-<triple>/
    //     bin/sigil
    //     lib/libgc.a
    //
    // walking up one level from the executable's parent and into
    // `lib/` recovers the archive. Also try the flat-bundle layout
    // (`libgc.a` next to the binary).
    if let Some(exe) = exe_path {
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
                return Some(c);
            }
        }
    }

    // `cargo build` places libgc.a under target/<profile>/.
    // Walk a few candidate profile directories in preference order.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target").join(profile).join("libgc.a");
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
            let p = base.join("target").join(profile).join("libgc.a");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    /// The platform-specific suffix appended after `-o <out>`.
    fn platform_suffix() -> Vec<OsString> {
        #[cfg(target_os = "linux")]
        {
            vec![
                OsString::from("-Wl,--build-id=none"),
                OsString::from("-lgcc_s"),
                OsString::from("-rdynamic"),
            ]
        }
        #[cfg(target_os = "macos")]
        {
            vec![OsString::from("-Wl,-reproducible")]
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Vec::new()
        }
    }

    #[test]
    fn build_link_argv_default_emits_exact_vector() {
        let obj = Path::new("/tmp/prog.o");
        let out = Path::new("/tmp/prog");
        let runtime = Path::new("/build/target/release/libsigil_runtime.a");

        let argv = build_link_argv(obj, out, runtime, &[]);

        let mut expected: Vec<OsString> = vec![
            OsString::from("/tmp/prog.o"),
            OsString::from("/build/target/release/libsigil_runtime.a"),
            OsString::from("-lgc"),
            OsString::from("-lpthread"),
            OsString::from("-ldl"),
            OsString::from("-lm"),
            OsString::from("-o"),
            OsString::from("/tmp/prog"),
        ];
        expected.extend(platform_suffix());

        assert_eq!(argv, expected);
    }

    #[test]
    fn build_link_argv_inserts_search_paths_before_lgc() {
        let obj = Path::new("/tmp/prog.o");
        let out = Path::new("/tmp/prog");
        let runtime = Path::new("/build/target/release/libsigil_runtime.a");
        let search_paths = vec![
            "/opt/homebrew/lib".to_string(),
            "/usr/local/lib".to_string(),
        ];

        let argv = build_link_argv(obj, out, runtime, &search_paths);

        let mut expected: Vec<OsString> = vec![
            OsString::from("/tmp/prog.o"),
            OsString::from("/build/target/release/libsigil_runtime.a"),
            OsString::from("-L/opt/homebrew/lib"),
            OsString::from("-L/usr/local/lib"),
            OsString::from("-lgc"),
            OsString::from("-lpthread"),
            OsString::from("-ldl"),
            OsString::from("-lm"),
            OsString::from("-o"),
            OsString::from("/tmp/prog"),
        ];
        expected.extend(platform_suffix());

        assert_eq!(argv, expected);
    }

    #[test]
    fn test_locate_gc_lib_env_override_absolute_path() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let gc_path = tmpdir.join("libgc.a");
        std::fs::write(&gc_path, b"fake gc lib").unwrap();

        std::env::set_var("SIGIL_GC_LIB", &gc_path);
        let result = locate_gc_lib();
        std::env::remove_var("SIGIL_GC_LIB");
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result, Some(gc_path));
    }

    #[test]
    fn test_locate_gc_lib_env_override_relative_path_rejected() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        std::env::set_var("SIGIL_GC_LIB", "relative/path/libgc.a");
        let result = locate_gc_lib();
        std::env::remove_var("SIGIL_GC_LIB");

        assert_eq!(result, None);
    }

    #[test]
    fn test_locate_gc_lib_env_override_nonexistent_rejected() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        std::env::set_var("SIGIL_GC_LIB", "/nonexistent/path/libgc.a");
        let result = locate_gc_lib();
        std::env::remove_var("SIGIL_GC_LIB");

        assert_eq!(result, None);
    }

    #[test]
    fn test_locate_gc_lib_cargo_target_release() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_rel_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        let target_release = tmpdir.join("target").join("release");
        std::fs::create_dir_all(&target_release).unwrap();
        let gc_path = target_release.join("libgc.a");
        std::fs::write(&gc_path, b"fake gc lib").unwrap();

        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmpdir).unwrap();
        let result = locate_gc_lib();
        let result_abs = result.and_then(|p| p.canonicalize().ok());
        let gc_path_abs = gc_path.canonicalize().ok();
        std::env::set_current_dir(&orig_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result_abs, gc_path_abs);
    }

    #[test]
    fn test_locate_gc_lib_cargo_target_debug() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_dbg_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        let target_debug = tmpdir.join("target").join("debug");
        std::fs::create_dir_all(&target_debug).unwrap();
        let gc_path = target_debug.join("libgc.a");
        std::fs::write(&gc_path, b"fake gc lib").unwrap();

        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmpdir).unwrap();
        let result = locate_gc_lib();
        let result_abs = result.and_then(|p| p.canonicalize().ok());
        let gc_path_abs = gc_path.canonicalize().ok();
        std::env::set_current_dir(&orig_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result_abs, gc_path_abs);
    }

    #[test]
    fn test_locate_gc_lib_none_when_not_found() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_empty_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();

        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmpdir).unwrap();
        let result = locate_gc_lib();
        std::env::set_current_dir(&orig_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result, None);
    }

    #[test]
    fn test_locate_gc_lib_release_archive_layout() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_rel_arch_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        let lib_dir = tmpdir.join("lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let gc_path = lib_dir.join("libgc.a");
        std::fs::write(&gc_path, b"fake gc lib").unwrap();

        // Simulate exe_path being in bin/ directory
        let exe_path = tmpdir.join("bin").join("sigil");
        let result = locate_gc_lib_internal(Some(&exe_path));
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result, Some(gc_path));
    }

    #[test]
    fn test_locate_gc_lib_flat_layout() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let tmpdir = std::env::temp_dir().join(format!("sigil_test_flat_{}", pid));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let gc_path = tmpdir.join("libgc.a");
        std::fs::write(&gc_path, b"fake gc lib").unwrap();

        // Simulate exe_path being in the same directory as libgc.a
        let exe_path = tmpdir.join("sigil");
        let result = locate_gc_lib_internal(Some(&exe_path));
        let _ = std::fs::remove_dir_all(&tmpdir);

        assert_eq!(result, Some(gc_path));
    }
}
