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

    let mut cmd = Command::new("cc");
    cmd.arg(obj_path).arg(&runtime);

    // On macOS Homebrew installs libgc outside the default linker search
    // path. Query pkg-config for `-L` entries and pass them through before
    // `-lgc`. Graceful fallback: if pkg-config is missing or has no entry
    // for bdw-gc we proceed with the bare `-lgc`, which works on Ubuntu
    // where apt places libgc on the default path.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    for search_path in pkg_config_search_paths("bdw-gc") {
        cmd.arg(format!("-L{search_path}"));
    }

    cmd.arg("-lgc")
        .arg("-lpthread")
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
    locate_gc_lib_internal(None)
}

fn locate_gc_lib_internal(exe_dir_override: Option<PathBuf>) -> Option<PathBuf> {
    // Explicit override wins — must be an absolute path and must exist.
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
        let path = PathBuf::from(p);
        if path.is_absolute() && path.exists() {
            return Some(path);
        }
    }

    // Release-archive layout: libgc.a in ../lib/ relative to binary,
    // or flat layout (libgc.a beside the binary).
    let exe_dir = exe_dir_override.or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from))
    });

    if let Some(exe_dir) = &exe_dir {
        let candidates = [
            // bin/sigil → ../lib/libgc.a
            exe_dir.parent().map(|p| p.join("lib").join("libgc.a")),
            // flat: sigil + libgc.a in the same dir
            Some(exe_dir.join("libgc.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(c);
            }
        }
    }

    // cargo build places libgc.a under target/<profile>/.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target").join(profile).join("libgc.a");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_locate_gc_lib_env_override_absolute_path_wins() {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let gc_lib = temp_dir.path().join("libgc.a");
        fs::write(&gc_lib, b"mock").expect("failed to write test file");

        let _guard = EnvGuard::set("SIGIL_GC_LIB", gc_lib.to_str().expect("invalid path"));
        assert_eq!(locate_gc_lib(), Some(gc_lib));
    }

    #[test]
    fn test_locate_gc_lib_env_override_rejects_relative_path() {
        let _guard = EnvGuard::set("SIGIL_GC_LIB", "relative/path/libgc.a");
        assert_eq!(locate_gc_lib(), None);
    }

    #[test]
    fn test_locate_gc_lib_env_override_rejects_nonexistent_absolute() {
        let _guard = EnvGuard::set("SIGIL_GC_LIB", "/nonexistent/absolute/path/libgc.a");
        assert_eq!(locate_gc_lib(), None);
    }

    #[test]
    fn test_locate_gc_lib_release_archive_layout() {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        // Simulate exe at bin/sigil, libgc.a at lib/libgc.a
        let bin_dir = temp_dir.path().join("bin");
        let lib_dir = temp_dir.path().join("lib");
        fs::create_dir_all(&bin_dir).expect("failed to create bin dir");
        fs::create_dir_all(&lib_dir).expect("failed to create lib dir");
        let gc_lib = lib_dir.join("libgc.a");
        fs::write(&gc_lib, b"mock").expect("failed to write test file");

        let _env_guard = EnvGuard::clear("SIGIL_GC_LIB");
        let exe_dir = bin_dir.clone();
        let result = locate_gc_lib_internal(Some(exe_dir));
        // Result is relative from the current directory perspective
        // but the function should find ../lib/libgc.a relative to bin/
        assert!(result.is_some() && result.unwrap().ends_with("libgc.a"));
    }

    #[test]
    fn test_locate_gc_lib_flat_layout() {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        // Simulate exe at root, libgc.a beside it
        let gc_lib = temp_dir.path().join("libgc.a");
        fs::write(&gc_lib, b"mock").expect("failed to write test file");

        let _env_guard = EnvGuard::clear("SIGIL_GC_LIB");
        let exe_dir = temp_dir.path().to_path_buf();
        let result = locate_gc_lib_internal(Some(exe_dir));
        assert!(result.is_some() && result.unwrap().ends_with("libgc.a"));
    }

    #[test]
    fn test_locate_gc_lib_cargo_target_release() {
        let _lock = GLOBAL_STATE_LOCK.lock();
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let temp_path = temp_dir.path().join("release_test");
        let target = temp_path.join("target").join("release");
        fs::create_dir_all(&target).expect("failed to create target dir");
        let gc_lib = target.join("libgc.a");
        fs::write(&gc_lib, b"mock").expect("failed to write test file");

        let old_cwd = std::env::current_dir().expect("failed to get current dir");
        std::env::set_current_dir(&temp_path).expect("failed to change dir");
        let _cwd_guard = CwdGuard::new(old_cwd);
        let _env_guard = EnvGuard::clear("SIGIL_GC_LIB");

        let result = locate_gc_lib_internal(None);
        assert_eq!(result, Some(PathBuf::from("target/release/libgc.a")));
    }

    #[test]
    fn test_locate_gc_lib_cargo_target_debug() {
        let _lock = GLOBAL_STATE_LOCK.lock();
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let temp_path = temp_dir.path().join("debug_test");
        let target = temp_path.join("target").join("debug");
        fs::create_dir_all(&target).expect("failed to create target dir");
        let gc_lib = target.join("libgc.a");
        fs::write(&gc_lib, b"mock").expect("failed to write test file");

        let old_cwd = std::env::current_dir().expect("failed to get current dir");
        std::env::set_current_dir(&temp_path).expect("failed to change dir");
        let _cwd_guard = CwdGuard::new(old_cwd);
        let _env_guard = EnvGuard::clear("SIGIL_GC_LIB");

        let result = locate_gc_lib_internal(None);
        assert_eq!(result, Some(PathBuf::from("target/debug/libgc.a")));
    }

    #[test]
    fn test_locate_gc_lib_none_when_not_found() {
        let _lock = GLOBAL_STATE_LOCK.lock();
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let temp_path = temp_dir.path().to_path_buf();
        let old_cwd = std::env::current_dir().expect("failed to get current dir");

        std::env::set_current_dir(&temp_path).expect("failed to change dir");
        let _cwd_guard = CwdGuard::new(old_cwd);
        let _guard = EnvGuard::clear("SIGIL_GC_LIB");

        let result = locate_gc_lib_internal(None);
        assert_eq!(result, None);
    }

    /// Guard that restores current directory on drop
    struct CwdGuard {
        old_cwd: PathBuf,
    }

    impl CwdGuard {
        fn new(old_cwd: PathBuf) -> Self {
            CwdGuard { old_cwd }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.old_cwd);
        }
    }

    /// Guard that restores environment variable on drop
    struct EnvGuard {
        key: String,
        old_value: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::set_var(key, value);
            EnvGuard {
                key: key.to_string(),
                old_value,
            }
        }

        fn clear(key: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::remove_var(key);
            EnvGuard {
                key: key.to_string(),
                old_value,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old_value {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}
